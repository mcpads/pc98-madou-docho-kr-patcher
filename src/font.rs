use std::sync::OnceLock;

use anyhow::{Context, Result, bail, ensure};
use fontdue::{Font, FontSettings};
use serde::{Deserialize, Serialize};

use crate::local_input;
use crate::source_disk::sha256_hex;

pub(crate) const GLYPH_WIDTH: usize = 16;
pub(crate) const GLYPH_HEIGHT: usize = 16;
pub(crate) const GLYPH_BYTES: usize = 32;
const PROFILE_SCHEMA: &str = "pc98_madou_docho.font_profile";
const FONT_PATH: &str = "fonts/Galmuri14.ttf";
const PROFILE_JSON: &str = include_str!("../assets/fonts/galmuri14-pc98-16x16.json");

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct FontReport {
    pub profile_id: String,
    pub font: String,
    pub font_sha256: String,
    pub font_version: String,
    pub license: String,
    pub source: String,
    pub upstream_revision: String,
}

#[derive(Debug, Deserialize)]
struct FontProfile {
    schema: String,
    id: String,
    font: String,
    font_sha256: String,
    font_version: String,
    font_size: u16,
    baseline_y: u8,
    vertical_fit: String,
    threshold: u8,
    rasterizer: String,
    license: String,
    source: String,
    upstream_revision: String,
}

struct EmbeddedFont {
    font: Font,
    profile: FontProfile,
}

static EMBEDDED_FONT: OnceLock<Result<EmbeddedFont, String>> = OnceLock::new();

pub(crate) fn font_report() -> Result<FontReport> {
    let embedded = embedded_font()?;
    Ok(FontReport {
        profile_id: embedded.profile.id.clone(),
        font: embedded.profile.font.clone(),
        font_sha256: embedded.profile.font_sha256.clone(),
        font_version: embedded.profile.font_version.clone(),
        license: embedded.profile.license.clone(),
        source: embedded.profile.source.clone(),
        upstream_revision: embedded.profile.upstream_revision.clone(),
    })
}

pub(crate) fn rasterize_character(character: char) -> Result<[u8; GLYPH_BYTES]> {
    if character == ' ' || character == '\u{3000}' {
        return Ok([0; GLYPH_BYTES]);
    }
    embedded_font()?.rasterize(character)
}

fn embedded_font() -> Result<&'static EmbeddedFont> {
    match EMBEDDED_FONT.get_or_init(|| EmbeddedFont::load().map_err(|error| format!("{error:#}"))) {
        Ok(font) => Ok(font),
        Err(error) => bail!("load embedded font: {error}"),
    }
}

impl EmbeddedFont {
    fn load() -> Result<Self> {
        let profile: FontProfile =
            serde_json::from_str(PROFILE_JSON).context("parse embedded font profile")?;
        let font_bytes = local_input::read(FONT_PATH)?;
        verify_profile(&profile, &font_bytes)?;
        let font = Font::from_bytes(font_bytes.as_slice(), FontSettings::default())
            .map_err(|error| anyhow::anyhow!("parse embedded font: {error}"))?;
        Ok(Self { font, profile })
    }

    fn rasterize(&self, character: char) -> Result<[u8; GLYPH_BYTES]> {
        ensure!(
            self.font.has_glyph(character),
            "embedded font has no glyph for {character:?} (U+{:04X})",
            character as u32
        );
        let (metrics, coverage) = self
            .font
            .rasterize(character, f32::from(self.profile.font_size));
        ensure!(
            metrics.width > 0 && metrics.height > 0 && !coverage.is_empty(),
            "embedded font rendered no pixels for {character:?}"
        );
        ensure!(
            metrics.width <= GLYPH_WIDTH && metrics.height <= GLYPH_HEIGHT,
            "glyph {character:?} is {}x{} and does not fit {GLYPH_WIDTH}x{GLYPH_HEIGHT}",
            metrics.width,
            metrics.height
        );

        let left = (GLYPH_WIDTH - metrics.width) / 2;
        let baseline_top =
            i32::from(self.profile.baseline_y) - (metrics.ymin + metrics.height as i32);
        let top = baseline_top.clamp(0, (GLYPH_HEIGHT - metrics.height) as i32) as usize;
        let mut bitmap = [0_u8; GLYPH_BYTES];
        for source_y in 0..metrics.height {
            for source_x in 0..metrics.width {
                if coverage[source_y * metrics.width + source_x] < self.profile.threshold {
                    continue;
                }
                let x = left + source_x;
                let y = top + source_y;
                bitmap[y * 2 + x / 8] |= 1 << (7 - x % 8);
            }
        }
        ensure!(
            bitmap.iter().any(|byte| *byte != 0),
            "embedded font rendered an empty bitmap for {character:?}"
        );
        Ok(bitmap)
    }
}

fn verify_profile(profile: &FontProfile, font_bytes: &[u8]) -> Result<()> {
    ensure!(
        profile.schema == PROFILE_SCHEMA,
        "unsupported embedded font profile schema {:?}",
        profile.schema
    );
    ensure!(
        profile.font == "Galmuri14.ttf",
        "embedded font profile names an unexpected font"
    );
    ensure!(
        profile.font_sha256 == sha256_hex(font_bytes),
        "embedded font SHA-256 does not match its profile"
    );
    ensure!(
        profile.license == "Galmuri-OFL.txt",
        "embedded font profile names an unexpected license"
    );
    ensure!(
        profile.rasterizer == "fontdue 0.9.3",
        "embedded font profile names an unexpected rasterizer"
    );
    ensure!(
        profile.vertical_fit == "baseline_then_clamp",
        "embedded font profile names an unexpected vertical-fit policy"
    );
    ensure!(
        (1..=32).contains(&profile.font_size),
        "embedded font size is outside 1..=32"
    );
    ensure!(
        usize::from(profile.baseline_y) <= GLYPH_HEIGHT,
        "embedded font baseline is outside the glyph cell"
    );
    ensure!(
        profile.threshold > 0,
        "embedded font threshold must be nonzero"
    );
    ensure!(
        !profile.font_version.is_empty()
            && !profile.source.is_empty()
            && !profile.upstream_revision.is_empty(),
        "embedded font profile has incomplete provenance"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn embedded_font_matches_its_profile_and_renders_hangul() {
        assert_eq!(
            font_report().unwrap().font_sha256,
            "6fe6c3fe4369e3837ac348431e8670733d67aa4bd550982baa72cc93c81a1c68"
        );
        assert!(
            rasterize_character('가')
                .unwrap()
                .iter()
                .any(|byte| *byte != 0)
        );
    }
}
