use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::source_disk::sha256_hex;

const MAX_DECODED_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_LITERAL_BYTES: usize = 0x7f;
const MAX_COPY_BYTES: usize = 0x82;
const MAX_COPY_DISTANCE: usize = 0x100;

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct CompileLzStreamReport {
    pub input_offset: usize,
    pub packed_size: usize,
    pub decoded_size: usize,
    pub decoded_sha256: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExactCompileLzReport {
    pub streams: Vec<CompileLzStreamReport>,
    pub decoded_size: usize,
}

pub(crate) struct DecodedCompileLz {
    pub report: ExactCompileLzReport,
    pub streams: Vec<Vec<u8>>,
}

pub(crate) fn decode_exact_compile_lz(input: &[u8]) -> Option<DecodedCompileLz> {
    let mut input_offset = 0usize;
    let mut streams = Vec::new();
    let mut reports = Vec::new();

    while input_offset < input.len() {
        let decoded = decode_stream(&input[input_offset..]).ok()?;
        if decoded.bytes.is_empty() {
            return None;
        }
        reports.push(CompileLzStreamReport {
            input_offset,
            packed_size: decoded.bytes_consumed,
            decoded_size: decoded.bytes.len(),
            decoded_sha256: sha256_hex(&decoded.bytes),
        });
        input_offset = input_offset.checked_add(decoded.bytes_consumed)?;
        streams.push(decoded.bytes);
    }

    if input_offset != input.len() || streams.is_empty() {
        return None;
    }
    let decoded_size = streams.iter().map(Vec::len).sum();
    Some(DecodedCompileLz {
        report: ExactCompileLzReport {
            streams: reports,
            decoded_size,
        },
        streams,
    })
}

pub(crate) fn encode_compile_lz(bytes: &[u8]) -> Vec<u8> {
    let mut packed = Vec::with_capacity(bytes.len());
    let mut literals = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let (copy_bytes, distance) = longest_previous_match(bytes, cursor);
        if copy_bytes >= 3 {
            append_literals(&mut packed, &mut literals);
            packed.push(0x80 | u8::try_from(copy_bytes - 3).expect("copy length fits u8"));
            packed.push(u8::try_from(distance - 1).expect("copy distance fits u8"));
            cursor += copy_bytes;
        } else {
            literals.push(bytes[cursor]);
            cursor += 1;
            if literals.len() == MAX_LITERAL_BYTES {
                append_literals(&mut packed, &mut literals);
            }
        }
    }
    append_literals(&mut packed, &mut literals);
    packed.push(0);
    packed
}

fn longest_previous_match(bytes: &[u8], cursor: usize) -> (usize, usize) {
    let maximum_bytes = MAX_COPY_BYTES.min(bytes.len() - cursor);
    let maximum_distance = MAX_COPY_DISTANCE.min(cursor);
    let mut best = (0usize, 0usize);
    for distance in 1..=maximum_distance {
        let mut matched = 0usize;
        while matched < maximum_bytes
            && bytes[cursor + matched] == bytes[cursor - distance + matched]
        {
            matched += 1;
        }
        if matched > best.0 {
            best = (matched, distance);
        }
    }
    best
}

fn append_literals(packed: &mut Vec<u8>, literals: &mut Vec<u8>) {
    if literals.is_empty() {
        return;
    }
    packed.push(u8::try_from(literals.len()).expect("literal length fits u8"));
    packed.append(literals);
}

struct DecodedStream {
    bytes: Vec<u8>,
    bytes_consumed: usize,
}

fn decode_stream(input: &[u8]) -> Result<DecodedStream> {
    let mut bytes = Vec::new();
    let mut cursor = 0usize;

    loop {
        let command = *input
            .get(cursor)
            .context("Compile-LZ stream has no terminator")?;
        cursor += 1;
        if command == 0 {
            return Ok(DecodedStream {
                bytes,
                bytes_consumed: cursor,
            });
        }

        if command < 0x80 {
            let literal_bytes = usize::from(command);
            let end = cursor
                .checked_add(literal_bytes)
                .context("Compile-LZ literal offset overflow")?;
            let literal = input
                .get(cursor..end)
                .context("Compile-LZ literal exceeds the packed stream")?;
            ensure_decoded_capacity(bytes.len(), literal.len())?;
            bytes.extend_from_slice(literal);
            cursor = end;
            continue;
        }

        let distance = usize::from(
            *input
                .get(cursor)
                .context("Compile-LZ back-reference lacks a distance")?,
        ) + 1;
        cursor += 1;
        let copy_bytes = usize::from(command & 0x7f) + 3;
        ensure_decoded_capacity(bytes.len(), copy_bytes)?;
        let source_start = bytes.len() as isize - distance as isize;
        for index in 0..copy_bytes {
            let source = source_start + index as isize;
            let byte = if source < 0 {
                0
            } else {
                bytes[usize::try_from(source).expect("non-negative source fits usize")]
            };
            bytes.push(byte);
        }
    }
}

fn ensure_decoded_capacity(current: usize, additional: usize) -> Result<()> {
    let total = current
        .checked_add(additional)
        .context("Compile-LZ decoded size overflow")?;
    if total > MAX_DECODED_STREAM_BYTES {
        bail!("Compile-LZ stream exceeds the decoded-size safety limit");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_literals_and_overlapping_back_references() {
        let decoded = decode_stream(&[3, b'a', b'b', b'c', 0x83, 2, 0]).unwrap();
        assert_eq!(decoded.bytes, b"abcabcabc");
        assert_eq!(decoded.bytes_consumed, 7);
    }

    #[test]
    fn decodes_zero_fill_before_the_output_start() {
        let decoded = decode_stream(&[0x80, 2, 0]).unwrap();
        assert_eq!(decoded.bytes, [0, 0, 0]);
    }

    #[test]
    fn reports_only_whole_nonempty_stream_sequences() {
        let exact = decode_exact_compile_lz(&[1, b'a', 0, 2, b'b', b'c', 0]).unwrap();
        assert_eq!(exact.report.decoded_size, 3);
        assert_eq!(exact.report.streams.len(), 2);
        assert_eq!(exact.report.streams[1].input_offset, 3);
        assert!(decode_exact_compile_lz(&[1, b'a', 0, 0]).is_none());
        assert!(decode_exact_compile_lz(&[1, b'a']).is_none());
    }

    #[test]
    fn encoded_stream_roundtrips_repeated_and_literal_bytes() {
        let mut original = b"opening graphic ".repeat(40);
        original.extend(0_u8..=255);
        let packed = encode_compile_lz(&original);
        let decoded = decode_exact_compile_lz(&packed).unwrap();
        assert_eq!(decoded.streams, [original]);
    }
}
