//! Deterministic BPS creation and complete, checked BPS application.
//!
//! The minimal codec is reused from the project-owned PC-98 Madou sibling.
//! The generator emits SourceRead and TargetRead actions; the reader accepts
//! all four standard BPS action kinds.

use anyhow::{Result, bail, ensure};

const BPS_MAGIC: &[u8; 4] = b"BPS1";

fn encode_vli(output: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            output.push(byte | 0x80);
            return;
        }
        output.push(byte);
        value -= 1;
    }
}

fn decode_vli(input: &[u8]) -> Result<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 1u64;
    for (index, byte) in input.iter().copied().enumerate() {
        let component = u64::from(byte & 0x7f)
            .checked_mul(shift)
            .ok_or_else(|| anyhow::anyhow!("BPS VLI overflow"))?;
        value = value
            .checked_add(component)
            .ok_or_else(|| anyhow::anyhow!("BPS VLI overflow"))?;
        if byte & 0x80 != 0 {
            return Ok((value, index + 1));
        }
        shift = shift
            .checked_shl(7)
            .ok_or_else(|| anyhow::anyhow!("BPS VLI overflow"))?;
        value = value
            .checked_add(shift)
            .ok_or_else(|| anyhow::anyhow!("BPS VLI overflow"))?;
    }
    bail!("unexpected end of BPS VLI data")
}

enum Action<'a> {
    SourceRead(usize),
    TargetRead(&'a [u8]),
}

fn actions<'a>(source: &[u8], target: &'a [u8]) -> Vec<Action<'a>> {
    let mut actions = Vec::new();
    let mut offset = 0usize;
    while offset < target.len() {
        if offset < source.len() && source[offset] == target[offset] {
            let start = offset;
            while offset < target.len() && offset < source.len() && source[offset] == target[offset]
            {
                offset += 1;
            }
            actions.push(Action::SourceRead(offset - start));
        } else {
            let start = offset;
            while offset < target.len()
                && (offset >= source.len() || source[offset] != target[offset])
            {
                offset += 1;
            }
            actions.push(Action::TargetRead(&target[start..offset]));
        }
    }
    actions
}

fn generate(source: &[u8], target: &[u8]) -> Vec<u8> {
    let mut patch = Vec::new();
    patch.extend_from_slice(BPS_MAGIC);
    encode_vli(&mut patch, source.len() as u64);
    encode_vli(&mut patch, target.len() as u64);
    encode_vli(&mut patch, 0);
    for action in actions(source, target) {
        match action {
            Action::SourceRead(length) => encode_vli(&mut patch, (length as u64 - 1) << 2),
            Action::TargetRead(bytes) => {
                encode_vli(&mut patch, ((bytes.len() as u64 - 1) << 2) | 1);
                patch.extend_from_slice(bytes);
            }
        }
    }
    patch.extend_from_slice(&crc32fast::hash(source).to_le_bytes());
    patch.extend_from_slice(&crc32fast::hash(target).to_le_bytes());
    let patch_crc = crc32fast::hash(&patch);
    patch.extend_from_slice(&patch_crc.to_le_bytes());
    patch
}

/// Create a deterministic BPS and prove that applying it recreates `target`.
pub(crate) fn create(source: &[u8], target: &[u8]) -> Result<Vec<u8>> {
    let patch = generate(source, target);
    ensure!(
        apply(source, &patch)? == target,
        "BPS self-check failed: applied output differs from target"
    );
    Ok(patch)
}

fn take_vli(patch: &[u8], position: &mut usize, end: usize) -> Result<u64> {
    ensure!(*position < end, "unexpected end of BPS actions");
    let (value, consumed) = decode_vli(&patch[*position..end])?;
    *position = position
        .checked_add(consumed)
        .ok_or_else(|| anyhow::anyhow!("BPS position overflow"))?;
    Ok(value)
}

fn action_length(value: u64) -> Result<usize> {
    usize::try_from(
        (value >> 2)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("BPS action length overflow"))?,
    )
    .map_err(Into::into)
}

fn relative_offset(current: i64, encoded: u64) -> Result<i64> {
    let distance = i64::try_from(encoded >> 1)?;
    if encoded & 1 == 0 {
        current
            .checked_add(distance)
            .ok_or_else(|| anyhow::anyhow!("BPS relative offset overflow"))
    } else {
        current
            .checked_sub(distance)
            .ok_or_else(|| anyhow::anyhow!("BPS relative offset overflow"))
    }
}

