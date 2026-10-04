use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::compile_lz::{ExactCompileLzReport, decode_exact_compile_lz};

pub(crate) const MAIN_COM_PACKED_STREAM_OFFSET: usize = 0x71;
const MAIN_COM_SELF_UNPACKER: [u8; MAIN_COM_PACKED_STREAM_OFFSET] = [
    0xfc, 0x60, 0x8c, 0xc8, 0x8e, 0xd8, 0x80, 0xc4, 0x10, 0x8e, 0xc0, 0xbe, 0x00, 0x01, 0x8b, 0xfe,
    0xb9, 0x80, 0x7f, 0xf3, 0xa5, 0x0e, 0x06, 0x68, 0x1b, 0x01, 0xcb, 0x8c, 0xd8, 0x05, 0x10, 0x00,
    0x8e, 0xc0, 0x8c, 0xc8, 0x8e, 0xd8, 0xbe, 0x71, 0x01, 0x33, 0xff, 0xac, 0x0a, 0xc0, 0x74, 0x36,
    0x78, 0x06, 0x8a, 0xc8, 0xf3, 0xa4, 0xeb, 0xf3, 0x25, 0x7f, 0x00, 0x05, 0x03, 0x00, 0x8b, 0xc8,
    0xac, 0x40, 0x8b, 0xde, 0x8c, 0xda, 0x8b, 0xf7, 0x2b, 0xf0, 0x8c, 0xc0, 0x8e, 0xd8, 0x72, 0x08,
    0xf3, 0xa4, 0x8b, 0xf3, 0x8e, 0xda, 0xeb, 0xd3, 0x8a, 0xc5, 0xaa, 0x46, 0xe0, 0xfc, 0x74, 0xf0,
    0x8b, 0xf3, 0x8e, 0xda, 0xeb, 0xc5, 0x58, 0x8e, 0xc0, 0x8e, 0xd8, 0x61, 0x1e, 0x68, 0x00, 0x01,
    0xcb,
];
const JAPANESE_DATA_ARCHIVE_NAME: &str = "%93%B9%91%90%88%D9%95%B7.DAT";
const JAPANESE_DATA_NAME: &[u8] = &[
    0x93, 0xb9, 0x91, 0x90, 0x88, 0xd9, 0x95, 0xb7, b'.', b'D', b'A', b'T',
];

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct PayloadFilenameReference {
    pub source: String,
    pub view: String,
    pub target: String,
    pub offsets: Vec<usize>,
}

pub(crate) fn exact_compile_lz_report(name: &str, bytes: &[u8]) -> Option<ExactCompileLzReport> {
    if name.ends_with(".COM") || name.ends_with(".BAT") {
        return None;
    }
    decode_exact_compile_lz(bytes).map(|decoded| decoded.report)
}

pub(crate) fn decode_main_com_image(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        bytes.get(..MAIN_COM_PACKED_STREAM_OFFSET) == Some(MAIN_COM_SELF_UNPACKER.as_slice()),
        "MAIN.COM self-unpacker differs from the supported program"
    );
    let packed = bytes
        .get(MAIN_COM_PACKED_STREAM_OFFSET..)
        .context("MAIN.COM packed stream is missing")?;
    let decoded = decode_exact_compile_lz(packed)
        .context("MAIN.COM body is not one exact Compile-LZ stream")?;
    ensure!(
        decoded.streams.len() == 1,
        "MAIN.COM has more than one decoded program stream"
    );
    Ok(decoded.streams.into_iter().next().unwrap())
}

pub(crate) fn find_payload_filename_references(
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<PayloadFilenameReference>> {
    let mut views = Vec::<(String, String, Vec<u8>)>::new();
    for (name, bytes) in payload {
        if name.ends_with(".OVL") {
            let decoded = decode_exact_compile_lz(bytes)
                .ok_or_else(|| anyhow::anyhow!("{name} is not an exact Compile-LZ overlay"))?;
            ensure!(
                decoded.streams.len() == 1,
                "{name} has more than one decoded program stream"
            );
            views.push((
                name.clone(),
                "decoded_compile_lz".to_owned(),
                decoded.streams.into_iter().next().unwrap(),
            ));
        } else if name == "MAIN.COM" {
            views.push((
                name.clone(),
                "self_unpacked_program".to_owned(),
                decode_main_com_image(bytes)?,
            ));
        } else if name.ends_with(".COM") || name.ends_with(".BAT") {
            views.push((name.clone(), "raw".to_owned(), bytes.clone()));
        }
    }

    let mut references = Vec::new();
    for (source, view, bytes) in views {
        for target in payload.keys() {
            let needle = target_bytes(target);
            let offsets = find_ascii_case_insensitive(&bytes, &needle);
            if !offsets.is_empty() {
                references.push(PayloadFilenameReference {
                    source: source.clone(),
                    view: view.clone(),
                    target: target.clone(),
                    offsets,
                });
            }
        }
    }
    references.sort_by(|left, right| {
        (&left.source, &left.view, &left.target).cmp(&(&right.source, &right.view, &right.target))
    });
    Ok(references)
}

fn target_bytes(name: &str) -> Vec<u8> {
    if name == JAPANESE_DATA_ARCHIVE_NAME {
        JAPANESE_DATA_NAME.to_vec()
    } else {
        name.as_bytes().to_vec()
    }
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(offset, candidate)| {
            candidate
                .iter()
                .zip(needle)
                .all(|(&left, &right)| left.eq_ignore_ascii_case(&right))
                .then_some(offset)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_all_ascii_case_insensitive_offsets() {
        assert_eq!(
            find_ascii_case_insensitive(b"op1.cns OP1.CNS", b"OP1.CNS"),
            [0, 8]
        );
    }

    #[test]
    fn preserves_the_shift_jis_save_name_for_search() {
        assert_eq!(target_bytes(JAPANESE_DATA_ARCHIVE_NAME), JAPANESE_DATA_NAME);
    }

    #[test]
    fn main_com_decodes_the_stream_after_the_self_unpacker() {
        let expected = b"complete decoded program";
        let mut program = MAIN_COM_SELF_UNPACKER.to_vec();
        program.extend(crate::compile_lz::encode_compile_lz(expected));

        assert_eq!(decode_main_com_image(&program).unwrap(), expected);
    }
}
