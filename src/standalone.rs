use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use fatfs::{FatType, FileSystem, FsOptions};
use serde::Serialize;

use crate::compile_lz::ExactCompileLzReport;
use crate::external_text::{
    ExternalTextBuildReport, apply_external_main_com_draft, apply_external_source_file_drafts,
    validate_external_text_catalog,
};
use crate::game_data::{
    PayloadFilenameReference, exact_compile_lz_report, find_payload_filename_references,
};
use crate::gameplay_text::{
    GameplayTextRebuildReport, apply_gameplay_text_drafts, gameplay_text_hangul_characters,
};
use crate::graphic_text::{GraphicTextRebuildReport, apply_graphic_text_drafts};
use crate::lha_sfx::extract_lha_sfx;
use crate::local_input;
use crate::main_text::{
    MainOverlayRebuildReport, apply_main_text_draft, apply_main_text_renderer_sheet_draft,
    main_text_hangul_characters,
};
use crate::official_patch::{OfficialFreezeFixReport, apply_official_freeze_fix};
use crate::opening_graphic::{OpeningGraphicPatchReport, apply_opening_graphic_draft};
use crate::renderer_font::{
    RendererFontReport, build_renderer_font_plan, install_renderer_font,
    install_renderer_font_with_bios_gaiji,
};
use crate::sample_bank::{SampleBankReport, inspect_sample_banks};
use crate::source_disk::{
    BOOT_SECTOR_SIZE, SYSTEM_FILES, TOOL_FILES, VerifiedSource, fatfs_mount_copy,
    load_verified_source, sha256_hex,
};
use crate::translation_test::{
    ReleaseInputReport, TranslationTestInputReport, audit_release_inputs,
    audit_translation_test_inputs,
};

