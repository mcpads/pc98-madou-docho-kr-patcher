//! Structure and current-consumer validation for `SAMP.CNS`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use crate::compile_lz::decode_exact_compile_lz;
use crate::official_patch::apply_official_freeze_fix;
use crate::source_disk::sha256_hex;

const SAMP_PACKED_SHA256: &str = "f16bc3fb9c8db340040e5d240eb84ff68f1acb530bf9586ad0576720e57c4fe2";
const SAMP_DECODED_SHA256: &str =
    "381f4e4fccc9d3d3bb57a5dd880f2e44336aa613af7716abba377b90d09c08a1";
const MAIN_OVL_PACKED_SHA256: &str =
    "419178fd0a795f629a9d67f68d58e1e6cb7539d149cfeed435a5d2c21bda924a";
const MAIN_OVL_DECODED_SHA256: &str =
    "6886bdfcba6bfdc462be854322d6b67fd854ef4008efd03a831831c8b3f0f736";

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct SampleBankReport {
    pub name: String,
    pub packed_sha256: String,
    pub decoded_sha256: String,
    pub decoded_size: usize,
    pub format: String,
    pub consumer_program: String,
    pub table_bytes: usize,
    pub entry_count: usize,
    pub empty_entry_count: usize,
    pub reused_entry_count: usize,
    pub control_counts: Vec<SampleBankControlCount>,
    pub tracks: Vec<SampleBankTrackReport>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct SampleBankControlCount {
    pub value: u16,
    pub entry_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct SampleBankTrackReport {
    pub offset: usize,
    pub payload_offset: usize,
    pub payload_bytes: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct IndexedSampleBank {
    table_bytes: usize,
    entries: Vec<IndexedSampleEntry>,
    tracks: Vec<SampleBankTrackReport>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct IndexedSampleEntry {
    track_offset: Option<usize>,
    control: u16,
    link_target: Option<usize>,
}

pub(crate) fn inspect_sample_banks(
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<SampleBankReport>> {
    let packed = payload
        .get("SAMP.CNS")
        .context("MADOU.EXE payload is missing SAMP.CNS")?;
    ensure!(
        sha256_hex(packed) == SAMP_PACKED_SHA256,
        "SAMP.CNS differs from the supported indexed sample bank"
    );
    let decoded = decode_exact_compile_lz(packed).context("decode SAMP.CNS sample bank")?;
    let [stream] = decoded.streams.as_slice() else {
        bail!("SAMP.CNS must contain one decoded sample-bank stream");
    };
    ensure!(
        sha256_hex(stream) == SAMP_DECODED_SHA256,
        "decoded SAMP.CNS differs from the supported indexed sample bank"
    );
    validate_main_consumer(payload)?;
    let original_bsamp = payload
        .get("BSAMP.COM")
        .context("MADOU.EXE payload is missing BSAMP.COM")?;
    let fixed_bsamp = apply_official_freeze_fix(original_bsamp)?.updated_bsamp;
    validate_bsamp_consumer(&fixed_bsamp)?;

    let bank = parse_indexed_sample_bank(stream)?;
    ensure!(
        (
            stream.len(),
            bank.table_bytes,
            bank.entries.len(),
            bank.tracks.len()
        ) == (58_663, 0x90, 36, 19),
        "SAMP.CNS indexed sample-bank shape changed"
    );

    let mut control_counts = BTreeMap::<u16, usize>::new();
    for entry in &bank.entries {
        *control_counts.entry(entry.control).or_default() += 1;
    }
    ensure!(
        control_counts == BTreeMap::from([(0x0000, 23), (0x01f4, 8), (0x8001, 5)]),
        "SAMP.CNS control-word population changed"
    );
    let empty_entry_count = bank
        .entries
        .iter()
        .filter(|entry| entry.track_offset.is_none())
        .count();
    let populated_entry_count = bank.entries.len() - empty_entry_count;

    Ok(vec![SampleBankReport {
        name: "SAMP.CNS".to_owned(),
        packed_sha256: sha256_hex(packed),
        decoded_sha256: sha256_hex(stream),
        decoded_size: stream.len(),
        format: "indexed BSAMP length-prefixed sample bank".to_owned(),
        consumer_program: "MAIN.OVL".to_owned(),
        table_bytes: bank.table_bytes,
        entry_count: bank.entries.len(),
        empty_entry_count,
        reused_entry_count: populated_entry_count - bank.tracks.len(),
        control_counts: control_counts
            .into_iter()
            .map(|(value, entry_count)| SampleBankControlCount { value, entry_count })
            .collect(),
        tracks: bank.tracks,
    }])
}

fn parse_indexed_sample_bank(bytes: &[u8]) -> Result<IndexedSampleBank> {
    ensure!(
        bytes.len() >= 4,
        "indexed sample bank is shorter than one table entry"
    );
    let first_track = bytes
        .chunks_exact(4)
        .find_map(|entry| {
            let pointer = usize::from(u16::from_le_bytes([entry[0], entry[1]]));
            (pointer != 0).then_some(pointer)
        })
        .context("indexed sample bank has no populated entry")?;
    ensure!(
        first_track.is_multiple_of(4) && first_track <= bytes.len(),
        "indexed sample-bank first track 0x{first_track:X} is not a table boundary"
    );

    let entry_count = first_track / 4;
    let mut entries = Vec::with_capacity(entry_count);
    let mut track_offsets = BTreeSet::new();
    for (index, entry) in bytes[..first_track].chunks_exact(4).enumerate() {
        let pointer = usize::from(u16::from_le_bytes([entry[0], entry[1]]));
        let control = u16::from_le_bytes([entry[2], entry[3]]);
        if pointer == 0 {
            ensure!(
                control == 0,
                "indexed sample-bank entry {index} has control 0x{control:04X} without a track"
            );
            entries.push(IndexedSampleEntry {
                track_offset: None,
                control,
                link_target: None,
            });
            continue;
        }
        ensure!(
            (first_track..bytes.len()).contains(&pointer),
            "indexed sample-bank entry {index} points outside the track region at 0x{pointer:X}"
        );
        track_offsets.insert(pointer);

        let link_target = if control & 0x8000 != 0 {
            let byte_delta = control.wrapping_shl(2) as i16;
            let target = isize::try_from(index)? + isize::from(byte_delta / 4);
            ensure!(
                target >= 0 && usize::try_from(target)? < entry_count,
                "indexed sample-bank entry {index} link 0x{control:04X} leaves the table"
            );
            Some(usize::try_from(target)?)
        } else {
            None
        };
        entries.push(IndexedSampleEntry {
            track_offset: Some(pointer),
            control,
            link_target,
        });
    }

    let track_offsets = track_offsets.into_iter().collect::<Vec<_>>();
    ensure!(
        track_offsets.first().copied() == Some(first_track),
        "indexed sample-bank first selected track does not meet the table end"
    );
    let mut tracks = Vec::with_capacity(track_offsets.len());
    for (index, &offset) in track_offsets.iter().enumerate() {
        let length = bytes
            .get(offset..offset + 2)
            .with_context(|| format!("sample track header at 0x{offset:X} is truncated"))?;
        let payload_bytes = usize::from(u16::from_le_bytes([length[0], length[1]]));
        let payload_offset = offset + 2;
        let end = payload_offset
            .checked_add(payload_bytes)
            .context("sample track end overflows")?;
        let expected_end = track_offsets.get(index + 1).copied().unwrap_or(bytes.len());
        ensure!(
            end == expected_end,
            "sample track at 0x{offset:X} ends at 0x{end:X}, expected 0x{expected_end:X}"
        );
        tracks.push(SampleBankTrackReport {
            offset,
            payload_offset,
            payload_bytes,
        });
    }

    Ok(IndexedSampleBank {
        table_bytes: first_track,
        entries,
        tracks,
    })
}

fn validate_main_consumer(payload: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let packed = payload
        .get("MAIN.OVL")
        .context("MADOU.EXE payload is missing MAIN.OVL")?;
    ensure!(
        sha256_hex(packed) == MAIN_OVL_PACKED_SHA256,
        "MAIN.OVL differs from the supported SAMP.CNS consumer"
    );
    let decoded = decode_exact_compile_lz(packed).context("decode SAMP.CNS consumer MAIN.OVL")?;
    let [decoded] = decoded.streams.as_slice() else {
        bail!("SAMP.CNS consumer MAIN.OVL must contain one decoded stream");
    };
    ensure!(
        sha256_hex(decoded) == MAIN_OVL_DECODED_SHA256,
        "decoded MAIN.OVL differs from the supported SAMP.CNS consumer"
    );

    let filename_pointer = u16::from_le_bytes(
        decoded[0x56de..0x56e0]
            .try_into()
            .expect("two bytes convert to a word"),
    );
    ensure!(
        filename_pointer == 0x6411 && decoded.get(0x6311..0x631c) == Some(b"0:SAMP.CNS\0"),
        "MAIN.OVL SAMP.CNS state-table filename binding changed"
    );
    ensure!(
        decoded.get(0x0194..0x01a3)
            == Some(
                [
                    0xb4, 0x01, 0xcd, 0x7b, 0x8c, 0xc8, 0x8e, 0xd8, 0xba, 0xc0, 0x57, 0xb4, 0x00,
                    0xcd, 0x7b,
                ]
                .as_slice()
            ),
        "MAIN.OVL state-table installation changed"
    );
    ensure!(
        decoded.get(0x02af..0x02d0)
            == Some(
                [
                    0xb4, 0x02, 0xba, 0x1e, 0x00, 0xcd, 0x7b, 0x8b, 0xd0, 0x8b, 0x3e, 0x38, 0x9f,
                    0xe8, 0x51, 0x42, 0xb8, 0x00, 0xa8, 0x8e, 0xc0, 0x2e, 0x8e, 0x1e, 0x38, 0x9f,
                    0x33, 0xf6, 0x8b, 0xfe, 0xb4, 0x03, 0xcd,
                ]
                .as_slice()
            ),
        "MAIN.OVL SAMP.CNS load and A800 decode path changed"
    );
    ensure!(
        decoded.get(0x0361..0x03bb)
            == Some(
                [
                    0x8e, 0xd8, 0x8a, 0xc5, 0x8b, 0xf0, 0x83, 0xe6, 0x7f, 0xd1, 0xe6, 0xd1, 0xe6,
                    0x8b, 0x1c, 0x0b, 0xdb, 0x75, 0x01, 0xc3, 0x8c, 0xd8, 0x8b, 0xfb, 0xc1, 0xef,
                    0x04, 0x03, 0xc7, 0x8e, 0xc0, 0x83, 0xe3, 0x0f, 0x8b, 0x7c, 0x02, 0x0b, 0xff,
                    0x7e, 0x13, 0x83, 0xe1, 0x07, 0x83, 0xe9, 0x01, 0x76, 0x0b, 0x8b, 0xd7, 0x80,
                    0xe6, 0x7f, 0xb4, 0x04, 0xcd, 0x7d, 0xe2, 0xfa, 0x33, 0xd2, 0xb4, 0x04, 0xcd,
                    0x7d, 0xd1, 0xe7, 0x73, 0x13, 0xd1, 0xe7, 0x03, 0xf7, 0x1e, 0x56, 0xe8, 0x19,
                    0x15, 0xb9, 0x0a, 0x00, 0xe8, 0xfe, 0x14, 0x5e, 0x1f, 0xeb, 0xb4, 0xc3,
                ]
                .as_slice()
            ),
        "MAIN.OVL indexed sample selector and INT 7Dh consumer changed"
    );
    Ok(())
}

fn validate_bsamp_consumer(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.get(0x154c..0x1554)
            == Some([0xba, 0x20, 0x01, 0xb8, 0x7e, 0x25, 0xcd, 0x21].as_slice()),
        "officially fixed BSAMP.COM no longer installs its handler at INT 7Eh / 0x0120"
    );
    ensure!(
        bytes.get(0x00ee..0x010a)
            == Some(
                [
                    0xf6, 0xc1, 0x02, 0x75, 0x17, 0x0b, 0xd5, 0x75, 0x05, 0x26, 0x8b, 0x17, 0x2b,
                    0xed, 0x83, 0xc3, 0x02, 0x73, 0x09, 0x50, 0x8c, 0xc0, 0x05, 0x00, 0x10, 0x8e,
                    0xc0, 0x58,
                ]
                .as_slice()
            ),
        "officially fixed BSAMP.COM length-prefixed sample path changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reused_tracks_and_relative_links() {
        let mut bytes = vec![0u8; 12];
        bytes[0..4].copy_from_slice(&[12, 0, 0xf4, 0x01]);
        bytes[4..8].copy_from_slice(&[17, 0, 0x01, 0x80]);
        bytes[8..12].copy_from_slice(&[17, 0, 0xff, 0xff]);
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&[1, 2, 3]);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&[4, 5]);

        let bank = parse_indexed_sample_bank(&bytes).unwrap();
        assert_eq!(bank.table_bytes, 12);
        assert_eq!(bank.entries.len(), 3);
        assert_eq!(bank.tracks.len(), 2);
        assert_eq!(bank.entries[1].link_target, Some(2));
        assert_eq!(bank.entries[2].link_target, Some(1));
        assert_eq!(bank.tracks[0].payload_bytes, 3);
        assert_eq!(bank.tracks[1].payload_bytes, 2);
    }

    #[test]
    fn rejects_a_gap_between_declared_tracks() {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(&[8, 0, 0, 0]);
        bytes[4..8].copy_from_slice(&[13, 0, 0, 0]);
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        bytes[13..15].copy_from_slice(&1u16.to_le_bytes());

        let error = parse_indexed_sample_bank(&bytes).unwrap_err().to_string();
        assert!(error.contains("ends at 0xC, expected 0xD"));
    }
}
