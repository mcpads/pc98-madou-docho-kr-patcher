use std::fs;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::external_text::read_external_text_catalog;
use crate::gameplay_text::read_gameplay_text_catalog;
use crate::graphic_text::{GraphicTextCatalog, GraphicTextDisposition};
use crate::local_input;
use crate::main_text::{MainTextCandidateState, read_main_text_catalog};

const RELEASE_STATUS: &str = "distribution_eligible";
const UNTRANSLATED_STATUS: &str = "untranslated";

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct TranslationTestInputReport {
    pub artifact_status: String,
    pub translation_status: String,
    pub distribution_scope: String,
    pub main_entry_count: usize,
    pub gameplay_entry_count: usize,
    pub graphic_target_count: usize,
    pub opening_entry_count: usize,
    pub external_entry_count: usize,
}

pub type ReleaseInputReport = TranslationTestInputReport;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpeningGraphicDraft {
    schema: String,
    status: String,
    entries: Vec<OpeningGraphicEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpeningGraphicEntry {
    x: usize,
    y: usize,
    text: String,
}

#[derive(Debug, Deserialize)]
struct TitleArtApproval {
    status: String,
}

pub fn audit_translation_test_inputs(
    main_catalog_path: &Path,
    gameplay_catalog_path: &Path,
    opening_graphic_path: &Path,
    graphic_text_catalog_path: &Path,
    external_text_catalog_path: &Path,
) -> Result<TranslationTestInputReport> {
    audit_complete_inputs(
        main_catalog_path,
        gameplay_catalog_path,
        opening_graphic_path,
        graphic_text_catalog_path,
        external_text_catalog_path,
        RELEASE_STATUS,
        "test_only",
        "identified_testers",
    )
}

pub fn audit_release_inputs(
    main_catalog_path: &Path,
    gameplay_catalog_path: &Path,
    opening_graphic_path: &Path,
    graphic_text_catalog_path: &Path,
    external_text_catalog_path: &Path,
) -> Result<ReleaseInputReport> {
    let report = audit_complete_inputs(
        main_catalog_path,
        gameplay_catalog_path,
        opening_graphic_path,
        graphic_text_catalog_path,
        external_text_catalog_path,
        RELEASE_STATUS,
        "release",
        "public_bps",
    )?;
    let title_art: TitleArtApproval =
        serde_json::from_str(&local_input::read_to_string("graphics/title-art.json")?)
            .context("failed to parse title-art approval")?;
    ensure!(
        title_art.status == RELEASE_STATUS,
        "title art must have status {RELEASE_STATUS:?} for a release"
    );
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn audit_complete_inputs(
    main_catalog_path: &Path,
    gameplay_catalog_path: &Path,
    opening_graphic_path: &Path,
    graphic_text_catalog_path: &Path,
    external_text_catalog_path: &Path,
    required_status: &str,
    artifact_status: &str,
    distribution_scope: &str,
) -> Result<TranslationTestInputReport> {
    let main = read_main_text_catalog(main_catalog_path)?;
    ensure!(
        main.unresolved_candidate_count == 0,
        "translation test image requires zero unresolved MAIN.OVL candidates"
    );
    for candidate in &main.diagnostic_candidates {
        ensure!(
            candidate.state != MainTextCandidateState::Unresolved,
            "{} is still an unresolved MAIN.OVL diagnostic candidate",
            candidate.id
        );
        ensure!(
            candidate.ko.is_empty() && candidate.status == UNTRANSLATED_STATUS,
            "{} is diagnostic evidence and must not become translation input",
            candidate.id
        );
    }
    for entry in &main.entries {
        require_complete(&entry.id, &entry.ko, &entry.status, required_status)?;
    }

    let gameplay = read_gameplay_text_catalog(gameplay_catalog_path)?;
    ensure!(
        gameplay.unresolved_entry_count == 0,
        "translation test image requires zero unresolved gameplay entries"
    );
    for resource in &gameplay.resources {
        ensure!(
            resource.unresolved_entry_count == 0,
            "{} still has unresolved gameplay entries",
            resource.name
        );
        for entry in &resource.entries {
            require_complete(&entry.id, &entry.ko, &entry.status, required_status)?;
        }
    }

    let graphics: GraphicTextCatalog = read_json(
        graphic_text_catalog_path,
        "graphic-text translation catalog",
    )?;
    let mut graphic_target_count = 0;
    for resource in &graphics.resources {
        for surface in &resource.surfaces {
            match surface.disposition {
                GraphicTextDisposition::Target => {
                    require_complete(&surface.id, &surface.ko, &surface.status, required_status)?;
                    graphic_target_count += 1;
                }
                GraphicTextDisposition::Excluded => ensure!(
                    surface.ko.is_empty() && surface.status == "excluded",
                    "{} is an excluded graphic surface and must remain untranslated",
                    surface.id
                ),
            }
        }
    }

    let opening: OpeningGraphicDraft =
        read_json(opening_graphic_path, "opening graphic translation draft")?;
    ensure!(
        opening.schema == "pc98_madou_docho.opening_graphic_draft",
        "unsupported opening graphic draft schema {:?}",
        opening.schema
    );
    ensure!(
        opening.status == required_status,
        "opening graphic input requires status {required_status:?}"
    );
    for (index, entry) in opening.entries.iter().enumerate() {
        let id = format!("opening entry {index} at ({}, {})", entry.x, entry.y);
        require_complete(&id, &entry.text, &opening.status, required_status)?;
    }

    let external = read_external_text_catalog(external_text_catalog_path)?;
    ensure!(
        external.unresolved_program_count == 0 && external.unresolved_programs.is_empty(),
        "translation test image requires zero unresolved external programs"
    );
    for program in &external.programs {
        for entry in &program.entries {
            require_complete(&entry.id, &entry.ko, &entry.status, required_status)?;
        }
    }

    Ok(TranslationTestInputReport {
        artifact_status: artifact_status.to_owned(),
        translation_status: required_status.to_owned(),
        distribution_scope: distribution_scope.to_owned(),
        main_entry_count: main.entries.len(),
        gameplay_entry_count: gameplay
            .resources
            .iter()
            .map(|resource| resource.entries.len())
            .sum(),
        graphic_target_count,
        opening_entry_count: opening.entries.len(),
        external_entry_count: external
            .programs
            .iter()
            .map(|program| program.entries.len())
            .sum(),
    })
}

fn read_json<T: DeserializeOwned>(path: &Path, role: &str) -> Result<T> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read {role}: {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {role}: {}", path.display()))
}

fn require_complete(id: &str, korean: &str, status: &str, required_status: &str) -> Result<()> {
    ensure!(
        status == required_status,
        "{id} must have status {required_status:?}, got {status:?}"
    );
    ensure!(!korean.is_empty(), "{id} has no Korean text");
    ensure!(
        !contains_japanese_script(korean),
        "{id} Korean text still contains Japanese script"
    );
    Ok(())
}

fn contains_japanese_script(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            character,
            '\u{3041}'..='\u{3096}'
                | '\u{30a1}'..='\u{30fa}'
                | '\u{30fd}'..='\u{30ff}'
                | '\u{3400}'..='\u{4dbf}'
                | '\u{4e00}'..='\u{9fff}'
                | '\u{ff66}'..='\u{ff9d}'
        )
    })
}