const JAPANESE_DATA_ARCHIVE_NAME: &str = "%93%B9%91%90%88%D9%95%B7.DAT";
const JAPANESE_DATA_PLACEHOLDER: &str = "DOCHOIBN.DAT";
const JAPANESE_DATA_RAW_NAME: [u8; 11] = [
    0x93, 0xb9, 0x91, 0x90, 0x88, 0xd9, 0x95, 0xb7, b'D', b'A', b'T',
];
/// Translation inputs read from `translations/` under the local input directory.
const TRANSLATION_CATALOG_FILES: &[&str] = &[
    "main/catalog.json",
    "main/entries/player-health-status.json",
    "main/entries/magic-power-status.json",
    "main/entries/health-recovery.json",
    "main/entries/magic-power-recovery.json",
    "main/entries/player-damage-reactions.json",
    "main/entries/enemy-condition.json",
    "main/entries/enemy-damage-reactions.json",
    "main/entries/in-game-menus-and-prompts.json",
    "main/entries/disk-and-format-errors.json",
    "main/entries/floor-labels-and-spell-names.json",
    "main/entries/consumable-and-equipment-names.json",
    "main/entries/progression-item-and-scenario-names.json",
    "main/entries/boxes-transport-and-library.json",
    "main/entries/fixed-dungeon-events-and-guards.json",
    "main/entries/shop-messages.json",
    "main/entries/spell-and-status-effects.json",
    "main/entries/recovery-and-progression-item-effects.json",
    "main/entries/companion-and-progression-item-actions.json",
    "main/entries/ailments-and-environment.json",
    "main/entries/encounters-and-battle-results.json",
    "gameplay/catalog.json",
    "gameplay/resources/s01.json",
    "gameplay/resources/s02.json",
    "gameplay/resources/enemy01.json",
    "gameplay/resources/enemy02.json",
    "gameplay/resources/enemy03.json",
    "gameplay/resources/enemy04.json",
    "gameplay/resources/enemy05.json",
    "gameplay/resources/enemy06.json",
    "gameplay/resources/enemy07.json",
    "gameplay/resources/enemy08.json",
    "gameplay/resources/enemy09.json",
    "gameplay/resources/enemy0a.json",
    "gameplay/resources/enemy0b.json",
    "gameplay/resources/enemy0c.json",
    "gameplay/resources/enemy0d.json",
    "gameplay/resources/enemy0e.json",
    "opening-graphic.json",
    "graphic-text.json",
    "external/catalog.json",
    "external/programs/bplay.json",
    "external/programs/bsamp.json",
    "external/programs/dsh.json",
    "external/programs/fplay.json",
    "external/programs/main.json",
    "external/programs/megdos.json",
    "external/programs/menu.json",
];

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct PayloadFileReport {
    pub name: String,
    pub size: usize,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_compile_lz: Option<ExactCompileLzReport>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct InputSurvey {
    pub source: crate::source_disk::SourceReport,
    pub official_freeze_fix: OfficialFreezeFixReport,
    pub installer_payload: Vec<PayloadFileReport>,
    pub filename_references: Vec<PayloadFilenameReference>,
    pub sample_banks: Vec<SampleBankReport>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct BuildReport {
    pub source_profile: String,
    pub source_sha256: String,
    pub official_freeze_fix: OfficialFreezeFixReport,
    pub output_sha256: String,
    pub output_size: usize,
    pub verified_file_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct OpeningGraphicPocBuildReport {
    pub opening_graphic: OpeningGraphicPatchReport,
    pub standalone: BuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct MainTextDevelopmentBuildReport {
    pub main_overlay: MainOverlayRebuildReport,
    pub standalone: BuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GameplayTextDevelopmentBuildReport {
    pub main_overlay: MainOverlayRebuildReport,
    pub gameplay_text: GameplayTextRebuildReport,
    pub renderer_font: RendererFontReport,
    pub standalone: BuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphicTextDevelopmentBuildReport {
    pub graphic_text: GraphicTextRebuildReport,
    pub standalone: BuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExternalTextDevelopmentBuildReport {
    pub external_text: ExternalTextBuildReport,
    pub standalone: BuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct LocalizationDevelopmentBuildReport {
    pub opening_graphic: OpeningGraphicPatchReport,
    pub main_overlay: MainOverlayRebuildReport,
    pub gameplay_text: GameplayTextRebuildReport,
    pub renderer_font: RendererFontReport,
    pub graphic_text: GraphicTextRebuildReport,
    pub external_text: ExternalTextBuildReport,
    pub standalone: BuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct TranslationTestImageBuildReport {
    pub readiness: TranslationTestInputReport,
    pub localization: LocalizationDevelopmentBuildReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub(crate) struct ReleaseImageBuildReport {
    pub readiness: ReleaseInputReport,
    pub localization: LocalizationDevelopmentBuildReport,
}

struct PreparedStandalone {
    source_path: PathBuf,
    source: VerifiedSource,
    original_system_files: BTreeMap<String, Vec<u8>>,
    official_freeze_fix: OfficialFreezeFixReport,
    payload: BTreeMap<String, Vec<u8>>,
}

pub fn survey_inputs(source_path: &Path) -> Result<InputSurvey> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    let original_bsamp = payload
        .get("BSAMP.COM")
        .context("MADOU.EXE payload is missing BSAMP.COM")?;
    let official_freeze_fix = apply_official_freeze_fix(original_bsamp)?.report;
    let filename_references = find_payload_filename_references(&payload)?;
    let sample_banks = inspect_sample_banks(&payload)?;
    let installer_payload = payload
        .iter()
        .map(|(name, bytes)| PayloadFileReport {
            name: name.clone(),
            size: bytes.len(),
            sha256: sha256_hex(bytes),
            exact_compile_lz: exact_compile_lz_report(name, bytes),
        })
        .collect();
    Ok(InputSurvey {
        source: source.report,
        official_freeze_fix,
        installer_payload,
        filename_references,
        sample_banks,
    })
}

pub fn build_standalone_image(source_path: &Path, output_path: &Path) -> Result<BuildReport> {
    let prepared = prepare_standalone(source_path)?;
    write_standalone(prepared, output_path)
}

pub fn build_opening_graphic_poc_image(
    source_path: &Path,
    draft_path: &Path,
    output_path: &Path,
) -> Result<OpeningGraphicPocBuildReport> {
    let mut prepared = prepare_standalone(source_path)?;
    let opening_graphic = apply_opening_graphic_draft(&mut prepared.payload, draft_path)?;
    let standalone = write_standalone(prepared, output_path)?;
    Ok(OpeningGraphicPocBuildReport {
        opening_graphic,
        standalone,
    })
}

pub fn build_main_text_development_image(
    source_path: &Path,
    catalog_path: &Path,
    output_path: &Path,
) -> Result<MainTextDevelopmentBuildReport> {
    let mut prepared = prepare_standalone(source_path)?;
    let main_overlay = apply_main_text_draft(&mut prepared.payload, catalog_path)?;
    ensure!(
        main_overlay.changed_entries + main_overlay.changed_candidate_slots > 0,
        "MAIN.OVL development image requires at least one Korean draft text slot"
    );
    let standalone = write_standalone(prepared, output_path)?;
    Ok(MainTextDevelopmentBuildReport {
        main_overlay,
        standalone,
    })
}

pub fn build_gameplay_text_development_image(
    source_path: &Path,
    main_catalog_path: &Path,
    gameplay_catalog_path: &Path,
    output_path: &Path,
) -> Result<GameplayTextDevelopmentBuildReport> {
    let mut prepared = prepare_standalone(source_path)?;
    let mut characters = main_text_hangul_characters(main_catalog_path)?;
    characters.extend(gameplay_text_hangul_characters(gameplay_catalog_path)?);
    let renderer_font_plan = build_renderer_font_plan(&characters)?;
    let main_overlay = apply_main_text_renderer_sheet_draft(
        &mut prepared.payload,
        main_catalog_path,
        &renderer_font_plan,
    )?;
    let gameplay_text = apply_gameplay_text_drafts(
        &mut prepared.payload,
        gameplay_catalog_path,
        renderer_font_plan.codebook(),
    )?;
    ensure!(
        gameplay_text.changed_entries > 0,
        "gameplay text development image requires at least one Korean DAT draft"
    );
    let renderer_font = install_renderer_font(&mut prepared.payload, &renderer_font_plan)?;
    let standalone = write_standalone(prepared, output_path)?;
    Ok(GameplayTextDevelopmentBuildReport {
        main_overlay,
        gameplay_text,
        renderer_font,
        standalone,
    })
}

pub fn build_graphic_text_development_image(
    source_path: &Path,
    catalog_path: &Path,
    output_path: &Path,
) -> Result<GraphicTextDevelopmentBuildReport> {
    let mut prepared = prepare_standalone(source_path)?;
    let graphic_text = apply_graphic_text_drafts(&mut prepared.payload, catalog_path)?;
    let standalone = write_standalone(prepared, output_path)?;
    Ok(GraphicTextDevelopmentBuildReport {
        graphic_text,
        standalone,
    })
}

pub fn build_external_text_development_image(
    source_path: &Path,
    catalog_path: &Path,
    output_path: &Path,
) -> Result<ExternalTextDevelopmentBuildReport> {
    validate_external_text_catalog(source_path, catalog_path)?;
    let mut prepared = prepare_standalone(source_path)?;
    let external_text = apply_external_source_file_drafts(
        &mut prepared.source.system_files,
        &mut prepared.source.tool_files,
        &mut prepared.payload,
        catalog_path,
    )?;
    let standalone = write_standalone(prepared, output_path)?;
    Ok(ExternalTextDevelopmentBuildReport {
        external_text,
        standalone,
    })
}

pub fn build_localization_development_image(
    source_path: &Path,
    main_catalog_path: &Path,
    gameplay_catalog_path: &Path,
    opening_graphic_draft_path: &Path,
    graphic_text_catalog_path: &Path,
    external_text_catalog_path: &Path,
    output_path: &Path,
) -> Result<LocalizationDevelopmentBuildReport> {
    validate_external_text_catalog(source_path, external_text_catalog_path)?;
    let mut prepared = prepare_standalone(source_path)?;
    let opening_graphic =
        apply_opening_graphic_draft(&mut prepared.payload, opening_graphic_draft_path)?;
    let graphic_text = apply_graphic_text_drafts(&mut prepared.payload, graphic_text_catalog_path)?;
    let mut external_text = apply_external_source_file_drafts(
        &mut prepared.source.system_files,
        &mut prepared.source.tool_files,
        &mut prepared.payload,
        external_text_catalog_path,
    )?;
    let external_main =
        apply_external_main_com_draft(&mut prepared.payload, external_text_catalog_path)?;
    let mut characters = main_text_hangul_characters(main_catalog_path)?;
    characters.extend(gameplay_text_hangul_characters(gameplay_catalog_path)?);
    let renderer_font_plan = build_renderer_font_plan(&characters)?;
    let main_overlay = apply_main_text_renderer_sheet_draft(
        &mut prepared.payload,
        main_catalog_path,
        &renderer_font_plan,
    )?;
    let gameplay_text = apply_gameplay_text_drafts(
        &mut prepared.payload,
        gameplay_catalog_path,
        renderer_font_plan.codebook(),
    )?;
    ensure!(
        gameplay_text.changed_entries > 0,
        "localization development image requires at least one Korean gameplay DAT draft"
    );
    let renderer_font = install_renderer_font_with_bios_gaiji(
        &mut prepared.payload,
        &renderer_font_plan,
        external_main.bios_gaiji(),
        external_main.loader_preimage_sha256(),
    )?;
    let final_main_com = prepared
        .payload
        .get("MAIN.COM")
        .context("combined localization payload is missing MAIN.COM")?;
    let external_main =
        external_main.finish(final_main_com, renderer_font.main_com_loader_logical_offset);
    external_text.include_main_com(external_main)?;
    let standalone = write_standalone(prepared, output_path)?;
    Ok(LocalizationDevelopmentBuildReport {
        opening_graphic,
        main_overlay,
        gameplay_text,
        renderer_font,
        graphic_text,
        external_text,
        standalone,
    })
}

pub fn build_translation_test_image(
    source_path: &Path,
    main_catalog_path: &Path,
    gameplay_catalog_path: &Path,
    opening_graphic_draft_path: &Path,
    graphic_text_catalog_path: &Path,
    external_text_catalog_path: &Path,
    output_path: &Path,
) -> Result<TranslationTestImageBuildReport> {
    let readiness = audit_translation_test_inputs(
        main_catalog_path,
        gameplay_catalog_path,
        opening_graphic_draft_path,
        graphic_text_catalog_path,
        external_text_catalog_path,
    )?;
    let localization = build_localization_development_image(
        source_path,
        main_catalog_path,
        gameplay_catalog_path,
        opening_graphic_draft_path,
        graphic_text_catalog_path,
        external_text_catalog_path,
        output_path,
    )?;
    Ok(TranslationTestImageBuildReport {
        readiness,
        localization,
    })
}

pub fn build_bundled_translation_test_image(
    source_path: &Path,
    output_path: &Path,
) -> Result<TranslationTestImageBuildReport> {
    let catalog_directory = stage_bundled_translation_catalogs()?;
    build_translation_test_image(
        source_path,
        &catalog_directory.path().join("main"),
        &catalog_directory.path().join("gameplay"),
        &catalog_directory.path().join("opening-graphic.json"),
        &catalog_directory.path().join("graphic-text.json"),
        &catalog_directory.path().join("external"),
        output_path,
    )
}

pub(crate) fn build_bundled_release_image(
    source_path: &Path,
    output_path: &Path,
) -> Result<ReleaseImageBuildReport> {
    let catalog_directory = stage_bundled_translation_catalogs()?;
    let main = catalog_directory.path().join("main");
    let gameplay = catalog_directory.path().join("gameplay");
    let opening = catalog_directory.path().join("opening-graphic.json");
    let graphics = catalog_directory.path().join("graphic-text.json");
    let external = catalog_directory.path().join("external");
    let readiness = audit_release_inputs(&main, &gameplay, &opening, &graphics, &external)?;
    let localization = build_localization_development_image(
        source_path,
        &main,
        &gameplay,
        &opening,
        &graphics,
        &external,
        output_path,
    )?;
    Ok(ReleaseImageBuildReport {
        readiness,
        localization,
    })
}

fn stage_bundled_translation_catalogs() -> Result<tempfile::TempDir> {
    let catalog_directory = tempfile::tempdir()
        .context("failed to create temporary directory for bundled translation catalogs")?;
    for &name in TRANSLATION_CATALOG_FILES {
        let contents = local_input::read(&format!("translations/{name}"))?;
        let path = catalog_directory.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create bundled translation catalog directory: {}",
                    parent.display()
                )
            })?;
        }
        fs::write(&path, contents).with_context(|| {
            format!(
                "failed to stage bundled translation catalog: {}",
                path.display()
            )
        })?;
    }
    Ok(catalog_directory)
}

pub(crate) fn transplant_localized_files(
    source_image: &[u8],
    localized_image: &[u8],
) -> Result<(Vec<u8>, usize)> {
    crate::source_disk::validate_pc98_fat12_geometry(source_image)?;
    crate::source_disk::validate_pc98_fat12_geometry(localized_image)?;
    ensure!(
        source_image.len() == localized_image.len(),
        "source and localized FAT12 images have different sizes"
    );

    let source_names = raw_root_file_names(source_image)?;
    let localized_names = raw_root_file_names(localized_image)?;
    let mut expected_names = source_names.clone();
    ensure!(
        expected_names.insert(ascii_raw_name("KFONT.BIN")?),
        "source FDI unexpectedly already contains KFONT.BIN"
    );
    ensure!(
        localized_names == expected_names,
        "localized file set is not the source FDI file set plus KFONT.BIN"
    );

    let source_japanese = read_raw_root_file(source_image, &JAPANESE_DATA_RAW_NAME)?;
    let localized_japanese = read_raw_root_file(localized_image, &JAPANESE_DATA_RAW_NAME)?;
    ensure!(
        source_japanese == localized_japanese,
        "localized build changed the original Japanese data archive"
    );

    struct Replacement {
        name: String,
        bytes: Vec<u8>,
        source_len: usize,
    }
    let mut replacements = Vec::new();
    for raw_name in &localized_names {
        if raw_name == &JAPANESE_DATA_RAW_NAME {
            continue;
        }
        let name = ascii_name_from_raw(raw_name)?;
        let bytes = read_raw_root_file(localized_image, raw_name)?;
        let source_bytes = if source_names.contains(raw_name) {
            read_raw_root_file(source_image, raw_name)?
        } else {
            Vec::new()
        };
        if source_bytes != bytes {
            replacements.push(Replacement {
                name,
                bytes,
                source_len: source_bytes.len(),
            });
        }
    }
    replacements.sort_by_key(|replacement| replacement.bytes.len() > replacement.source_len);

    let mut candidate = fatfs_mount_copy(source_image)?;
    {
        let filesystem = FileSystem::new(Cursor::new(candidate.as_mut_slice()), FsOptions::new())
            .context("failed to mount source FDI FAT12 payload")?;
        ensure!(
            matches!(filesystem.fat_type(), FatType::Fat12),
            "source FDI payload is not FAT12"
        );
        let root = filesystem.root_dir();
        for replacement in &replacements {
            write_file(&root, &replacement.name, &replacement.bytes)?;
        }
        drop(root);
        filesystem
            .unmount()
            .context("failed to unmount localized FDI FAT12 payload")?;
    }
    candidate[..BOOT_SECTOR_SIZE].copy_from_slice(&source_image[..BOOT_SECTOR_SIZE]);
    verify_fat_mirrors(&candidate)?;
    ensure!(
        raw_root_file_names(&candidate)? == localized_names,
        "localized FDI root file set differs from the verified localized build"
    );
    for raw_name in &localized_names {
        ensure!(
            read_raw_root_file(&candidate, raw_name)?
                == read_raw_root_file(localized_image, raw_name)?,
            "localized FDI file differs from the verified localized build: {}",
            raw_name_label(raw_name)
        );
    }
    Ok((candidate, localized_names.len()))
}

fn ascii_name_from_raw(raw_name: &[u8; 11]) -> Result<String> {
    ensure!(
        raw_name.iter().all(u8::is_ascii),
        "non-ASCII FAT name requires an explicit mapping: {}",
        raw_name_label(raw_name)
    );
    let stem = std::str::from_utf8(&raw_name[..8])?.trim_end_matches(' ');
    let extension = std::str::from_utf8(&raw_name[8..])?.trim_end_matches(' ');
    ensure!(!stem.is_empty(), "FAT 8.3 name has an empty stem");
    if extension.is_empty() {
        Ok(stem.to_owned())
    } else {
        Ok(format!("{stem}.{extension}"))
    }
}

fn prepare_standalone(source_path: &Path) -> Result<PreparedStandalone> {
    let source = load_verified_source(source_path)?;
    let original_system_files = source.system_files.clone();
    let mut payload = extract_lha_sfx(&source.installer)?;
    let original_bsamp = payload
        .get("BSAMP.COM")
        .context("MADOU.EXE payload is missing BSAMP.COM")?;
    let applied_freeze_fix = apply_official_freeze_fix(original_bsamp)?;
    payload.insert("BSAMP.COM".to_owned(), applied_freeze_fix.updated_bsamp);

    Ok(PreparedStandalone {
        source_path: source_path.to_owned(),
        source,
        original_system_files,
        official_freeze_fix: applied_freeze_fix.report,
        payload,
    })
}

fn write_standalone(prepared: PreparedStandalone, output_path: &Path) -> Result<BuildReport> {
    let PreparedStandalone {
        source_path,
        source,
        original_system_files,
        official_freeze_fix,
        payload,
    } = prepared;

    verify_writer_ownership(&source, &payload)?;
    let candidate = assemble_image(&source, &original_system_files, &payload)?;
    let verified_file_count = verify_image(&source, &candidate, &payload)?;
    ensure_output_does_not_alias_source(&source_path, output_path)?;
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create standalone image directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(output_path, &candidate).with_context(|| {
        format!(
            "failed to write standalone image: {}",
            output_path.display()
        )
    })?;

    Ok(BuildReport {
        source_profile: source.report.profile,
        source_sha256: source.report.sha256,
        official_freeze_fix,
        output_sha256: sha256_hex(&candidate),
        output_size: candidate.len(),
        verified_file_count,
    })
}

fn ensure_output_does_not_alias_source(source_path: &Path, output_path: &Path) -> Result<()> {
    if output_path
        .try_exists()
        .with_context(|| format!("failed to inspect output path: {}", output_path.display()))?
    {
        ensure!(
            !same_file::is_same_file(source_path, output_path).with_context(|| format!(
                "failed to compare source and output paths: {} and {}",
                source_path.display(),
                output_path.display()
            ))?,
            "output path aliases the verified source image; refusing to overwrite source: {}",
            source_path.display()
        );
    }
    Ok(())
}

fn verify_writer_ownership(
    source: &VerifiedSource,
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let system: BTreeSet<_> = source.system_files.keys().map(String::as_str).collect();
    let tools: BTreeSet<_> = source.tool_files.keys().map(String::as_str).collect();
    let payload_names: BTreeSet<_> = payload.keys().map(String::as_str).collect();
    ensure!(
        system.is_disjoint(&tools),
        "system and tool writers overlap"
    );
    ensure!(
        system.is_disjoint(&payload_names),
        "system and payload writers overlap"
    );
    ensure!(
        tools.is_disjoint(&payload_names),
        "tool and payload writers overlap"
    );
    Ok(())
}

fn assemble_image(
    source: &VerifiedSource,
    original_system_files: &BTreeMap<String, Vec<u8>>,
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    ensure!(
        source.system_files.keys().eq(original_system_files.keys()),
        "system-file override set differs from the verified source"
    );
    let mut image = fatfs_mount_copy(&source.image)?;
    {
        let filesystem = FileSystem::new(Cursor::new(image.as_mut_slice()), FsOptions::new())
            .context("failed to mount working FAT12 image")?;
        ensure!(
            matches!(filesystem.fat_type(), FatType::Fat12),
            "working image is not FAT12"
        );
        let root = filesystem.root_dir();
        remove_disc_station_files(&root)?;
        for (name, bytes) in &source.system_files {
            let original = original_system_files
                .get(name)
                .with_context(|| format!("verified source is missing system file {name}"))?;
            if bytes != original {
                write_file(&root, name, bytes)?;
            }
        }
        for (name, bytes) in &source.tool_files {
            write_file(&root, name, bytes)?;
        }
        for (name, bytes) in payload {
            write_file(&root, payload_write_name(name)?, bytes)?;
        }
        drop(root);
        filesystem
            .unmount()
            .context("failed to unmount assembled FAT12 image")?;
    }
    rename_japanese_data_file(&mut image)?;
    image[..BOOT_SECTOR_SIZE].copy_from_slice(&source.image[..BOOT_SECTOR_SIZE]);
    Ok(image)
}

fn verify_image(
    source: &VerifiedSource,
    image: &[u8],
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<usize> {
    ensure!(
        image.len() == source.image.len(),
        "standalone image size changed: expected {}, got {}",
        source.image.len(),
        image.len()
    );
    ensure!(
        image[..BOOT_SECTOR_SIZE] == source.image[..BOOT_SECTOR_SIZE],
        "standalone image changed the PC-98 boot sector"
    );
    verify_fat_mirrors(image)?;

    let filesystem = FileSystem::new(Cursor::new(fatfs_mount_copy(image)?), FsOptions::new())
        .context("failed to remount assembled FAT12 image")?;
    ensure!(
        matches!(filesystem.fat_type(), FatType::Fat12),
        "assembled image did not remount as FAT12"
    );
    let root = filesystem.root_dir();
    let actual_names = raw_root_file_names(image)?;
    let mut expected_names: BTreeSet<[u8; 11]> = SYSTEM_FILES
        .into_iter()
        .chain(TOOL_FILES)
        .map(ascii_raw_name)
        .collect::<Result<_>>()?;
    for name in payload.keys() {
        let raw_name = if name == JAPANESE_DATA_ARCHIVE_NAME {
            JAPANESE_DATA_RAW_NAME
        } else {
            ascii_raw_name(name)?
        };
        ensure!(
            expected_names.insert(raw_name),
            "two producers target the same FAT12 name: {}",
            raw_name_label(&raw_name)
        );
    }
    ensure!(
        actual_names == expected_names,
        "assembled root file set differs: expected {}, got {}",
        raw_name_set_label(&expected_names),
        raw_name_set_label(&actual_names)
    );

    for (name, expected) in &source.system_files {
        verify_file(&root, name, expected)?;
    }
    for (name, expected) in &source.tool_files {
        verify_file(&root, name, expected)?;
    }
    for (name, expected) in payload {
        if name == JAPANESE_DATA_ARCHIVE_NAME {
            let actual = read_raw_root_file(image, &JAPANESE_DATA_RAW_NAME)?;
            ensure!(
                actual == *expected,
                "assembled file differs from its verified producer output: 道草異聞.DAT"
            );
        } else {
            verify_file(&root, name, expected)?;
        }
    }
    Ok(expected_names.len())
}

fn payload_write_name(name: &str) -> Result<&str> {
    if name == JAPANESE_DATA_ARCHIVE_NAME {
        return Ok(JAPANESE_DATA_PLACEHOLDER);
    }
    ascii_raw_name(name)?;
    Ok(name)
}

fn rename_japanese_data_file(image: &mut [u8]) -> Result<()> {
    let placeholder = ascii_raw_name(JAPANESE_DATA_PLACEHOLDER)?;
    let range = root_directory_range(image)?;
    let mut matches = 0;
    for entry in image[range].chunks_exact_mut(32) {
        if entry[..11] == placeholder {
            entry[..11].copy_from_slice(&JAPANESE_DATA_RAW_NAME);
            matches += 1;
        }
    }
    ensure!(
        matches == 1,
        "expected one FAT12 placeholder for 道草異聞.DAT, found {matches}"
    );
    Ok(())
}

fn raw_root_file_names(image: &[u8]) -> Result<BTreeSet<[u8; 11]>> {
    let mut names = BTreeSet::new();
    for entry in image[root_directory_range(image)?].chunks_exact(32) {
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xe5 || entry[11] == 0x0f || entry[11] & 0x08 != 0 {
            continue;
        }
        ensure!(
            entry[11] & 0x10 == 0,
            "assembled root still contains a directory"
        );
        let name: [u8; 11] = entry[..11]
            .try_into()
            .map_err(|_| anyhow::anyhow!("truncated FAT12 root entry"))?;
        ensure!(
            names.insert(name),
            "assembled FAT12 root contains a duplicate name: {}",
            raw_name_label(&name)
        );
    }
    Ok(names)
}

fn read_raw_root_file(image: &[u8], expected_name: &[u8; 11]) -> Result<Vec<u8>> {
    let layout = Fat12Layout::read(image)?;
    let mut matched_entry = None;
    for entry in image[layout.root_offset..layout.data_offset].chunks_exact(32) {
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xe5 || entry[11] == 0x0f || entry[11] & 0x18 != 0 {
            continue;
        }
        if entry[..11] == *expected_name {
            ensure!(matched_entry.is_none(), "duplicate raw FAT12 file entry");
            matched_entry = Some(entry);
        }
    }
    let entry = matched_entry.with_context(|| {
        format!(
            "assembled image is missing raw FAT12 file: {}",
            raw_name_label(expected_name)
        )
    })?;
    let mut cluster = u16::from_le_bytes([entry[26], entry[27]]) as usize;
    let file_size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]) as usize;
    let fat = image
        .get(layout.fat_offset..layout.fat_offset + layout.fat_size)
        .context("FAT12 table lies outside the image")?;
    let mut bytes = Vec::with_capacity(file_size);
    let mut visited = BTreeSet::new();
    while bytes.len() < file_size {
        ensure!(
            cluster >= 2,
            "FAT12 file chain uses reserved cluster {cluster}"
        );
        ensure!(visited.insert(cluster), "FAT12 file chain contains a loop");
        let cluster_offset = layout
            .data_offset
            .checked_add(
                (cluster - 2)
                    .checked_mul(layout.cluster_size)
                    .context("FAT12 cluster offset overflow")?,
            )
            .context("FAT12 data offset overflow")?;
        let cluster_bytes = image
            .get(cluster_offset..cluster_offset + layout.cluster_size)
            .context("FAT12 file cluster lies outside the image")?;
        let remaining = file_size - bytes.len();
        bytes.extend_from_slice(&cluster_bytes[..remaining.min(layout.cluster_size)]);
        let next = fat12_entry(fat, cluster)?;
        if bytes.len() < file_size {
            ensure!(
                next < 0xff8,
                "FAT12 file chain ended before the declared size"
            );
        }
        cluster = next;
    }
    Ok(bytes)
}

struct Fat12Layout {
    fat_offset: usize,
    fat_size: usize,
    root_offset: usize,
    data_offset: usize,
    cluster_size: usize,
}

impl Fat12Layout {
    fn read(image: &[u8]) -> Result<Self> {
        let u16_at = |offset: usize| -> Result<usize> {
            let bytes = image
                .get(offset..offset + 2)
                .context("truncated FAT12 BPB")?;
            Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
        };
        let bytes_per_sector = u16_at(11)?;
        let sectors_per_cluster = *image.get(13).context("truncated FAT12 BPB")? as usize;
        let reserved_sectors = u16_at(14)?;
        let fat_count = *image.get(16).context("truncated FAT12 BPB")? as usize;
        let root_entries = u16_at(17)?;
        let sectors_per_fat = u16_at(22)?;
        let fat_offset = reserved_sectors
            .checked_mul(bytes_per_sector)
            .context("FAT12 FAT offset overflow")?;
        let fat_size = sectors_per_fat
            .checked_mul(bytes_per_sector)
            .context("FAT12 FAT size overflow")?;
        let root_offset = fat_offset
            .checked_add(
                fat_count
                    .checked_mul(fat_size)
                    .context("FAT12 root offset overflow")?,
            )
            .context("FAT12 root offset overflow")?;
        let root_size = root_entries
            .checked_mul(32)
            .and_then(|bytes| bytes.checked_add(bytes_per_sector - 1))
            .map(|bytes| bytes / bytes_per_sector * bytes_per_sector)
            .context("FAT12 root size overflow")?;
        let data_offset = root_offset
            .checked_add(root_size)
            .context("FAT12 data offset overflow")?;
        let cluster_size = sectors_per_cluster
            .checked_mul(bytes_per_sector)
            .context("FAT12 cluster size overflow")?;
        ensure!(
            data_offset <= image.len(),
            "FAT12 data area lies outside image"
        );
        Ok(Self {
            fat_offset,
            fat_size,
            root_offset,
            data_offset,
            cluster_size,
        })
    }
}

fn root_directory_range(image: &[u8]) -> Result<std::ops::Range<usize>> {
    let layout = Fat12Layout::read(image)?;
    Ok(layout.root_offset..layout.data_offset)
}

fn fat12_entry(fat: &[u8], cluster: usize) -> Result<usize> {
    let offset = cluster
        .checked_add(cluster / 2)
        .context("FAT12 entry offset overflow")?;
    let bytes = fat
        .get(offset..offset + 2)
        .context("FAT12 entry lies outside table")?;
    let packed = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    Ok(if cluster & 1 == 0 {
        packed & 0x0fff
    } else {
        packed >> 4
    })
}

fn ascii_raw_name(name: &str) -> Result<[u8; 11]> {
    ensure!(
        name.is_ascii(),
        "non-ASCII FAT name requires an explicit raw mapping: {name}"
    );
    let (stem, extension) = name
        .split_once('.')
        .map_or((name, ""), |(stem, extension)| (stem, extension));
    ensure!(
        !stem.is_empty() && stem.len() <= 8,
        "invalid FAT 8.3 stem: {name}"
    );
    ensure!(extension.len() <= 3, "invalid FAT 8.3 extension: {name}");
    ensure!(
        !extension.contains('.'),
        "FAT 8.3 name contains multiple periods: {name}"
    );
    let mut raw = [b' '; 11];
    raw[..stem.len()].copy_from_slice(stem.to_ascii_uppercase().as_bytes());
    raw[8..8 + extension.len()].copy_from_slice(extension.to_ascii_uppercase().as_bytes());
    Ok(raw)
}

fn raw_name_label(name: &[u8; 11]) -> String {
    name.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn raw_name_set_label(names: &BTreeSet<[u8; 11]>) -> String {
    names
        .iter()
        .map(raw_name_label)
        .collect::<Vec<_>>()
        .join(",")
}

fn remove_disc_station_files<T: fatfs::ReadWriteSeek>(root: &fatfs::Dir<'_, T>) -> Result<()> {
    let retained: BTreeSet<_> = SYSTEM_FILES.into_iter().collect();
    let entries: Vec<(String, bool)> = root
        .iter()
        .map(|entry| entry.map(|entry| (entry.file_name(), entry.is_dir())))
        .collect::<std::io::Result<_>>()
        .context("failed to enumerate source FAT12 root")?;
    for (name, is_dir) in entries {
        if retained.contains(name.to_ascii_uppercase().as_str()) {
            continue;
        }
        if is_dir {
            let directory = root
                .open_dir(&name)
                .with_context(|| format!("failed to open source directory: {name}"))?;
            clear_directory(&directory)?;
            drop(directory);
        }
        root.remove(&name)
            .with_context(|| format!("failed to remove Disc Station entry: {name}"))?;
    }
    Ok(())
}

fn clear_directory<T: fatfs::ReadWriteSeek>(directory: &fatfs::Dir<'_, T>) -> Result<()> {
    let entries: Vec<(String, bool)> = directory
        .iter()
        .map(|entry| entry.map(|entry| (entry.file_name(), entry.is_dir())))
        .collect::<std::io::Result<Vec<(String, bool)>>>()
        .context("failed to enumerate source directory")?
        .into_iter()
        .filter(|(name, _)| name != "." && name != "..")
        .collect();
    for (name, is_dir) in entries {
        if is_dir {
            let child = directory
                .open_dir(&name)
                .with_context(|| format!("failed to open nested source directory: {name}"))?;
            clear_directory(&child)?;
            drop(child);
        }
        directory
            .remove(&name)
            .with_context(|| format!("failed to remove nested Disc Station entry: {name}"))?;
    }
    Ok(())
}

fn write_file<T: fatfs::ReadWriteSeek>(
    root: &fatfs::Dir<'_, T>,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut file = root
        .create_file(name)
        .with_context(|| format!("failed to create FAT12 file: {name}"))?;
    file.truncate()
        .with_context(|| format!("failed to truncate FAT12 file: {name}"))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write FAT12 file: {name}"))?;
    Ok(())
}

fn read_file<T: fatfs::ReadWriteSeek>(root: &fatfs::Dir<'_, T>, name: &str) -> Result<Vec<u8>> {
    let mut file = root
        .open_file(name)
        .with_context(|| format!("assembled image is missing file: {name}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to read assembled file: {name}"))?;
    Ok(bytes)
}

fn verify_file<T: fatfs::ReadWriteSeek>(
    root: &fatfs::Dir<'_, T>,
    name: &str,
    expected: &[u8],
) -> Result<()> {
    let actual = read_file(root, name)?;
    ensure!(
        actual == expected,
        "assembled file differs from its verified producer output: {name}"
    );
    Ok(())
}

fn verify_fat_mirrors(image: &[u8]) -> Result<()> {
    let bytes_per_sector = u16::from_le_bytes([image[11], image[12]]) as usize;
    let reserved_sectors = u16::from_le_bytes([image[14], image[15]]) as usize;
    let fat_count = image[16] as usize;
    let sectors_per_fat = u16::from_le_bytes([image[22], image[23]]) as usize;
    ensure!(
        fat_count == 2,
        "assembled image does not have two FAT copies"
    );
    let fat_size = bytes_per_sector
        .checked_mul(sectors_per_fat)
        .context("FAT byte size overflow")?;
    let first_offset = bytes_per_sector
        .checked_mul(reserved_sectors)
        .context("FAT offset overflow")?;
    let second_offset = first_offset
        .checked_add(fat_size)
        .context("second FAT offset overflow")?;
    let first = image
        .get(first_offset..first_offset + fat_size)
        .context("first FAT lies outside assembled image")?;
    let second = image
        .get(second_offset..second_offset + fat_size)
        .context("second FAT lies outside assembled image")?;
    ensure!(first == second, "assembled FAT mirrors differ");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::ensure_output_does_not_alias_source;

    #[test]
    fn output_must_not_be_the_source_or_its_hard_link() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.hdm");
        let hard_link = directory.path().join("source-alias.hdm");
        fs::write(&source, b"synthetic source").unwrap();
        fs::hard_link(&source, &hard_link).unwrap();

        assert!(ensure_output_does_not_alias_source(&source, &source).is_err());
        assert!(ensure_output_does_not_alias_source(&source, &hard_link).is_err());
    }

    #[test]
    fn distinct_or_new_output_path_is_allowed() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.hdm");
        let existing_output = directory.path().join("existing-output.hdm");
        let new_output = directory.path().join("new-output.hdm");
        fs::write(&source, b"synthetic source").unwrap();
        fs::write(&existing_output, b"previous output").unwrap();

        ensure_output_does_not_alias_source(&source, &existing_output).unwrap();
        ensure_output_does_not_alias_source(&source, &new_output).unwrap();
    }
}