/// Apply a BPS1 action stream and verify patch, source, and target CRCs.
pub(crate) fn apply(source: &[u8], patch: &[u8]) -> Result<Vec<u8>> {
    ensure!(patch.len() >= 16, "BPS patch too small");
    ensure!(&patch[..4] == BPS_MAGIC, "not a BPS1 patch");
    let patch_crc_offset = patch.len() - 4;
    let stored_patch_crc = u32::from_le_bytes(patch[patch_crc_offset..].try_into()?);
    let actual_patch_crc = crc32fast::hash(&patch[..patch_crc_offset]);
    ensure!(
        stored_patch_crc == actual_patch_crc,
        "BPS patch CRC mismatch: expected {stored_patch_crc:08X}, actual {actual_patch_crc:08X}"
    );

    let footer = patch.len() - 12;
    let mut position = 4usize;
    let source_size = take_vli(patch, &mut position, footer)?;
    let target_size = take_vli(patch, &mut position, footer)?;
    let metadata_size = usize::try_from(take_vli(patch, &mut position, footer)?)?;
    position = position
        .checked_add(metadata_size)
        .ok_or_else(|| anyhow::anyhow!("BPS metadata length overflow"))?;
    ensure!(position <= footer, "BPS metadata exceeds action area");
    ensure!(
        source.len() as u64 == source_size,
        "BPS source size mismatch: expected {source_size}, actual {}",
        source.len()
    );
    let stored_source_crc = u32::from_le_bytes(patch[footer..footer + 4].try_into()?);
    let actual_source_crc = crc32fast::hash(source);
    ensure!(
        stored_source_crc == actual_source_crc,
        "BPS source CRC mismatch: expected {stored_source_crc:08X}, actual {actual_source_crc:08X}"
    );

    let target_len = usize::try_from(target_size)?;
    let mut target = vec![0u8; target_len];
    let mut output = 0usize;
    let mut source_relative = 0i64;
    let mut target_relative = 0i64;
    while position < footer {
        let action = take_vli(patch, &mut position, footer)?;
        let kind = action & 3;
        let length = action_length(action)?;
        let output_end = output
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("BPS output offset overflow"))?;
        ensure!(output_end <= target.len(), "BPS action exceeds target");
        match kind {
            0 => {
                ensure!(output_end <= source.len(), "BPS SourceRead exceeds source");
                target[output..output_end].copy_from_slice(&source[output..output_end]);
            }
            1 => {
                let input_end = position
                    .checked_add(length)
                    .ok_or_else(|| anyhow::anyhow!("BPS TargetRead offset overflow"))?;
                ensure!(input_end <= footer, "BPS TargetRead exceeds patch");
                target[output..output_end].copy_from_slice(&patch[position..input_end]);
                position = input_end;
            }
            2 => {
                let encoded = take_vli(patch, &mut position, footer)?;
                source_relative = relative_offset(source_relative, encoded)?;
                ensure!(source_relative >= 0, "BPS SourceCopy precedes source");
                let start = usize::try_from(source_relative)?;
                let end = start
                    .checked_add(length)
                    .ok_or_else(|| anyhow::anyhow!("BPS SourceCopy offset overflow"))?;
                ensure!(end <= source.len(), "BPS SourceCopy exceeds source");
                target[output..output_end].copy_from_slice(&source[start..end]);
                source_relative = i64::try_from(end)?;
            }
            3 => {
                let encoded = take_vli(patch, &mut position, footer)?;
                target_relative = relative_offset(target_relative, encoded)?;
                for destination in output..output_end {
                    ensure!(target_relative >= 0, "BPS TargetCopy precedes target");
                    let source_index = usize::try_from(target_relative)?;
                    ensure!(
                        source_index < destination,
                        "BPS TargetCopy reads unwritten target"
                    );
                    target[destination] = target[source_index];
                    target_relative = target_relative
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("BPS TargetCopy offset overflow"))?;
                }
            }
            _ => unreachable!(),
        }
        output = output_end;
    }
    ensure!(
        output == target.len(),
        "BPS output size mismatch: wrote {output}, expected {}",
        target.len()
    );
    let stored_target_crc = u32::from_le_bytes(patch[footer + 4..footer + 8].try_into()?);
    let actual_target_crc = crc32fast::hash(&target);
    ensure!(
        stored_target_crc == actual_target_crc,
        "BPS target CRC mismatch: expected {stored_target_crc:08X}, actual {actual_target_crc:08X}"
    );
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_patch_recreates_target() {
        let source = b"PC-98 Madou source";
        let target = b"PC-98 Madou Korean target with more data";
        let patch = create(source, target).unwrap();
        assert_eq!(apply(source, &patch).unwrap(), target);
    }

    #[test]
    fn wrong_source_and_corrupt_patch_are_rejected() {
        let mut patch = create(b"source", b"target").unwrap();
        assert!(apply(b"wrong!", &patch).is_err());
        patch[5] ^= 1;
        assert!(apply(b"source", &patch).is_err());
    }

    #[test]
    fn malformed_vli_is_rejected_without_panicking() {
        let mut patch = vec![0x7f; 32];
        patch[..4].copy_from_slice(BPS_MAGIC);
        let crc_offset = patch.len() - 4;
        let crc = crc32fast::hash(&patch[..crc_offset]);
        patch[crc_offset..].copy_from_slice(&crc.to_le_bytes());
        assert!(apply(&[], &patch).is_err());
    }

    #[test]
    fn source_copy_and_target_copy_actions_are_supported() {
        let source = b"abcdef";
        let target = b"cdecde";
        let mut patch = Vec::new();
        patch.extend_from_slice(BPS_MAGIC);
        encode_vli(&mut patch, source.len() as u64);
        encode_vli(&mut patch, target.len() as u64);
        encode_vli(&mut patch, 0);
        encode_vli(&mut patch, ((3u64 - 1) << 2) | 2);
        encode_vli(&mut patch, 2 << 1);
        encode_vli(&mut patch, ((3u64 - 1) << 2) | 3);
        encode_vli(&mut patch, 0);
        patch.extend_from_slice(&crc32fast::hash(source).to_le_bytes());
        patch.extend_from_slice(&crc32fast::hash(target).to_le_bytes());
        let crc = crc32fast::hash(&patch);
        patch.extend_from_slice(&crc.to_le_bytes());

        assert_eq!(apply(source, &patch).unwrap(), target);
    }
}
