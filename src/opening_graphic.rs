use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::compile_lz::{decode_exact_compile_lz, encode_compile_lz};
use crate::font::{
    FontReport, GLYPH_BYTES, GLYPH_HEIGHT, GLYPH_WIDTH, font_report, rasterize_character,
};
use crate::source_disk::sha256_hex;

const OPENING_TEXT_RESOURCE: &str = "OP3.CNS";
const OPENING_CONSUMER_RESOURCE: &str = "OPEN.OVL";
const ORIGINAL_PACKED_SHA256: &str =
    "c6b0fa234ba27aff49546f0745c378efc38464a5a77236945b245d127bbe45e6";
const ORIGINAL_DECODED_SHA256: &str =
    "9cb4db910af4559764d7a76c270cfee1fdc758db87b23995da23bcc07519ac58";
const ORIGINAL_CONSUMER_PACKED_SHA256: &str =
    "394381d9b1fec74e5bd87087f6c3e9888c79e640565e42fb8df940b4700f43dc";
const ORIGINAL_CONSUMER_DECODED_SHA256: &str =
    "39284b67a631919d18bb833f0a46fe96665284c431030b482c2c163d9f3f7da5";
const SCREEN_WIDTH: usize = 640;
const SCREEN_HEIGHT: usize = 400;
const SCREEN_BYTES: usize = SCREEN_WIDTH * SCREEN_HEIGHT / 8;
const TEXT_RIGHT: usize = 320;
const TEXT_BOTTOM: usize = 128;
const DRAFT_SCHEMA: &str = "pc98_madou_docho.opening_graphic_draft";
const DRAFT_STATUS: &str = "needs_human_review";
const RELEASE_STATUS: &str = "distribution_eligible";

#[derive(Clone, Copy)]
struct OpeningTextStrip {
    source_y: usize,
    visible_row_bytes: usize,
}

// OPEN.OVL copies these eight 16-row strips from OP3.CNS. Each routine grows
// DX from one byte up to, but not including, its terminal comparison value.
const OPENING_TEXT_STRIPS: [OpeningTextStrip; 8] = [
    OpeningTextStrip {
        source_y: 0,
        visible_row_bytes: 0x25,
    },
    OpeningTextStrip {
        source_y: 16,
        visible_row_bytes: 0x07,
    },
    OpeningTextStrip {
        source_y: 32,
        visible_row_bytes: 0x1f,
    },
    OpeningTextStrip {
        source_y: 48,
        visible_row_bytes: 0x27,
    },
    OpeningTextStrip {
        source_y: 64,
        visible_row_bytes: 0x07,
    },
    OpeningTextStrip {
        source_y: 80,
        visible_row_bytes: 0x21,
    },
    OpeningTextStrip {
        source_y: 96,
        visible_row_bytes: 0x09,
    },
    OpeningTextStrip {
        source_y: 112,
        visible_row_bytes: 0x21,
    },
];

#[derive(Debug, Deserialize)]
struct OpeningGraphicDraft {
    schema: String,
    status: String,
    entries: Vec<OpeningTextEntry>,
}

#[derive(Debug, Deserialize)]
struct OpeningTextEntry {
    x: usize,
    y: usize,
    text: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct OpeningGraphicPatchReport {
    pub resource: String,
    pub draft_status: String,
    pub entry_count: usize,
    pub original_packed_sha256: String,
    pub original_decoded_sha256: String,
    pub updated_packed_sha256: String,
    pub updated_decoded_sha256: String,
    pub original_packed_size: usize,
    pub updated_packed_size: usize,
    pub preserved_outside_text_rectangle: bool,
    pub font: FontReport,
}

pub(crate) fn apply_opening_graphic_draft(
    payload: &mut BTreeMap<String, Vec<u8>>,
    draft_path: &Path,
) -> Result<OpeningGraphicPatchReport> {
    let draft_bytes = fs::read(draft_path).with_context(|| {
        format!(
            "failed to read opening graphic draft: {}",
            draft_path.display()
        )
    })?;
    let draft: OpeningGraphicDraft = serde_json::from_slice(&draft_bytes).with_context(|| {
        format!(
            "failed to parse opening graphic draft: {}",
            draft_path.display()
        )
    })?;
    validate_draft(&draft)?;
    validate_opening_consumer(payload)?;

    let original_packed = payload
        .get(OPENING_TEXT_RESOURCE)
        .context("installer payload is missing OP3.CNS")?;
    ensure!(
        sha256_hex(original_packed) == ORIGINAL_PACKED_SHA256,
        "OP3.CNS does not match the supported opening text resource"
    );
    let decoded = decode_exact_compile_lz(original_packed)
        .context("OP3.CNS is not an exact Compile-LZ resource")?;
    ensure!(
        decoded.streams.len() == 1 && decoded.streams[0].len() == SCREEN_BYTES,
        "OP3.CNS is not one 640x400 1bpp screen"
    );
    let original_decoded = &decoded.streams[0];
    ensure!(
        sha256_hex(original_decoded) == ORIGINAL_DECODED_SHA256,
        "OP3.CNS decoded pixels do not match the supported opening screen"
    );

    let mut updated_decoded = original_decoded.clone();
    clear_text_rectangle(&mut updated_decoded);
    for entry in &draft.entries {
        draw_text(&mut updated_decoded, entry)?;
    }
    verify_preserved_pixels(original_decoded, &updated_decoded)?;
    ensure!(
        updated_decoded != *original_decoded,
        "opening graphic draft did not change OP3.CNS"
    );

    let updated_packed = encode_compile_lz(&updated_decoded);
    let roundtrip = decode_exact_compile_lz(&updated_packed)
        .context("repacked OP3.CNS did not decode as exact Compile-LZ")?;
    ensure!(
        roundtrip.streams == [updated_decoded.clone()],
        "repacked OP3.CNS did not preserve the updated pixels"
    );
    let report = OpeningGraphicPatchReport {
        resource: OPENING_TEXT_RESOURCE.to_owned(),
        draft_status: draft.status,
        entry_count: draft.entries.len(),
        original_packed_sha256: sha256_hex(original_packed),
        original_decoded_sha256: sha256_hex(original_decoded),
        updated_packed_sha256: sha256_hex(&updated_packed),
        updated_decoded_sha256: sha256_hex(&updated_decoded),
        original_packed_size: original_packed.len(),
        updated_packed_size: updated_packed.len(),
        preserved_outside_text_rectangle: true,
        font: font_report()?,
    };
    payload.insert(OPENING_TEXT_RESOURCE.to_owned(), updated_packed);
    Ok(report)
}

fn validate_draft(draft: &OpeningGraphicDraft) -> Result<()> {
    ensure!(
        draft.schema == DRAFT_SCHEMA,
        "unsupported opening graphic draft schema {:?}",
        draft.schema
    );
    ensure!(
        matches!(draft.status.as_str(), DRAFT_STATUS | RELEASE_STATUS),
        "opening graphic requires status {DRAFT_STATUS:?} or {RELEASE_STATUS:?}"
    );
    ensure!(
        !draft.entries.is_empty(),
        "opening graphic draft has no entries"
    );
    let mut populated_strips = std::collections::BTreeSet::new();
    for entry in &draft.entries {
        ensure!(!entry.text.is_empty(), "opening graphic entry is empty");
        ensure!(
            entry.x.is_multiple_of(GLYPH_WIDTH) && entry.y.is_multiple_of(GLYPH_HEIGHT),
            "opening graphic entry coordinates must use 16x16 cells"
        );
        let strip = OPENING_TEXT_STRIPS
            .iter()
            .find(|strip| strip.source_y == entry.y)
            .with_context(|| {
                format!(
                    "opening graphic entry at y={} is not consumed by OPEN.OVL",
                    entry.y
                )
            })?;
        ensure!(
            populated_strips.insert(entry.y),
            "opening graphic has more than one entry for consumer strip y={}",
            entry.y
        );
        let width = entry
            .text
            .chars()
            .count()
            .checked_mul(GLYPH_WIDTH)
            .context("opening graphic text width overflow")?;
        ensure!(
            entry.x + width <= strip.visible_row_bytes * 8,
            "opening graphic entry at y={} does not fit the {}-pixel OPEN.OVL copy",
            entry.y,
            strip.visible_row_bytes * 8
        );
    }
    let consumed_strips = OPENING_TEXT_STRIPS
        .iter()
        .map(|strip| strip.source_y)
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(
        populated_strips == consumed_strips,
        "opening graphic must provide one phrase for every OPEN.OVL consumer strip"
    );
    Ok(())
}

fn validate_opening_consumer(payload: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let packed = payload
        .get(OPENING_CONSUMER_RESOURCE)
        .context("installer payload is missing OPEN.OVL")?;
    ensure!(
        sha256_hex(packed) == ORIGINAL_CONSUMER_PACKED_SHA256,
        "OPEN.OVL does not match the supported opening-strip consumer"
    );
    let decoded =
        decode_exact_compile_lz(packed).context("OPEN.OVL is not one exact Compile-LZ stream")?;
    ensure!(
        decoded.streams.len() == 1
            && sha256_hex(&decoded.streams[0]) == ORIGINAL_CONSUMER_DECODED_SHA256,
        "decoded OPEN.OVL does not match the supported opening-strip consumer"
    );
    Ok(())
}

fn clear_text_rectangle(screen: &mut [u8]) {
    let row_bytes = SCREEN_WIDTH / 8;
    for y in 0..TEXT_BOTTOM {
        screen[y * row_bytes..y * row_bytes + TEXT_RIGHT / 8].fill(0);
    }
}

fn draw_text(screen: &mut [u8], entry: &OpeningTextEntry) -> Result<()> {
    for (index, character) in entry.text.chars().enumerate() {
        let glyph = rasterize_character(character)
            .with_context(|| format!("failed to rasterize opening character {character:?}"))?;
        ensure!(
            glyph.len() == GLYPH_BYTES,
            "opening glyph has an invalid size"
        );
        draw_glyph(screen, entry.x + index * GLYPH_WIDTH, entry.y, &glyph);
    }
    Ok(())
}

fn draw_glyph(screen: &mut [u8], origin_x: usize, origin_y: usize, glyph: &[u8; GLYPH_BYTES]) {
    let row_bytes = SCREEN_WIDTH / 8;
    for y in 0..GLYPH_HEIGHT {
        for byte_x in 0..2 {
            screen[(origin_y + y) * row_bytes + origin_x / 8 + byte_x] |= glyph[y * 2 + byte_x];
        }
    }
}

fn verify_preserved_pixels(original: &[u8], updated: &[u8]) -> Result<()> {
    ensure!(
        original.len() == SCREEN_BYTES && updated.len() == SCREEN_BYTES,
        "opening screen size changed"
    );
    let row_bytes = SCREEN_WIDTH / 8;
    for y in 0..SCREEN_HEIGHT {
        for byte_x in 0..row_bytes {
            if y < TEXT_BOTTOM && byte_x < TEXT_RIGHT / 8 {
                continue;
            }
            ensure!(
                original[y * row_bytes + byte_x] == updated[y * row_bytes + byte_x],
                "opening graphic changed outside the text rectangle at byte ({byte_x}, {y})"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_phrase_per_consumer_strip() -> Vec<OpeningTextEntry> {
        OPENING_TEXT_STRIPS
            .iter()
            .map(|strip| OpeningTextEntry {
                x: 0,
                y: strip.source_y,
                text: "가".to_owned(),
            })
            .collect()
    }

    #[test]
    fn draft_validation_rejects_text_outside_the_owned_rectangle() {
        let draft = OpeningGraphicDraft {
            schema: DRAFT_SCHEMA.to_owned(),
            status: DRAFT_STATUS.to_owned(),
            entries: {
                let mut entries = one_phrase_per_consumer_strip();
                entries[0].x = TEXT_RIGHT;
                entries
            },
        };
        assert!(
            validate_draft(&draft)
                .unwrap_err()
                .to_string()
                .contains("does not fit")
        );
    }

    #[test]
    fn draft_validation_rejects_a_missing_consumer_strip() {
        let draft = OpeningGraphicDraft {
            schema: DRAFT_SCHEMA.to_owned(),
            status: DRAFT_STATUS.to_owned(),
            entries: one_phrase_per_consumer_strip()
                .into_iter()
                .filter(|entry| entry.y != 16)
                .collect(),
        };
        assert!(
            validate_draft(&draft)
                .unwrap_err()
                .to_string()
                .contains("every OPEN.OVL consumer strip")
        );
    }

    #[test]
    fn draft_validation_rejects_text_wider_than_the_consumed_strip() {
        let draft = OpeningGraphicDraft {
            schema: DRAFT_SCHEMA.to_owned(),
            status: DRAFT_STATUS.to_owned(),
            entries: {
                let mut entries = one_phrase_per_consumer_strip();
                entries[1].text = "가나다라".to_owned();
                entries
            },
        };
        assert!(
            validate_draft(&draft)
                .unwrap_err()
                .to_string()
                .contains("OPEN.OVL copy")
        );
    }
}
