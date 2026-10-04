use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};
use v30::{
    AssembledProgram, Assembler, CodeLocation, Instruction, Operand, Register8, Register16,
    SegmentRegister,
};

use crate::catalog_bundle::{read_catalog_json, resolve_catalog_part_path};
use crate::compile_lz::{decode_exact_compile_lz, encode_compile_lz};
use crate::font::{FontReport, GLYPH_BYTES, font_report, rasterize_character};
use crate::lha_sfx::extract_lha_sfx;
use crate::renderer_font::{
    RendererFontPlan, RendererParticleSelector, contains_renderer_sheet_code, encode_renderer_text,
    install_main_renderer_sheet_hook, is_modern_hangul, jis_to_shift_jis,
};
use crate::source_disk::{load_verified_source, sha256_hex};

const CATALOG_SCHEMA: &str = "pc98_madou_docho.main_text_catalog";
const CATALOG_BUNDLE_SCHEMA: &str = "pc98_madou_docho.main_text_catalog_bundle";
const ENTRY_BATCH_SCHEMA: &str = "pc98_madou_docho.main_text_entry_batch";
const RESOURCE_NAME: &str = "MAIN.OVL";
// The bytes immediately after the original overlay become composition scratch
// during battle. Keep relocated text and its renderer hook in the high band
// that remains resident in the target runtime instead.
const RUNTIME_RELOCATION_DECODED_RANGE: std::ops::Range<usize> = 0xb000..0xc000;
const ORIGINAL_PACKED_SHA256: &str =
    "419178fd0a795f629a9d67f68d58e1e6cb7539d149cfeed435a5d2c21bda924a";
const ORIGINAL_DECODED_SHA256: &str =
    "6886bdfcba6bfdc462be854322d6b67fd854ef4008efd03a831831c8b3f0f736";
const LOAD_OFFSET: usize = 0x0100;
const RENDERER_LOGICAL_OFFSET: usize = 0x4ef0;
const MIN_TABLE_ENTRIES: usize = 4;
const TABLE_STRIDES: [usize; 8] = [2, 4, 6, 8, 10, 12, 14, 16];
const SOUND_CONFIGURATION_RECORD_TABLE_OFFSET: usize = 0x39fe;
const SOUND_CONFIGURATION_RECORD_STRIDE: usize = 4;
const SOUND_CONFIGURATION_RECORD_COUNT: usize = 5;
const DISK_ERROR_POINTER_TABLE_OFFSET: usize = 0x4c02;
const DISK_ERROR_POINTER_COUNT: usize = 16;
const SPELL_RECORD_TABLE_OFFSET: usize = 0x58d4;
const SPELL_RECORD_STRIDE: usize = 8;
const SPELL_RECORD_COUNT: usize = 16;
const SPELL_FIRST_DISPLAY_PREFIX_OFFSET: usize = 0x6063;
const ITEM_RECORD_TABLE_OFFSET: usize = 0x5954;
const ITEM_RECORD_STRIDE: usize = 10;
const ITEM_RECORD_COUNT: usize = 64;
const RECOVERED_SANITY_MESSAGE_OFFSET: usize = 0x8d61;
const AWAKENED_MESSAGE_OFFSET: usize = 0x8db8;
const REFLECTED_DAMAGE_MESSAGE_OFFSET: usize = 0x8de8;
const UNSELECTED_BARRIER_EXPIRY_MESSAGE_OFFSET: usize = 0x8e0b;
const REFLECTED_DAMAGE_EXPIRY_MESSAGE_OFFSET: usize = 0x8e20;
const REFLECTED_DAMAGE_EXPIRY_SUFFIX_OFFSET: usize = 0x8e2c;
const INTERNAL_SAVE_DATA_SIGNATURE_MESSAGE_OFFSET: usize = 0x56c0;
const DIAGNOSTIC_CANDIDATE_OFFSETS: [usize; 40] = [
    0x4d64, 0x4d77, 0x56c0, 0x62cc, 0x62d7, 0x8e0b, 0x8f4e, 0x905e, 0x9085, 0x909e, 0x90bc, 0x90eb,
    0x911e, 0x913a, 0x9153, 0x916c, 0x9182, 0x91a3, 0x91bf, 0x91dd, 0x9204, 0x9239, 0x932b, 0x9340,
    0x9355, 0x936e, 0x9389, 0x93a8, 0x93bd, 0x93d2, 0x93ef, 0x940e, 0x9437, 0x948d, 0x94af, 0x94cd,
    0x94e4, 0x94fd, 0x951c, 0x9527,
];
const RESOLVED_DIAGNOSTIC_CANDIDATE_ENTRIES: [(usize, usize); 2] =
    [(0x62cc, 0x62ce), (0x62d7, 0x62d9)];
const NON_KANA_DIAGNOSTIC_CANDIDATE_OFFSETS: [usize; 1] = [0x951c];
const EXCLUDED_DISK_ERROR_CANDIDATE_OFFSETS: [usize; 2] = [0x4d64, 0x4d77];
const DISABLED_ITEM_EFFECT_CANDIDATE_GROUPS: &[(usize, usize, &[usize])] = &[
    (16, 0x8598, &[0x905e, 0x9085]),
    (17, 0x8599, &[0x909e, 0x90bc]),
    (19, 0x85da, &[0x90eb, 0x911e]),
    (20, 0x85da, &[0x913a, 0x9153]),
    (22, 0x85db, &[0x916c, 0x9182, 0x91a3]),
    (23, 0x85dc, &[0x91bf]),
    (24, 0x85dd, &[0x91dd]),
    (25, 0x85de, &[0x9204]),
    (26, 0x85df, &[0x9239]),
];
const WALKING_RECOVERY_ITEM_RECORD_INDEX: usize = 9;
const CONSUMABLE_ITEM_EFFECT_LOGICAL_OFFSET: usize = 0x846c;
const WALKING_RECOVERY_EVENT_LOGICAL_OFFSET: usize = 0x84b5;
const CONSUMABLE_ITEM_MESSAGE_OFFSET: usize = 0x8f71;
const UNSELECTED_WALKING_RECOVERY_MESSAGE_OFFSET: usize = 0x8f4e;
const DUNGEON_FLOOR_RECORD_TABLE_OFFSET: usize = 0x5bd4;
const DUNGEON_FLOOR_RECORD_STRIDE: usize = 8;
const DUNGEON_FLOOR_RECORD_COUNT: usize = 6;
const DUNGEON_RESOURCE_FILENAME_LOGICAL_OFFSET: u16 = 0x641c;
const DUNGEON_FLOOR_MESSAGE_OFFSETS: [usize; DUNGEON_FLOOR_RECORD_COUNT] =
    [0x5f14, 0x5f1f, 0x5f2a, 0x5f35, 0x5f40, 0x5f4b];
const DAMAGE_REACTION_RECORD_TABLE_OFFSET: usize = 0x1011;
const DAMAGE_REACTION_RECORD_OFFSETS: [usize; 16] = [
    0x11a9, 0x118f, 0x1179, 0x1167, 0x114f, 0x113b, 0x111a, 0x10fe, 0x10e6, 0x10d0, 0x10be, 0x109a,
    0x1080, 0x1065, 0x104f, 0x1031,
];
const DAMAGE_REACTION_RECORD_PREFIXES: [u8; DAMAGE_REACTION_RECORD_OFFSETS.len()] = [
    0xff, 0xff, 0xff, 0xff, 0x0c, 0x0c, 0x0c, 0x0c, 0x0b, 0x0b, 0x0b, 0x0b, 0x0a, 0x0a, 0x0a, 0x0a,
];
const ENEMY_HEALTH_STATUS_TABLE_RANGE: std::ops::Range<usize> = 0x121d..0x123d;
const ENEMY_HIT_REACTION_TABLE_RANGE: std::ops::Range<usize> = 0x1422..0x1432;
const ENEMY_STATUS_MESSAGE_COUNT: usize = 24;
const ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES: [(usize, [u8; 6]); 2] = [
    (0x11d0, [0xb8, 0x4f, 0x24, 0xe8, 0x4a, 0x3d]),
    (0x13f5, [0xb8, 0x4f, 0x24, 0xe8, 0x25, 0x3b]),
];
const FULL_INVENTORY_MESSAGE_OFFSET: usize = 0x6717;
const UMBRELLA_FALL_MESSAGE_OFFSET: usize = 0x748b;
const PAUSE_MESSAGE_OFFSET: usize = 0x75b5;
const SYSTEM_MENU_MESSAGE_OFFSET: usize = 0x410c;
const COMMAND_MENU_MESSAGE_OFFSET: usize = 0x40a1;
const LOAD_DATA_TITLE_MESSAGE_OFFSET: usize = 0x4227;
const SAVE_DATA_TITLE_MESSAGE_OFFSET: usize = 0x4241;
const LOAD_CONFIRMATION_MESSAGE_OFFSET: usize = 0x425b;
const OVERWRITE_CONFIRMATION_MESSAGE_OFFSET: usize = 0x4278;
const VOLUME_MENU_MESSAGE_OFFSET: usize = 0x4171;
const BEEP1_MENU_MESSAGE_OFFSET: usize = 0x418d;
const BEEP2_MENU_MESSAGE_OFFSET: usize = 0x41a5;
const PSG_MENU_MESSAGE_OFFSET: usize = 0x41bd;
const OTOHIME_MENU_MESSAGE_OFFSET: usize = 0x41c7;
const SPEAK_BOARD_MENU_MESSAGE_OFFSET: usize = 0x41cf;
const SOUND_OFF_MENU_MESSAGE_OFFSET: usize = 0x41e1;
const DISPLAY_MODE_MENU_MESSAGE_OFFSET: usize = 0x41eb;
const FIXED_CURSOR_MENU_MESSAGE_OFFSETS: &[usize] = &[
    SYSTEM_MENU_MESSAGE_OFFSET,
    VOLUME_MENU_MESSAGE_OFFSET,
    BEEP1_MENU_MESSAGE_OFFSET,
    BEEP2_MENU_MESSAGE_OFFSET,
    PSG_MENU_MESSAGE_OFFSET,
    OTOHIME_MENU_MESSAGE_OFFSET,
    SPEAK_BOARD_MENU_MESSAGE_OFFSET,
    SOUND_OFF_MENU_MESSAGE_OFFSET,
    DISPLAY_MODE_MENU_MESSAGE_OFFSET,
];
const SOUND_DESCRIPTION_MESSAGE_OFFSETS: &[usize] =
    &[BEEP1_MENU_MESSAGE_OFFSET, BEEP2_MENU_MESSAGE_OFFSET];
const SOUND_DESCRIPTION_MAX_DISPLAY_COLUMNS: usize = 22;
const UNUSED_DATA_SLOT_MESSAGE_OFFSET: usize = 0x4217;
const POLLEN_ALLERGY_MESSAGE_OFFSET: usize = 0x9947;
const EXPLICITLY_VERIFIED_MESSAGE_OFFSETS: &[usize] = &[
    COMMAND_MENU_MESSAGE_OFFSET,
    VOLUME_MENU_MESSAGE_OFFSET,
    UNUSED_DATA_SLOT_MESSAGE_OFFSET,
    LOAD_DATA_TITLE_MESSAGE_OFFSET,
    SAVE_DATA_TITLE_MESSAGE_OFFSET,
    LOAD_CONFIRMATION_MESSAGE_OFFSET,
    OVERWRITE_CONFIRMATION_MESSAGE_OFFSET,
    0x5f14,
    0x5f1f,
    0x5f2a,
    0x5f35,
    0x5f40,
    0x5f4b,
    FULL_INVENTORY_MESSAGE_OFFSET,
    UMBRELLA_FALL_MESSAGE_OFFSET,
    PAUSE_MESSAGE_OFFSET,
    POLLEN_ALLERGY_MESSAGE_OFFSET,
];
const CURRENT_COMPANION_RECORD_RANGE: std::ops::Range<usize> = 32..40;
const CURRENT_COMPANION_DESCRIPTOR_LOGICAL_OFFSET: usize = 0x8739;
const CURRENT_COMPANION_DISPLAY_NAME_MESSAGE_OFFSET: usize = 0x6252;
const CURRENT_COMPANION_DISPLAY_NAME_POINTER_SITE: usize =
    CURRENT_COMPANION_DESCRIPTOR_LOGICAL_OFFSET - LOAD_OFFSET + 6;
const CURRENT_COMPANION_ACTION_MESSAGES: [(usize, usize); 3] =
    [(0x9456, 0x87a8), (0x9465, 0x886b), (0x9482, 0x8807)];
const UNSELECTED_COMPANION_ACTION_CANDIDATE_OFFSETS: [usize; 18] = [
    0x932b, 0x9340, 0x9355, 0x936e, 0x9389, 0x93a8, 0x93bd, 0x93d2, 0x93ef, 0x940e, 0x9437, 0x948d,
    0x94af, 0x94cd, 0x94e4, 0x94fd, 0x951c, 0x9527,
];
const UNTRANSLATED_STATUS: &str = "untranslated";
const DRAFT_STATUS: &str = "needs_human_review";
const RELEASE_STATUS: &str = "distribution_eligible";
const MAIN_ENTRY_LOGICAL_OFFSET: usize = 0x0100;
const MAIN_ENTRY_RESUME_LOGICAL_OFFSET: usize = 0x010b;
const GAIJI_FIRST_ROW: u8 = 0x76;
const GAIJI_LAST_ROW: u8 = 0x77;
const JIS_FIRST_CELL: u8 = 0x21;
const JIS_LAST_CELL: u8 = 0x7e;
const GAIJI_RECORD_PREFIX: [u8; 2] = [0x02, 0x02];

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainTextCatalog {
    pub schema: String,
    pub resource: String,
    pub source_packed_sha256: String,
    pub source_decoded_sha256: String,
    pub decoded_size: usize,
    pub load_offset: usize,
    pub renderer_logical_offset: usize,
    pub strict_renderer_messages: usize,
    pub entry_count: usize,
    pub rewrite_site_count: usize,
    pub total_slot_bytes: usize,
    pub diagnostic_candidate_count: usize,
    pub resolved_candidate_count: usize,
    pub excluded_candidate_count: usize,
    pub unresolved_candidate_count: usize,
    pub diagnostic_candidates: Vec<MainTextDiagnosticCandidate>,
    pub entries: Vec<MainTextEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MainTextCatalogBundle {
    schema: String,
    catalog: MainTextCatalog,
    entry_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MainTextEntryBatch {
    schema: String,
    purpose: String,
    entries: Vec<MainTextEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainTextEntry {
    pub id: String,
    pub string_decoded_offset: usize,
    pub string_logical_offset: usize,
    pub byte_budget: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues_at_logical_offset: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addressed_prefix_raw_hex: Option<String>,
    pub raw_hex: String,
    pub source_text: String,
    pub rewrite_sites: Vec<MainTextRewriteSite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncation_sites: Vec<MainTextTruncationSite>,
    pub ko: String,
    pub status: String,
    pub notes: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainTextRewriteSite {
    pub decoded_offset: usize,
    pub kind: MainTextPointerKind,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub string_offset_from_target: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainTextTruncationSite {
    pub decoded_offset: usize,
    pub line_number: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTextPointerKind {
    MovSi,
    MovDi,
    StoredPointer,
    Table,
    PrefixedRecord,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainTextDiagnosticCandidate {
    pub id: String,
    pub string_decoded_offset: usize,
    pub string_logical_offset: usize,
    pub byte_budget: usize,
    pub raw_hex: String,
    pub decoded_text: String,
    pub state: MainTextCandidateState,
    pub classification_basis: MainTextCandidateClassificationBasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<MainTextCandidateNextAction>,
    pub ko: String,
    pub status: String,
    pub notes: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTextCandidateState {
    Resolved,
    Excluded,
    Unresolved,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTextCandidateClassificationBasis {
    VerifiedPrefixedRecordConsumer,
    OutsideBoundedDiskErrorPointerTable,
    DisabledItemEffectHandler,
    ItemEffectSelectsDifferentMessage,
    TimedStatusSelectsDifferentExpiryMessage,
    CompanionRecordsSelectCurrentActionDescriptor,
    InternalSaveDataSignature,
    NoVerifiedConsumer,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTextCandidateNextAction {
    ClassifyDataBeforeResourceFilenameTable,
    TraceStatusOrBattleDispatchConsumer,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct MainOverlayRebuildReport {
    pub resource: String,
    pub changed_entries: usize,
    pub relocated_entries: usize,
    pub translation_pool_bytes: usize,
    pub changed_candidate_slots: usize,
    pub original_packed_sha256: String,
    pub updated_packed_sha256: String,
    pub original_decoded_sha256: String,
    pub updated_decoded_sha256: String,
    pub original_packed_size: usize,
    pub updated_packed_size: usize,
    pub decoded_size: usize,
    pub gaiji_glyph_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaiji_installer_logical_offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<FontReport>,
    pub gaiji_glyphs: Vec<MainTextGaijiGlyphReport>,
    pub exact_decode_roundtrip: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct MainTextCatalogValidation {
    pub entry_count: usize,
    pub rewrite_site_count: usize,
    pub total_slot_bytes: usize,
    pub diagnostic_candidate_count: usize,
    pub unresolved_candidate_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct MainTextGaijiGlyphReport {
    pub character: char,
    pub jis_code: String,
    pub shift_jis_code: String,
}

#[derive(Debug, Clone)]
struct Message {
    string_decoded_offset: usize,
    string_logical_offset: usize,
    continuation_decoded_offset: Option<usize>,
    addressed_prefix_raw: Vec<u8>,
    raw: Vec<u8>,
    text: String,
    rewrite_sites: Vec<MainTextRewriteSite>,
    truncation_sites: Vec<MainTextTruncationSite>,
}

#[derive(Debug, Clone)]
struct StrictReference {
    renderer_decoded_offset: usize,
    string_decoded_offset: usize,
    rewrite_site: usize,
}

#[derive(Debug, Clone)]
struct MainTextGaijiGlyph {
    character: char,
    jis_code: u16,
    shift_jis_code: [u8; 2],
    bitmap: [u8; GLYPH_BYTES],
}

pub fn extract_main_text_catalog(source_path: &Path) -> Result<MainTextCatalog> {
    let (packed, decoded) = load_main_overlay(source_path)?;
    build_catalog(&packed, &decoded)
}

pub fn write_main_text_catalog(source_path: &Path, output_path: &Path) -> Result<MainTextCatalog> {
    let catalog = extract_main_text_catalog(source_path)?;
    let encoded = serde_json::to_vec_pretty(&catalog).context("serialize MAIN.OVL text catalog")?;
    write_after_validation(output_path, &encoded, "MAIN.OVL text catalog")?;
    Ok(catalog)
}

pub fn validate_main_text_catalog(
    source_path: &Path,
    catalog_path: &Path,
) -> Result<MainTextCatalogValidation> {
    let (original_packed, original_decoded) = load_main_overlay(source_path)?;
    let catalog =
        read_validated_main_text_catalog(&original_packed, &original_decoded, catalog_path)?;
    Ok(MainTextCatalogValidation {
        entry_count: catalog.entry_count,
        rewrite_site_count: catalog.rewrite_site_count,
        total_slot_bytes: catalog.total_slot_bytes,
        diagnostic_candidate_count: catalog.diagnostic_candidate_count,
        unresolved_candidate_count: catalog.unresolved_candidate_count,
    })
}

pub fn rebuild_main_overlay(
    source_path: &Path,
    catalog_path: &Path,
    output_path: &Path,
) -> Result<MainOverlayRebuildReport> {
    let (original_packed, original_decoded) = load_main_overlay(source_path)?;
    let (updated_packed, report) =
        compile_main_overlay(&original_packed, &original_decoded, catalog_path)?;
    write_after_validation(output_path, &updated_packed, "rebuilt MAIN.OVL")?;
    Ok(report)
}

pub(crate) fn apply_main_text_draft(
    payload: &mut BTreeMap<String, Vec<u8>>,
    catalog_path: &Path,
) -> Result<MainOverlayRebuildReport> {
    let original_packed = payload
        .get(RESOURCE_NAME)
        .context("installer payload is missing MAIN.OVL")?
        .clone();
    let original_decoded = decode_supported_main_overlay(&original_packed)?;
    let (updated_packed, report) =
        compile_main_overlay(&original_packed, &original_decoded, catalog_path)?;
    payload.insert(RESOURCE_NAME.to_owned(), updated_packed);
    Ok(report)
}

pub(crate) fn apply_main_text_renderer_sheet_draft(
    payload: &mut BTreeMap<String, Vec<u8>>,
    catalog_path: &Path,
    font_plan: &RendererFontPlan,
) -> Result<MainOverlayRebuildReport> {
    let original_packed = payload
        .get(RESOURCE_NAME)
        .context("installer payload is missing MAIN.OVL")?
        .clone();
    let original_decoded = decode_supported_main_overlay(&original_packed)?;
    let supplied =
        read_validated_main_text_catalog(&original_packed, &original_decoded, catalog_path)?;
    validate_renderer_sheet_source_messages(&supplied)?;
    let (updated_packed, report) = compile_main_overlay_draft(
        &original_packed,
        &original_decoded,
        &supplied,
        font_plan.codebook(),
        MainTextGlyphSupply::RendererSheet(font_plan.particle_selector()),
    )?;
    payload.insert(RESOURCE_NAME.to_owned(), updated_packed);
    Ok(report)
}

fn validate_renderer_sheet_source_messages(catalog: &MainTextCatalog) -> Result<()> {
    for (id, raw_hex) in catalog
        .entries
        .iter()
        .map(|entry| (&entry.id, &entry.raw_hex))
        .chain(
            catalog
                .diagnostic_candidates
                .iter()
                .map(|candidate| (&candidate.id, &candidate.raw_hex)),
        )
    {
        let bytes =
            decode_hex(raw_hex).with_context(|| format!("{id} has invalid protected raw_hex"))?;
        ensure_message_avoids_renderer_sheet_codes(&bytes)
            .with_context(|| format!("{id} source text conflicts with the renderer font rows"))?;
    }
    Ok(())
}

pub(crate) fn main_text_hangul_characters(catalog_path: &Path) -> Result<BTreeSet<char>> {
    let catalog = read_main_text_catalog(catalog_path)?;
    Ok(catalog
        .entries
        .iter()
        .flat_map(|entry| entry.ko.chars())
        .chain(
            catalog
                .diagnostic_candidates
                .iter()
                .filter(|candidate| candidate.state == MainTextCandidateState::Unresolved)
                .flat_map(|candidate| candidate.ko.chars()),
        )
        .filter(|character| is_modern_hangul(*character))
        .collect())
}

enum MainTextGlyphSupply<'a> {
    BiosGaiji(&'a [MainTextGaijiGlyph]),
    RendererSheet(&'a RendererParticleSelector),
}

fn compile_main_overlay(
    original_packed: &[u8],
    original_decoded: &[u8],
    catalog_path: &Path,
) -> Result<(Vec<u8>, MainOverlayRebuildReport)> {
    let supplied =
        read_validated_main_text_catalog(original_packed, original_decoded, catalog_path)?;
    let gaiji_glyphs = compile_gaiji_glyphs(&supplied)?;
    let codebook = gaiji_glyphs
        .iter()
        .map(|glyph| (glyph.character, glyph.shift_jis_code))
        .collect::<BTreeMap<_, _>>();
    compile_main_overlay_draft(
        original_packed,
        original_decoded,
        &supplied,
        &codebook,
        MainTextGlyphSupply::BiosGaiji(&gaiji_glyphs),
    )
}

fn read_validated_main_text_catalog(
    original_packed: &[u8],
    original_decoded: &[u8],
    catalog_path: &Path,
) -> Result<MainTextCatalog> {
    let expected = build_catalog(original_packed, original_decoded)?;
    let supplied = read_main_text_catalog(catalog_path)?;
    validate_protected_catalog(&expected, &supplied)?;
    validate_candidate_translation_states(&supplied)?;
    validate_fixed_cursor_menu_layouts(&supplied)?;
    Ok(supplied)
}

fn validate_fixed_cursor_menu_layouts(catalog: &MainTextCatalog) -> Result<()> {
    for entry in &catalog.entries {
        validate_fixed_cursor_menu_text(&entry.id, entry.string_decoded_offset, &entry.ko)?;
    }
    Ok(())
}

fn validate_fixed_cursor_menu_text(id: &str, decoded_offset: usize, text: &str) -> Result<()> {
    if text.is_empty() || !FIXED_CURSOR_MENU_MESSAGE_OFFSETS.contains(&decoded_offset) {
        return Ok(());
    }
    for line in text.split(['\n', '\r']).filter(|line| !line.is_empty()) {
        ensure!(
            line.starts_with('　'),
            "{id} fixed cursor menu line must reserve one full-width leading cell"
        );
    }
    if SOUND_DESCRIPTION_MESSAGE_OFFSETS.contains(&decoded_offset) {
        let display_columns = text
            .split(['\n', '\r'])
            .filter(|line| !line.is_empty())
            .map(display_columns)
            .max()
            .unwrap_or(0);
        ensure!(
            display_columns <= SOUND_DESCRIPTION_MAX_DISPLAY_COLUMNS,
            "{id} sound description needs {display_columns} display columns but the menu allows {SOUND_DESCRIPTION_MAX_DISPLAY_COLUMNS}",
        );
    }
    Ok(())
}

fn display_columns(text: &str) -> usize {
    text.chars()
        .map(|character| {
            if character.is_ascii() || ('\u{ff61}'..='\u{ff9f}').contains(&character) {
                1
            } else {
                2
            }
        })
        .sum()
}

fn compile_main_overlay_draft(
    original_packed: &[u8],
    original_decoded: &[u8],
    supplied: &MainTextCatalog,
    codebook: &BTreeMap<char, [u8; 2]>,
    glyph_supply: MainTextGlyphSupply<'_>,
) -> Result<(Vec<u8>, MainOverlayRebuildReport)> {
    let mut updated_decoded = original_decoded.to_vec();
    let mut changed_entries = 0usize;
    let mut relocated_entries = 0usize;
    let mut translation_pool_bytes = 0usize;
    let mut changed_candidate_slots = 0usize;
    let mut runtime_relocation_pool_started = false;
    for entry in &supplied.entries {
        if entry.ko.is_empty() {
            ensure!(
                entry.status == UNTRANSLATED_STATUS,
                "{} has no Korean text but status is {:?}",
                entry.id,
                entry.status
            );
            continue;
        }
        ensure!(
            matches!(entry.status.as_str(), DRAFT_STATUS | RELEASE_STATUS),
            "{} Korean text requires status {DRAFT_STATUS:?} or {RELEASE_STATUS:?}",
            entry.id,
        );
        let replacement = encode_renderer_text(&entry.ko, codebook)
            .with_context(|| format!("{} Korean text cannot be encoded", entry.id))?;
        validate_message_body(&replacement)
            .with_context(|| format!("{} Korean text is not a valid message body", entry.id))?;
        let source_raw = decode_hex(&entry.raw_hex)
            .with_context(|| format!("{} has invalid protected raw_hex", entry.id))?;
        ensure!(
            renderer_control_sequences(&replacement)? == renderer_control_sequences(&source_raw)?,
            "{} Korean text changed the ordered renderer control instructions",
            entry.id
        );

        if replacement_requires_relocation(entry, &replacement) && !runtime_relocation_pool_started
        {
            start_runtime_relocation_pool(
                &mut updated_decoded,
                RUNTIME_RELOCATION_DECODED_RANGE.clone(),
            )?;
            runtime_relocation_pool_started = true;
        }
        let decoded_size_before = updated_decoded.len();
        if apply_main_text_replacement(&mut updated_decoded, entry, &source_raw, &replacement)? {
            relocated_entries += 1;
            translation_pool_bytes += updated_decoded.len() - decoded_size_before;
        }
        changed_entries += 1;
    }
    for candidate in &supplied.diagnostic_candidates {
        if candidate.ko.is_empty() {
            ensure!(
                candidate.status == UNTRANSLATED_STATUS,
                "{} has no Korean text but status is {:?}",
                candidate.id,
                candidate.status
            );
            continue;
        }
        ensure!(
            candidate.state == MainTextCandidateState::Unresolved,
            "{} is not an unresolved in-place candidate slot",
            candidate.id
        );
        ensure!(
            matches!(candidate.status.as_str(), DRAFT_STATUS | RELEASE_STATUS),
            "{} Korean text requires status {DRAFT_STATUS:?} or {RELEASE_STATUS:?}",
            candidate.id,
        );
        let replacement = encode_renderer_text(&candidate.ko, codebook)
            .with_context(|| format!("{} Korean text cannot be encoded", candidate.id))?;
        ensure!(
            replacement.len() < candidate.byte_budget,
            "{} Korean text needs {} bytes plus a terminator but its fixed slot is {} bytes",
            candidate.id,
            replacement.len(),
            candidate.byte_budget
        );
        validate_message_body(&replacement)
            .with_context(|| format!("{} Korean text is not a valid message body", candidate.id))?;
        let source_raw = decode_hex(&candidate.raw_hex)
            .with_context(|| format!("{} has invalid protected raw_hex", candidate.id))?;
        ensure!(
            renderer_control_sequences(&replacement)? == renderer_control_sequences(&source_raw)?,
            "{} Korean text changed the ordered renderer control instructions",
            candidate.id
        );

        write_nul_terminated_text_replacement(
            &mut updated_decoded,
            candidate.string_decoded_offset,
            candidate.byte_budget,
            &candidate.id,
            &replacement,
        )?;
        changed_candidate_slots += 1;
    }

    let complete_korean_enemy_status = enemy_status_tables_have_complete_korean_text(supplied)?;

    let (gaiji_glyphs, gaiji_installer_logical_offset, font, supply_changed) = match glyph_supply {
        MainTextGlyphSupply::BiosGaiji(gaiji_glyphs) if gaiji_glyphs.is_empty() => {
            (gaiji_glyphs, None, None, false)
        }
        MainTextGlyphSupply::BiosGaiji(gaiji_glyphs) => {
            if complete_korean_enemy_status {
                remove_enemy_status_topic_particle_glyphs(&mut updated_decoded)?;
            }
            let installer = install_main_entry_gaiji_hook(&mut updated_decoded, gaiji_glyphs)?;
            (gaiji_glyphs, Some(installer), Some(font_report()?), true)
        }
        MainTextGlyphSupply::RendererSheet(particle_selector) => {
            if !runtime_relocation_pool_started {
                start_runtime_relocation_pool(
                    &mut updated_decoded,
                    RUNTIME_RELOCATION_DECODED_RANGE.clone(),
                )?;
                runtime_relocation_pool_started = true;
            }
            install_main_renderer_sheet_hook(
                &mut updated_decoded,
                particle_selector,
                complete_korean_enemy_status,
            )?;
            (&[][..], None, None, true)
        }
    };
    if runtime_relocation_pool_started {
        finish_runtime_relocation_pool(
            &mut updated_decoded,
            RUNTIME_RELOCATION_DECODED_RANGE.clone(),
        )?;
    }
    let updated_packed = if changed_entries == 0 && changed_candidate_slots == 0 && !supply_changed
    {
        original_packed.to_vec()
    } else {
        encode_compile_lz(&updated_decoded)
    };
    let roundtrip = decode_exact_compile_lz(&updated_packed)
        .context("rebuilt MAIN.OVL is not one exact Compile-LZ stream")?;
    ensure!(
        roundtrip.streams == [updated_decoded.clone()],
        "rebuilt MAIN.OVL does not decode to the planned bytes"
    );
    let gaiji_glyph_reports = gaiji_glyphs
        .iter()
        .map(|glyph| MainTextGaijiGlyphReport {
            character: glyph.character,
            jis_code: format!("0x{:04X}", glyph.jis_code),
            shift_jis_code: encode_hex(&glyph.shift_jis_code),
        })
        .collect::<Vec<_>>();

    Ok((
        updated_packed.clone(),
        MainOverlayRebuildReport {
            resource: RESOURCE_NAME.to_owned(),
            changed_entries,
            relocated_entries,
            translation_pool_bytes,
            changed_candidate_slots,
            original_packed_sha256: sha256_hex(original_packed),
            updated_packed_sha256: sha256_hex(&updated_packed),
            original_decoded_sha256: sha256_hex(original_decoded),
            updated_decoded_sha256: sha256_hex(&updated_decoded),
            original_packed_size: original_packed.len(),
            updated_packed_size: updated_packed.len(),
            decoded_size: updated_decoded.len(),
            gaiji_glyph_count: gaiji_glyphs.len(),
            gaiji_installer_logical_offset,
            font,
            gaiji_glyphs: gaiji_glyph_reports,
            exact_decode_roundtrip: true,
        },
    ))
}

fn enemy_status_tables_have_complete_korean_text(catalog: &MainTextCatalog) -> Result<bool> {
    let entries = catalog
        .entries
        .iter()
        .filter(|entry| {
            entry.rewrite_sites.iter().any(|site| {
                site.kind == MainTextPointerKind::Table
                    && (ENEMY_HEALTH_STATUS_TABLE_RANGE.contains(&site.decoded_offset)
                        || ENEMY_HIT_REACTION_TABLE_RANGE.contains(&site.decoded_offset))
            })
        })
        .collect::<Vec<_>>();
    ensure!(
        entries.len() == ENEMY_STATUS_MESSAGE_COUNT,
        "MAIN.OVL enemy status table population changed"
    );
    Ok(entries.iter().all(|entry| !entry.ko.is_empty()))
}

fn remove_enemy_status_topic_particle_glyphs(decoded: &mut [u8]) -> Result<()> {
    for (site, expected) in &ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
        ensure!(
            decoded.get(*site..*site + expected.len()) == Some(expected.as_slice()),
            "MAIN.OVL enemy status topic-particle renderer changed at {site:#06x}"
        );
    }
    for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
        decoded[site..site + expected.len()].fill(0x90);
    }
    Ok(())
}

fn apply_main_text_replacement(
    decoded: &mut Vec<u8>,
    entry: &MainTextEntry,
    source: &[u8],
    replacement: &[u8],
) -> Result<bool> {
    if entry.continues_at_logical_offset.is_some() {
        ensure!(
            replacement.len() <= entry.byte_budget,
            "{} Korean text needs {} bytes but its continuing prefix slot is {} bytes",
            entry.id,
            replacement.len(),
            entry.byte_budget
        );
        write_in_place_text_replacement(decoded, entry, replacement)?;
        rewrite_message_truncation_sites(
            decoded,
            entry,
            source,
            replacement,
            entry.string_logical_offset,
        )?;
        return Ok(false);
    }

    if replacement.len() < entry.byte_budget {
        write_in_place_text_replacement(decoded, entry, replacement)?;
        rewrite_message_truncation_sites(
            decoded,
            entry,
            source,
            replacement,
            entry.string_logical_offset,
        )?;
        return Ok(false);
    }

    relocate_main_text_replacement(decoded, entry, source, replacement)?;
    Ok(true)
}

fn replacement_requires_relocation(entry: &MainTextEntry, replacement: &[u8]) -> bool {
    entry.continues_at_logical_offset.is_none() && replacement.len() >= entry.byte_budget
}

fn start_runtime_relocation_pool(
    decoded: &mut Vec<u8>,
    range: std::ops::Range<usize>,
) -> Result<()> {
    ensure!(
        range.start < range.end,
        "MAIN.OVL runtime relocation range is empty or reversed"
    );
    ensure!(
        decoded.len() <= range.start,
        "MAIN.OVL decoded body reaches {:#06x}, overlapping the runtime relocation range starting at {:#06x}",
        decoded.len(),
        range.start
    );
    decoded.resize(range.start, 0);
    Ok(())
}

fn finish_runtime_relocation_pool(
    decoded: &mut Vec<u8>,
    range: std::ops::Range<usize>,
) -> Result<()> {
    ensure!(
        decoded.len() <= range.end,
        "MAIN.OVL runtime relocation payload ends at {:#06x}, beyond the safe range ending at {:#06x}",
        decoded.len(),
        range.end
    );
    decoded.resize(range.end, 0);
    Ok(())
}

fn relocate_main_text_replacement(
    decoded: &mut Vec<u8>,
    entry: &MainTextEntry,
    source: &[u8],
    replacement: &[u8],
) -> Result<()> {
    ensure!(
        entry.continues_at_logical_offset.is_none(),
        "{} continuing prefix cannot be relocated independently",
        entry.id
    );
    let addressed_prefix = entry
        .addressed_prefix_raw_hex
        .as_deref()
        .map(decode_hex)
        .transpose()
        .with_context(|| format!("{} has invalid addressed prefix", entry.id))?
        .unwrap_or_default();
    let record_decoded_offset = decoded.len();
    let replacement_string_decoded_offset = record_decoded_offset
        .checked_add(addressed_prefix.len())
        .context("MAIN.OVL translated string offset overflow")?;
    let replacement_string_logical_offset = replacement_string_decoded_offset
        .checked_add(LOAD_OFFSET)
        .context("MAIN.OVL translated string address overflow")?;
    let translated_end = replacement_string_decoded_offset
        .checked_add(replacement.len())
        .and_then(|end| end.checked_add(1))
        .context("MAIN.OVL translated record size overflow")?;
    ensure!(
        translated_end
            .checked_add(LOAD_OFFSET)
            .is_some_and(|end| end <= 0x1_0000),
        "{} translated record exceeds the MAIN.OVL code segment",
        entry.id
    );

    validate_message_rewrite_targets(decoded, entry, replacement_string_logical_offset)?;
    validate_message_truncation_targets(
        decoded,
        entry,
        source,
        replacement,
        replacement_string_logical_offset,
    )?;

    decoded.extend_from_slice(&addressed_prefix);
    decoded.extend_from_slice(replacement);
    decoded.push(0);
    write_nul_terminated_text_replacement(
        decoded,
        entry.string_decoded_offset,
        entry.byte_budget,
        &entry.id,
        &[],
    )?;
    rewrite_message_references(decoded, entry, replacement_string_logical_offset)?;
    rewrite_message_truncation_sites(
        decoded,
        entry,
        source,
        replacement,
        replacement_string_logical_offset,
    )?;

    ensure!(
        decoded.get(record_decoded_offset..replacement_string_decoded_offset)
            == Some(addressed_prefix.as_slice()),
        "{} relocated addressed prefix readback differs",
        entry.id
    );
    ensure!(
        decoded.get(replacement_string_decoded_offset..translated_end - 1) == Some(replacement),
        "{} relocated Korean text readback differs",
        entry.id
    );
    ensure!(
        decoded.get(translated_end - 1) == Some(&0),
        "{} relocated Korean text lost its terminator",
        entry.id
    );
    Ok(())
}

fn validate_message_rewrite_targets(
    decoded: &[u8],
    entry: &MainTextEntry,
    replacement_string_logical_offset: usize,
) -> Result<()> {
    let mut seen_sites = HashSet::new();
    for site in &entry.rewrite_sites {
        if !seen_sites.insert(site.decoded_offset) {
            continue;
        }
        let source_target = entry
            .string_logical_offset
            .checked_sub(site.string_offset_from_target)
            .context("MAIN.OVL source rewrite target underflow")?;
        let replacement_target = replacement_string_logical_offset
            .checked_sub(site.string_offset_from_target)
            .context("MAIN.OVL replacement rewrite target underflow")?;
        let source_word = u16::try_from(source_target)
            .context("MAIN.OVL source rewrite target exceeds the code segment")?
            .to_le_bytes();
        u16::try_from(replacement_target)
            .context("MAIN.OVL replacement rewrite target exceeds the code segment")?;
        ensure!(
            decoded.get(site.decoded_offset..site.decoded_offset + 2)
                == Some(source_word.as_slice()),
            "{} {:?} rewrite site source target changed",
            entry.id,
            site.kind
        );
    }
    Ok(())
}

fn rewrite_message_references(
    decoded: &mut [u8],
    entry: &MainTextEntry,
    replacement_string_logical_offset: usize,
) -> Result<()> {
    let mut seen_sites = HashSet::new();
    for site in &entry.rewrite_sites {
        if !seen_sites.insert(site.decoded_offset) {
            continue;
        }
        let replacement_target = replacement_string_logical_offset
            .checked_sub(site.string_offset_from_target)
            .context("MAIN.OVL replacement rewrite target underflow")?;
        let replacement_word = u16::try_from(replacement_target)
            .context("MAIN.OVL replacement rewrite target exceeds the code segment")?
            .to_le_bytes();
        let target = decoded
            .get_mut(site.decoded_offset..site.decoded_offset + 2)
            .with_context(|| format!("{} rewrite site lies outside MAIN.OVL", entry.id))?;
        target.copy_from_slice(&replacement_word);
        ensure!(
            target == replacement_word,
            "{} {:?} rewrite site readback differs",
            entry.id,
            site.kind
        );
    }
    Ok(())
}

fn write_in_place_text_replacement(
    decoded: &mut [u8],
    entry: &MainTextEntry,
    replacement: &[u8],
) -> Result<()> {
    if entry.continues_at_logical_offset.is_none() {
        return write_nul_terminated_text_replacement(
            decoded,
            entry.string_decoded_offset,
            entry.byte_budget,
            &entry.id,
            replacement,
        );
    }
    let slot_start = entry.string_decoded_offset;
    let slot_end = slot_start
        .checked_add(entry.byte_budget)
        .context("MAIN.OVL message slot overflow")?;
    let slot = decoded
        .get_mut(slot_start..slot_end)
        .with_context(|| format!("{} message slot lies outside MAIN.OVL", entry.id))?;
    ensure!(
        replacement.len() <= slot.len(),
        "{} replacement exceeds its validated slot",
        entry.id
    );
    let continuation = entry
        .continues_at_logical_offset
        .expect("non-continuing slots returned above");
    ensure!(
        slot_end.checked_add(LOAD_OFFSET) == Some(continuation),
        "{} continuing prefix does not end at its declared continuation",
        entry.id
    );
    slot.fill(b' ');
    slot[..replacement.len()].copy_from_slice(replacement);
    Ok(())
}

fn rewrite_message_truncation_sites(
    decoded: &mut [u8],
    entry: &MainTextEntry,
    source: &[u8],
    replacement: &[u8],
    replacement_string_logical_offset: usize,
) -> Result<()> {
    validate_message_truncation_targets(
        decoded,
        entry,
        source,
        replacement,
        replacement_string_logical_offset,
    )?;
    for site in &entry.truncation_sites {
        let replacement_line_offset = message_line_start_offset(replacement, site.line_number)
            .with_context(|| {
                format!(
                    "{} Korean text has no line {} required by a truncation site",
                    entry.id, site.line_number
                )
            })?;
        let replacement_target = u16::try_from(
            replacement_string_logical_offset
                .checked_add(replacement_line_offset)
                .context("MAIN.OVL replacement truncation target overflow")?,
        )
        .context("MAIN.OVL replacement truncation target exceeds the code segment")?;
        let target = decoded
            .get_mut(site.decoded_offset..site.decoded_offset + 2)
            .with_context(|| format!("{} truncation site lies outside MAIN.OVL", entry.id))?;
        target.copy_from_slice(&replacement_target.to_le_bytes());
        ensure!(
            target == replacement_target.to_le_bytes(),
            "{} truncation site readback differs",
            entry.id
        );
    }
    Ok(())
}

fn validate_message_truncation_targets(
    decoded: &[u8],
    entry: &MainTextEntry,
    source: &[u8],
    replacement: &[u8],
    replacement_string_logical_offset: usize,
) -> Result<()> {
    for site in &entry.truncation_sites {
        let source_line_offset =
            message_line_start_offset(source, site.line_number).with_context(|| {
                format!(
                    "{} source text has no line {} for truncation site",
                    entry.id, site.line_number
                )
            })?;
        let replacement_line_offset = message_line_start_offset(replacement, site.line_number)
            .with_context(|| {
                format!(
                    "{} Korean text has no line {} required by a truncation site",
                    entry.id, site.line_number
                )
            })?;
        let source_target = u16::try_from(
            entry
                .string_logical_offset
                .checked_add(source_line_offset)
                .context("MAIN.OVL source truncation target overflow")?,
        )
        .context("MAIN.OVL source truncation target exceeds the code segment")?;
        u16::try_from(
            replacement_string_logical_offset
                .checked_add(replacement_line_offset)
                .context("MAIN.OVL replacement truncation target overflow")?,
        )
        .context("MAIN.OVL replacement truncation target exceeds the code segment")?;
        ensure!(
            decoded.get(site.decoded_offset..site.decoded_offset + 2)
                == Some(source_target.to_le_bytes().as_slice()),
            "{} truncation site source target changed",
            entry.id
        );
    }
    Ok(())
}

fn write_nul_terminated_text_replacement(
    decoded: &mut [u8],
    slot_start: usize,
    byte_budget: usize,
    id: &str,
    replacement: &[u8],
) -> Result<()> {
    let slot_end = slot_start
        .checked_add(byte_budget)
        .context("MAIN.OVL NUL-terminated text slot overflow")?;
    let slot = decoded
        .get_mut(slot_start..slot_end)
        .with_context(|| format!("{id} text slot lies outside MAIN.OVL"))?;
    ensure!(
        replacement.len() < slot.len(),
        "{id} replacement plus terminator exceeds its validated slot"
    );
    slot.fill(0);
    slot[..replacement.len()].copy_from_slice(replacement);
    Ok(())
}

pub(crate) fn read_main_text_catalog(path: &Path) -> Result<MainTextCatalog> {
    if !path.is_dir() {
        return read_catalog_json(path, "MAIN.OVL text catalog");
    }

    let bundle_path = path.join("catalog.json");
    let bundle: MainTextCatalogBundle =
        read_catalog_json(&bundle_path, "MAIN.OVL text catalog bundle")?;
    ensure!(
        bundle.schema == CATALOG_BUNDLE_SCHEMA,
        "unsupported MAIN.OVL text catalog bundle schema {:?}",
        bundle.schema
    );
    ensure!(
        bundle.catalog.entries.is_empty(),
        "MAIN.OVL catalog bundle metadata must not contain inline entries"
    );
    ensure!(
        !bundle.entry_files.is_empty(),
        "MAIN.OVL catalog bundle has no entry files"
    );

    let mut catalog = bundle.catalog;
    let mut listed_files = HashSet::new();
    let mut entry_ids = HashSet::new();
    for relative_path in bundle.entry_files {
        ensure!(
            listed_files.insert(relative_path.clone()),
            "MAIN.OVL catalog bundle lists {relative_path:?} more than once"
        );
        let batch_path = resolve_catalog_part_path(path, &relative_path, "MAIN.OVL entry batch")?;
        let batch: MainTextEntryBatch = read_catalog_json(&batch_path, "MAIN.OVL entry batch")?;
        ensure!(
            batch.schema == ENTRY_BATCH_SCHEMA,
            "unsupported MAIN.OVL entry batch schema {:?} in {}",
            batch.schema,
            batch_path.display()
        );
        ensure!(
            !batch.purpose.trim().is_empty(),
            "MAIN.OVL entry batch has no purpose: {}",
            batch_path.display()
        );
        for entry in batch.entries {
            ensure!(
                entry_ids.insert(entry.id.clone()),
                "MAIN.OVL entry {} appears in more than one batch",
                entry.id
            );
            catalog.entries.push(entry);
        }
    }

    ensure!(
        catalog.entries.len() == catalog.entry_count,
        "MAIN.OVL catalog bundle declares {} entries but contains {}",
        catalog.entry_count,
        catalog.entries.len()
    );
    let rewrite_site_count = catalog
        .entries
        .iter()
        .map(|entry| entry.rewrite_sites.len() + entry.truncation_sites.len())
        .sum::<usize>();
    ensure!(
        rewrite_site_count == catalog.rewrite_site_count,
        "MAIN.OVL catalog bundle declares {} rewrite sites but contains {rewrite_site_count}",
        catalog.rewrite_site_count
    );
    let total_slot_bytes = catalog
        .entries
        .iter()
        .map(|entry| entry.byte_budget)
        .sum::<usize>();
    ensure!(
        total_slot_bytes == catalog.total_slot_bytes,
        "MAIN.OVL catalog bundle declares {} slot bytes but contains {total_slot_bytes}",
        catalog.total_slot_bytes
    );
    Ok(catalog)
}

fn compile_gaiji_glyphs(catalog: &MainTextCatalog) -> Result<Vec<MainTextGaijiGlyph>> {
    let characters = catalog
        .entries
        .iter()
        .flat_map(|entry| entry.ko.chars())
        .chain(
            catalog
                .diagnostic_candidates
                .iter()
                .filter(|candidate| candidate.state == MainTextCandidateState::Unresolved)
                .flat_map(|candidate| candidate.ko.chars()),
        )
        .filter(|character| is_modern_hangul(*character))
        .collect::<BTreeSet<_>>();
    let cells_per_row = usize::from(JIS_LAST_CELL - JIS_FIRST_CELL + 1);
    let capacity = usize::from(GAIJI_LAST_ROW - GAIJI_FIRST_ROW + 1) * cells_per_row;
    ensure!(
        characters.len() <= capacity,
        "MAIN.OVL Korean text needs {} Hangul glyphs but BIOS gaiji capacity is {capacity}",
        characters.len()
    );

    characters
        .into_iter()
        .enumerate()
        .map(|(index, character)| {
            let row = GAIJI_FIRST_ROW + u8::try_from(index / cells_per_row)?;
            let cell = JIS_FIRST_CELL + u8::try_from(index % cells_per_row)?;
            let jis_code = u16::from_be_bytes([row, cell]);
            Ok(MainTextGaijiGlyph {
                character,
                jis_code,
                shift_jis_code: jis_to_shift_jis(jis_code)?,
                bitmap: rasterize_character(character)
                    .with_context(|| format!("rasterize MAIN.OVL Hangul {character:?}"))?,
            })
        })
        .collect()
}

fn install_main_entry_gaiji_hook(
    decoded: &mut Vec<u8>,
    glyphs: &[MainTextGaijiGlyph],
) -> Result<usize> {
    ensure!(!glyphs.is_empty(), "MAIN.OVL gaiji hook has no glyphs");
    let installer_logical_offset = decoded
        .len()
        .checked_add(LOAD_OFFSET)
        .context("MAIN.OVL gaiji installer address overflow")?;
    let installer_logical_offset = u16::try_from(installer_logical_offset)
        .context("MAIN.OVL gaiji installer exceeds the code segment")?;
    let original_entry = assemble_main_entry_prologue()?;
    ensure!(
        decoded.get(..original_entry.bytes().len()) == Some(original_entry.bytes()),
        "MAIN.OVL entry bytes differ from the typed V30 precondition"
    );

    let placeholder_addresses = vec![0_u16; glyphs.len()];
    let placeholder =
        assemble_gaiji_installer(installer_logical_offset, &placeholder_addresses, glyphs)?;
    let glyph_base = usize::from(installer_logical_offset)
        .checked_add(placeholder.bytes().len())
        .context("MAIN.OVL gaiji record address overflow")?;
    let record_size = GAIJI_RECORD_PREFIX.len() + GLYPH_BYTES;
    let addresses = (0..glyphs.len())
        .map(|index| {
            u16::try_from(glyph_base + index * record_size)
                .context("MAIN.OVL gaiji record exceeds the code segment")
        })
        .collect::<Result<Vec<_>>>()?;
    let installer = assemble_gaiji_installer(installer_logical_offset, &addresses, glyphs)?;
    ensure!(
        installer.bytes().len() == placeholder.bytes().len(),
        "MAIN.OVL gaiji installer changed size after record placement"
    );
    let entry_hijack = assemble_main_entry_hijack(
        u16::try_from(original_entry.bytes().len()).context("MAIN.OVL entry length overflow")?,
        installer_logical_offset,
    )?;
    ensure!(
        entry_hijack.bytes().len() == original_entry.bytes().len(),
        "MAIN.OVL entry hijack is not length-preserving"
    );
    decoded[..entry_hijack.bytes().len()].copy_from_slice(entry_hijack.bytes());
    decoded.extend_from_slice(installer.bytes());
    for glyph in glyphs {
        decoded.extend_from_slice(&GAIJI_RECORD_PREFIX);
        decoded.extend_from_slice(&glyph.bitmap);
    }
    verify_main_gaiji_hook_readback(
        decoded,
        installer_logical_offset,
        &entry_hijack,
        &installer,
        glyphs,
    )?;
    Ok(usize::from(installer_logical_offset))
}

fn assemble_main_entry_prologue() -> Result<AssembledProgram> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Cli)
        .emit(Instruction::Mov {
            dest: Operand::Reg16(Register16::AX),
            src: Operand::Sreg(SegmentRegister::CS),
        })
        .emit(Instruction::Mov {
            dest: Operand::Sreg(SegmentRegister::DS),
            src: Operand::Reg16(Register16::AX),
        })
        .emit(Instruction::Mov {
            dest: Operand::Sreg(SegmentRegister::SS),
            src: Operand::Reg16(Register16::AX),
        })
        .emit(Instruction::Mov {
            dest: Operand::Reg16(Register16::SP),
            src: Operand::Imm16(0),
        })
        .emit(Instruction::Sti);
    assembler
        .assemble(CodeLocation {
            seg: 0,
            off: MAIN_ENTRY_LOGICAL_OFFSET as u16,
        })
        .context("assemble typed MAIN.OVL entry precondition")
}

fn assemble_main_entry_hijack(entry_len: u16, installer_offset: u16) -> Result<AssembledProgram> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Push {
            src: Operand::Imm16(installer_offset),
        })
        .emit(Instruction::Ret { pop: 0 });
    for _ in 4..entry_len {
        assembler.emit(Instruction::Nop);
    }
    assembler
        .assemble(CodeLocation {
            seg: 0,
            off: MAIN_ENTRY_LOGICAL_OFFSET as u16,
        })
        .context("assemble typed MAIN.OVL entry hijack")
}

fn assemble_gaiji_installer(
    origin: u16,
    record_addresses: &[u16],
    glyphs: &[MainTextGaijiGlyph],
) -> Result<AssembledProgram> {
    ensure!(
        !glyphs.is_empty() && record_addresses.len() == glyphs.len(),
        "MAIN.OVL gaiji installer has mismatched records"
    );
    let mut assembler = gaiji_installer_body(record_addresses, glyphs)?;
    assembler
        .emit(Instruction::Push {
            src: Operand::Imm16(MAIN_ENTRY_RESUME_LOGICAL_OFFSET as u16),
        })
        .emit(Instruction::Ret { pop: 0 });
    assembler
        .assemble(CodeLocation {
            seg: 0,
            off: origin,
        })
        .context("assemble typed MAIN.OVL gaiji installer")
}

fn gaiji_installer_body(
    record_addresses: &[u16],
    glyphs: &[MainTextGaijiGlyph],
) -> Result<Assembler> {
    let mut assembler = Assembler::new();
    let original_entry = assemble_main_entry_prologue()?;
    for instruction in original_entry.instruction_spans() {
        assembler.emit(instruction.instruction.clone());
    }
    assembler.emit(Instruction::Pushf).emit(Instruction::Pusha);
    for (&record_address, glyph) in record_addresses.iter().zip(glyphs) {
        assembler
            .emit(Instruction::Mov {
                dest: Operand::Reg16(Register16::BX),
                src: Operand::Sreg(SegmentRegister::CS),
            })
            .emit(Instruction::Mov {
                dest: Operand::Reg16(Register16::CX),
                src: Operand::Imm16(record_address),
            })
            .emit(Instruction::Mov {
                dest: Operand::Reg16(Register16::DX),
                src: Operand::Imm16(glyph.jis_code),
            })
            .emit(Instruction::Mov {
                dest: Operand::Reg8(Register8::AH),
                src: Operand::Imm8(0x1a),
            })
            .emit(Instruction::Int { vector: 0x18 });
    }
    assembler.emit(Instruction::Popa).emit(Instruction::Popf);
    Ok(assembler)
}

fn verify_main_gaiji_hook_readback(
    decoded: &[u8],
    installer_logical_offset: u16,
    entry_hijack: &AssembledProgram,
    installer: &AssembledProgram,
    glyphs: &[MainTextGaijiGlyph],
) -> Result<()> {
    ensure!(
        decoded.get(..entry_hijack.bytes().len()) == Some(entry_hijack.bytes()),
        "MAIN.OVL entry hijack readback differs"
    );
    let installer_offset = usize::from(installer_logical_offset) - LOAD_OFFSET;
    let installer_end = installer_offset + installer.bytes().len();
    ensure!(
        decoded.get(installer_offset..installer_end) == Some(installer.bytes()),
        "MAIN.OVL gaiji installer readback differs"
    );
    let mut cursor = installer_end;
    for glyph in glyphs {
        let end = cursor + GAIJI_RECORD_PREFIX.len() + GLYPH_BYTES;
        let record = decoded
            .get(cursor..end)
            .context("MAIN.OVL gaiji record readback lies outside output")?;
        ensure!(
            record[..GAIJI_RECORD_PREFIX.len()] == GAIJI_RECORD_PREFIX
                && record[GAIJI_RECORD_PREFIX.len()..] == glyph.bitmap,
            "MAIN.OVL gaiji record readback differs for {:?}",
            glyph.character
        );
        cursor = end;
    }
    ensure!(
        cursor == decoded.len(),
        "MAIN.OVL gaiji tail has unknown bytes"
    );
    Ok(())
}

fn load_main_overlay(source_path: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    let packed = payload
        .get(RESOURCE_NAME)
        .context("MADOU.EXE payload is missing MAIN.OVL")?
        .clone();
    let decoded = decode_supported_main_overlay(&packed)?;
    Ok((packed, decoded))
}

fn decode_supported_main_overlay(packed: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        sha256_hex(packed) == ORIGINAL_PACKED_SHA256,
        "MAIN.OVL does not match the supported packed resource"
    );
    let decoded =
        decode_exact_compile_lz(packed).context("MAIN.OVL is not an exact Compile-LZ resource")?;
    ensure!(
        decoded.streams.len() == 1,
        "MAIN.OVL must contain exactly one Compile-LZ stream"
    );
    let decoded = decoded.streams.into_iter().next().unwrap();
    ensure!(
        sha256_hex(&decoded) == ORIGINAL_DECODED_SHA256,
        "MAIN.OVL decoded bytes do not match the supported resource"
    );
    Ok(decoded)
}

fn build_catalog(packed: &[u8], decoded: &[u8]) -> Result<MainTextCatalog> {
    let strict_references = strict_renderer_references(decoded);
    let dominant_renderer = dominant_renderer(&strict_references)
        .context("MAIN.OVL has no supported immediate renderer references")?;
    ensure!(
        dominant_renderer == RENDERER_LOGICAL_OFFSET - LOAD_OFFSET,
        "MAIN.OVL dominant renderer changed: expected 0x{RENDERER_LOGICAL_OFFSET:04X} logical"
    );
    let strict_renderer_messages = strict_references
        .iter()
        .filter(|reference| reference.renderer_decoded_offset == dominant_renderer)
        .map(|reference| reference.string_decoded_offset)
        .collect::<BTreeSet<_>>()
        .len();
    let messages = unified_messages(decoded, &strict_references)?;
    let diagnostic_candidates = build_main_text_diagnostic_candidates(decoded, &messages)?;
    validate_in_place_candidate_slots(decoded, &messages, &diagnostic_candidates)?;
    let resolved_candidate_count = diagnostic_candidates
        .iter()
        .filter(|candidate| candidate.state == MainTextCandidateState::Resolved)
        .count();
    let excluded_candidate_count = diagnostic_candidates
        .iter()
        .filter(|candidate| candidate.state == MainTextCandidateState::Excluded)
        .count();
    let unresolved_candidate_count = diagnostic_candidates
        .iter()
        .filter(|candidate| candidate.state == MainTextCandidateState::Unresolved)
        .count();
    let rewrite_site_count = messages
        .iter()
        .map(|message| message.rewrite_sites.len() + message.truncation_sites.len())
        .sum();
    let total_slot_bytes = messages
        .iter()
        .map(|message| {
            message.raw.len() + usize::from(message.continuation_decoded_offset.is_none())
        })
        .sum();
    let entries = messages
        .into_iter()
        .map(|message| MainTextEntry {
            id: format!("MAIN_{:04X}", message.string_logical_offset),
            string_decoded_offset: message.string_decoded_offset,
            string_logical_offset: message.string_logical_offset,
            byte_budget: message.raw.len()
                + usize::from(message.continuation_decoded_offset.is_none()),
            continues_at_logical_offset: message
                .continuation_decoded_offset
                .map(|offset| offset + LOAD_OFFSET),
            addressed_prefix_raw_hex: (!message.addressed_prefix_raw.is_empty())
                .then(|| encode_hex(&message.addressed_prefix_raw)),
            raw_hex: encode_hex(&message.raw),
            source_text: message.text,
            rewrite_sites: message.rewrite_sites,
            truncation_sites: message.truncation_sites,
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        })
        .collect::<Vec<_>>();

    Ok(MainTextCatalog {
        schema: CATALOG_SCHEMA.to_owned(),
        resource: RESOURCE_NAME.to_owned(),
        source_packed_sha256: sha256_hex(packed),
        source_decoded_sha256: sha256_hex(decoded),
        decoded_size: decoded.len(),
        load_offset: LOAD_OFFSET,
        renderer_logical_offset: RENDERER_LOGICAL_OFFSET,
        strict_renderer_messages,
        entry_count: entries.len(),
        rewrite_site_count,
        total_slot_bytes,
        diagnostic_candidate_count: diagnostic_candidates.len(),
        resolved_candidate_count,
        excluded_candidate_count,
        unresolved_candidate_count,
        diagnostic_candidates,
        entries,
    })
}

fn strict_renderer_references(decoded: &[u8]) -> Vec<StrictReference> {
    let mut references = Vec::new();
    for offset in 0..decoded.len().saturating_sub(10) {
        if decoded[offset] != 0xbe
            || decoded[offset + 3] != 0xbf
            || decoded[offset + 4] != 0xff
            || decoded[offset + 5] != 0xff
            || decoded[offset + 6] != 0xb0
            || decoded[offset + 8] != 0xe8
        {
            continue;
        }
        let string_logical =
            u16::from_le_bytes([decoded[offset + 1], decoded[offset + 2]]) as usize;
        let Some(string_decoded_offset) = string_logical.checked_sub(LOAD_OFFSET) else {
            continue;
        };
        if message_bytes(decoded, string_decoded_offset).is_none() {
            continue;
        }
        let relative = i16::from_le_bytes([decoded[offset + 9], decoded[offset + 10]]) as isize;
        let renderer = offset as isize + 11 + relative;
        if renderer < 0 || renderer as usize >= decoded.len() {
            continue;
        }
        references.push(StrictReference {
            renderer_decoded_offset: renderer as usize,
            string_decoded_offset,
            rewrite_site: offset + 1,
        });
    }
    references
}

fn dominant_renderer(references: &[StrictReference]) -> Option<usize> {
    let mut counts = BTreeMap::<usize, usize>::new();
    for reference in references {
        *counts.entry(reference.renderer_decoded_offset).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(renderer, _)| renderer)
}

fn unified_messages(decoded: &[u8], strict: &[StrictReference]) -> Result<Vec<Message>> {
    let strict_starts = strict
        .iter()
        .map(|reference| reference.string_decoded_offset)
        .collect::<HashSet<_>>();
    let run_starts = scan_sjis_runs(decoded, 2)
        .into_iter()
        .filter(|run| run.kana_count > 0)
        .map(|run| run.offset);
    let damage_reaction_text_offsets = DAMAGE_REACTION_RECORD_OFFSETS.map(|offset| offset + 1);
    let mut starts = nul_delimited_kana_starts(decoded, 1);
    starts.extend(strict_starts.iter().copied());
    starts.extend(run_starts);
    starts.extend(EXPLICITLY_VERIFIED_MESSAGE_OFFSETS.iter().copied());
    starts.extend(damage_reaction_text_offsets);

    let mut text_spans = strict_starts
        .iter()
        .filter_map(|offset| {
            message_bytes(decoded, *offset).map(|raw| (*offset, *offset + raw.len() + 1))
        })
        .collect::<Vec<_>>();
    for offset in nul_delimited_kana_starts(decoded, 1) {
        if let Some(raw) = message_bytes(decoded, offset) {
            text_spans.push((offset, offset + raw.len() + 1));
        }
    }
    for &offset in EXPLICITLY_VERIFIED_MESSAGE_OFFSETS {
        if let Some(raw) = message_bytes(decoded, offset) {
            text_spans.push((offset, offset + raw.len() + 1));
        }
    }
    for offset in damage_reaction_text_offsets {
        if let Some(raw) = message_bytes(decoded, offset) {
            text_spans.push((offset - 1, offset + raw.len() + 1));
        }
    }
    text_spans.sort_unstable();
    text_spans.dedup();

    let mut by_string = BTreeMap::<usize, Message>::new();
    for reference in strict {
        insert_message(
            &mut by_string,
            decoded,
            reference.string_decoded_offset,
            MainTextRewriteSite {
                decoded_offset: reference.rewrite_site,
                kind: MainTextPointerKind::MovSi,
                string_offset_from_target: 0,
            },
        );
    }

    if decoded.len() >= 3 {
        for offset in 0..=decoded.len() - 3 {
            if decoded[offset] != 0xbe
                || text_spans
                    .iter()
                    .any(|&(start, end)| offset < end && start < offset + 3)
            {
                continue;
            }
            let logical = u16::from_le_bytes([decoded[offset + 1], decoded[offset + 2]]) as usize;
            if let Some(target) = logical.checked_sub(LOAD_OFFSET)
                && starts.contains(&target)
            {
                insert_message(
                    &mut by_string,
                    decoded,
                    target,
                    MainTextRewriteSite {
                        decoded_offset: offset + 1,
                        kind: MainTextPointerKind::MovSi,
                        string_offset_from_target: 0,
                    },
                );
            }
        }
    }

    ensure_bytes(
        decoded,
        0x3a20,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0xbe, 0xfa, 0x3a, 0x80, 0xfa, 0x00, 0x74, 0x1a, 0x83, 0xc6,
            0x04, 0xd0, 0xea, 0x73, 0xf4, 0x41, 0x51, 0x52, 0x56, 0x8b, 0x34, 0xbf, 0xff, 0xff,
            0xb0, 0x00, 0xe8, 0xaf, 0x13,
        ],
        "sound configuration renderer consumer",
    )?;
    add_consumer_record_table(
        &mut by_string,
        decoded,
        SOUND_CONFIGURATION_RECORD_TABLE_OFFSET,
        SOUND_CONFIGURATION_RECORD_STRIDE,
        SOUND_CONFIGURATION_RECORD_COUNT,
        5,
        5,
        "sound configuration record table",
    )?;

    ensure_bytes(
        decoded,
        0x4b6b,
        &[
            0x8a, 0x1e, 0xfd, 0x4c, 0xd1, 0xeb, 0xd1, 0xeb, 0xd1, 0xeb, 0x83, 0xe3, 0x1e, 0x8b,
            0xb7, 0x02, 0x4d, 0xff, 0x36, 0x70, 0x9e, 0xc6, 0x06, 0x70, 0x9e, 0x57, 0xbf, 0xff,
            0xff, 0xb0, 0x00, 0xe8, 0x63, 0x02,
        ],
        "disk error renderer consumer",
    )?;
    add_consumer_record_table(
        &mut by_string,
        decoded,
        DISK_ERROR_POINTER_TABLE_OFFSET,
        2,
        DISK_ERROR_POINTER_COUNT,
        16,
        16,
        "disk error pointer table",
    )?;

    ensure_bytes(
        decoded,
        0x3be7,
        &[
            0x8a, 0xdc, 0xd1, 0xeb, 0x83, 0xe3, 0x78, 0x8b, 0xb7, 0xd4, 0x59, 0xbf, 0xff, 0xff,
            0xb0, 0x00, 0xe8,
        ],
        "spell record renderer consumer",
    )?;
    add_consumer_record_table(
        &mut by_string,
        decoded,
        SPELL_RECORD_TABLE_OFFSET,
        SPELL_RECORD_STRIDE,
        SPELL_RECORD_COUNT,
        16,
        16,
        "spell record table",
    )?;
    ensure_bytes(
        decoded,
        0x2f09,
        &[
            0x25, 0x3f, 0x00, 0x8b, 0xf8, 0xd1, 0xe7, 0xd1, 0xe7, 0x03, 0xf8, 0xd1, 0xe7, 0x2e,
            0xff, 0xb5, 0x54, 0x5a,
        ],
        "item record renderer consumer",
    )?;
    add_consumer_record_table(
        &mut by_string,
        decoded,
        ITEM_RECORD_TABLE_OFFSET,
        ITEM_RECORD_STRIDE,
        ITEM_RECORD_COUNT,
        51,
        42,
        "item record table",
    )?;
    ensure_bytes(
        decoded,
        0x35cd,
        &[
            0x26, 0x8b, 0x77, 0x38, 0xff, 0x14, 0x8b, 0x74, 0x06, 0xbf, 0xff, 0xff, 0xb0, 0x01,
            0xe8, 0x12, 0x18,
        ],
        "current companion display-name renderer consumer",
    )?;
    ensure_bytes(
        decoded,
        CURRENT_COMPANION_DESCRIPTOR_LOGICAL_OFFSET - LOAD_OFFSET,
        &[0x0b, 0x88, 0x0c, 0x88, 0x0b, 0x88, 0x52, 0x63],
        "current companion action descriptor",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        CURRENT_COMPANION_DISPLAY_NAME_POINTER_SITE,
        CURRENT_COMPANION_DISPLAY_NAME_MESSAGE_OFFSET,
        MainTextPointerKind::Table,
        "current companion display-name descriptor field",
    )?;

    ensure_bytes(
        decoded,
        0x0faf,
        &[
            0xbe, 0x1e, 0x00, 0x3b, 0xc1, 0x73, 0x15, 0x33, 0xd2, 0x8a, 0xd4, 0xc1, 0xe0, 0x04,
            0xc1, 0xea, 0x04, 0xf7, 0xf1, 0x3d, 0x10, 0x00, 0x73, 0x04, 0xd1, 0xe0, 0x8b, 0xf0,
        ],
        "damage-reaction record selector",
    )?;
    ensure_bytes(
        decoded,
        0x0fcb,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0x56, 0x8b, 0xb4, 0x11, 0x11, 0x8a, 0x2c, 0x46, 0xb1, 0x01,
            0xe8, 0x4b, 0xf3, 0x56, 0xb0, 0x05, 0xe8, 0x00, 0xf5, 0xb4, 0x08, 0xcd, 0x7d, 0x5e,
            0x59, 0x56, 0xba, 0x03, 0x01, 0xb4, 0x02, 0xcd, 0x7b, 0x8a, 0xe8, 0xfe, 0xc1, 0xb8,
            0x56, 0x02, 0xcd, 0x7d, 0xb0, 0x01, 0xe8, 0x4d, 0x09, 0x8c, 0xc8, 0x8e, 0xd8, 0x5e,
            0xbf, 0xff, 0xff, 0xb0, 0x02, 0xe8, 0xe5, 0x3d,
        ],
        "damage-reaction prefixed-text renderer consumer",
    )?;
    add_damage_reaction_record_references(&mut by_string, decoded)?;

    ensure_bytes(
        decoded,
        0x3653,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0xbe, 0xa1, 0x41, 0xbb, 0x3e, 0x37, 0xe8, 0x5b, 0x09,
        ],
        "command menu caller",
    )?;
    ensure_bytes(
        decoded,
        0x3fbb,
        &[
            0x1e, 0x56, 0xff, 0x37, 0x83, 0xc3, 0x02, 0x53, 0x6a, 0x00, 0xe8, 0x63, 0x10, 0x8b,
            0xec, 0x8a, 0x46, 0x05, 0xe8, 0x94, 0x12, 0x8b, 0xec, 0xc5, 0x76, 0x06, 0xbf, 0xff,
            0xff, 0xb0, 0x00, 0xe8, 0x13, 0x0e,
        ],
        "command and volume menu renderer",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x3658,
        COMMAND_MENU_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "command menu message",
    )?;
    ensure_bytes(
        decoded,
        0x00dc,
        &[0x2e, 0xc6, 0x06, 0x47, 0x42, 0x00],
        "system menu conditional truncation",
    )?;
    add_verified_message_truncation_site(
        &mut by_string,
        decoded,
        0x00df,
        SYSTEM_MENU_MESSAGE_OFFSET,
        6,
        "system menu exit-line truncation",
    )?;

    ensure_bytes(
        decoded,
        0x1725,
        &[
            0x56, 0xb0, 0x00, 0xe8, 0x39, 0x3b, 0x8c, 0xc8, 0x8e, 0xd8, 0x5e, 0xbf, 0xff, 0xff,
            0xb0, 0x00, 0xe8, 0xb8, 0x36,
        ],
        "save-data title renderer",
    )?;
    for (site, target, kind, label) in [
        (
            0x154a,
            LOAD_DATA_TITLE_MESSAGE_OFFSET,
            MainTextPointerKind::MovSi,
            "load-data title message",
        ),
        (
            0x1643,
            SAVE_DATA_TITLE_MESSAGE_OFFSET,
            MainTextPointerKind::MovSi,
            "save-data title message",
        ),
        (
            0x1585,
            LOAD_CONFIRMATION_MESSAGE_OFFSET,
            MainTextPointerKind::MovSi,
            "load confirmation message",
        ),
        (
            0x166c,
            OVERWRITE_CONFIRMATION_MESSAGE_OFFSET,
            MainTextPointerKind::MovSi,
            "overwrite confirmation message",
        ),
    ] {
        add_verified_immediate_reference(&mut by_string, decoded, site, target, kind, label)?;
    }
    ensure_bytes(
        decoded,
        0x1549,
        &[
            0xbe, 0x27, 0x43, 0xe8, 0xd6, 0x01, 0x73, 0x08, 0xe8, 0xba, 0x02, 0xe8, 0xc2, 0x04,
            0xf8, 0xc3,
        ],
        "load-data title caller",
    )?;
    ensure_bytes(
        decoded,
        0x1642,
        &[
            0xbe, 0x41, 0x43, 0xe8, 0xdd, 0x00, 0x73, 0x08, 0xe8, 0xc1, 0x01, 0xe8, 0xc9, 0x03,
            0xf8, 0xc3,
        ],
        "save-data title caller",
    )?;
    ensure_bytes(
        decoded,
        0x1584,
        &[
            0xbe, 0x5b, 0x43, 0x8b, 0x3e, 0x94, 0x9e, 0x81, 0xc7, 0x00, 0x1e, 0x53, 0xb0, 0x02,
            0xe8, 0x5b, 0x38,
        ],
        "load confirmation renderer caller",
    )?;
    ensure_bytes(
        decoded,
        0x166b,
        &[
            0xbe, 0x78, 0x43, 0x8b, 0x3e, 0x94, 0x9e, 0x81, 0xc7, 0x00, 0x1e, 0x53, 0xb0, 0x02,
            0xe8, 0x74, 0x37,
        ],
        "overwrite confirmation renderer caller",
    )?;

    ensure_bytes(
        decoded,
        0x39bc,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0xbe, 0x71, 0x42, 0xbb, 0xb4, 0x3a, 0xe8, 0xf2, 0x05, 0xb0,
            0x02, 0xe8, 0x96, 0x18, 0xf8, 0xc3,
        ],
        "volume menu helper caller",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x39c1,
        VOLUME_MENU_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "volume menu message",
    )?;

    ensure_bytes(
        decoded,
        0x1791,
        &[
            0x26, 0x8b, 0x75, 0x10, 0x0b, 0xf6, 0x75, 0x0b, 0xbe, 0x17, 0x43, 0xbf, 0xff, 0xff,
            0xb0, 0x00, 0xe9, 0x4c, 0x36,
        ],
        "unused data-slot fallback renderer consumer",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x179a,
        UNUSED_DATA_SLOT_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "unused data-slot fallback message",
    )?;

    ensure_bytes(
        decoded,
        0x3757,
        &[
            0x25, 0x0f, 0x00, 0xc1, 0xe0, 0x03, 0x8b, 0xd0, 0x81, 0xc2, 0x80, 0x00, 0x8b, 0xf0,
            0x8b, 0xb4, 0xd4, 0x5c,
        ],
        "dungeon floor record selector",
    )?;
    ensure_bytes(
        decoded,
        0x37bc,
        &[
            0x5e, 0x8c, 0xc8, 0x8e, 0xd8, 0x8b, 0xb4, 0xd8, 0x5c, 0xbf, 0x32, 0x0f, 0xb0, 0x00,
            0xe8, 0x23, 0x16,
        ],
        "dungeon floor label renderer consumer",
    )?;
    add_dungeon_floor_label_references(&mut by_string, decoded)?;

    ensure_bytes(
        decoded,
        0x65eb,
        &[
            0x73, 0x0e, 0x83, 0xc4, 0x04, 0xbe, 0x17, 0x68, 0xbf, 0xff, 0xff, 0xb0, 0x02, 0xe9,
            0xf5, 0xe7,
        ],
        "full-inventory tail renderer consumer",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x65f1,
        FULL_INVENTORY_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "full-inventory message",
    )?;

    ensure_bytes(
        decoded,
        0x7352,
        &[0xb4, 0x01, 0xcd, 0x7b, 0xbe, 0x8b, 0x75, 0xe8, 0x95, 0x00],
        "umbrella-fall message helper caller",
    )?;
    ensure_bytes(
        decoded,
        0x73f1,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0xbf, 0xff, 0xff, 0xb0, 0x02, 0xe8, 0xf3, 0xd9, 0xb0, 0x00,
            0xe8, 0x74, 0xa4, 0xb0, 0x00, 0xe8, 0x5d, 0xde, 0x8c, 0xc8, 0x8e, 0xd8, 0xc3,
        ],
        "umbrella-fall renderer helper",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x7357,
        UMBRELLA_FALL_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "umbrella-fall message",
    )?;

    ensure_bytes(
        decoded,
        0x7519,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0xbf, 0xff, 0xff, 0xb0, 0x08, 0xbe, 0xb5, 0x76, 0xe8, 0xc8,
            0xd8,
        ],
        "pause message renderer consumer",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x7523,
        PAUSE_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "pause message",
    )?;

    ensure_bytes(
        decoded,
        0x9917,
        &[
            0x26, 0xc6, 0x47, 0x01, 0x00, 0xb4, 0x07, 0xcd, 0x7a, 0x3c, 0x64, 0xbe, 0x47, 0x9a,
            0x72, 0x01, 0xc3, 0x56, 0x32, 0xc0, 0xe8, 0x36, 0xb9, 0x8c, 0xc8, 0x8e, 0xd8, 0x5e,
            0xbf, 0xff, 0xff, 0xb0, 0x01, 0xe8, 0xb5, 0xb4,
        ],
        "pollen-allergy effect message consumer",
    )?;
    add_verified_immediate_reference(
        &mut by_string,
        decoded,
        0x9923,
        POLLEN_ALLERGY_MESSAGE_OFFSET,
        MainTextPointerKind::MovSi,
        "pollen-allergy effect message",
    )?;

    ensure_bytes(
        decoded,
        0x24ca,
        &[
            0xb4, 0x02, 0xcd, 0x7b, 0x8b, 0xf0, 0x83, 0xe6, 0x0f, 0xd1, 0xe6, 0x8b, 0xb4, 0x40,
            0x58,
        ],
        "scenario selection record table consumer",
    )?;
    ensure_bytes(
        decoded,
        0x2520,
        &[
            0x8a, 0xdc, 0xc1, 0xeb, 0x03, 0x83, 0xe3, 0x1e, 0x60, 0xad, 0xc7, 0x06, 0xda, 0x9e,
            0xff, 0xff, 0xff, 0x97, 0x84, 0x65, 0xfc, 0x61,
        ],
        "scenario selection record dispatcher",
    )?;
    ensure_bytes(
        decoded,
        0x6496,
        &[0x50, 0x79],
        "scenario selection prompt handler table entry",
    )?;
    ensure_bytes(
        decoded,
        0x7850,
        &[
            0x50, 0x33, 0xc0, 0xe8, 0x0e, 0xda, 0xe8, 0xcb, 0xd7, 0x5e, 0x8c, 0xc8, 0x8e, 0xd8,
            0xff, 0x34, 0x83, 0xc6, 0x02, 0xbf, 0xff, 0xff, 0xb0, 0x02, 0xe8, 0x85, 0xd5,
        ],
        "scenario selection prefixed-text renderer",
    )?;
    for (site, selector, record, data_filename, expected_filename, label) in [
        (
            0x577e,
            0x9056,
            0x62cc,
            0x6335,
            b"0:S01.DAT".as_slice(),
            "first scenario selection record",
        ),
        (
            0x586c,
            0x9050,
            0x62d7,
            0x633f,
            b"0:S02.DAT".as_slice(),
            "second scenario selection record",
        ),
    ] {
        add_verified_prefixed_record_text(
            &mut by_string,
            decoded,
            site,
            selector,
            record,
            data_filename,
            expected_filename,
            label,
        )?;
    }

    ensure_bytes(
        decoded,
        0x8024,
        &[
            0xbe, 0x50, 0x8e, 0xbf, 0x61, 0x8e, 0xe8, 0x2a, 0xb4, 0x2e, 0xa1, 0x3c, 0x9f, 0x40,
            0x8e, 0xd8, 0x8b, 0x36, 0x0a, 0x00, 0xbf, 0xff, 0xff, 0xb0, 0x01, 0xe8, 0xb0, 0xcd,
        ],
        "recovered sanity event consumer",
    )?;
    ensure_bytes(
        decoded,
        0x805d,
        &[
            0xbe, 0x94, 0x8e, 0xbf, 0xb8, 0x8e, 0xe8, 0xf1, 0xb3, 0x2e, 0xa1, 0x3c, 0x9f, 0x40,
            0x8e, 0xd8, 0x8b, 0x36, 0x0a, 0x00, 0xbf, 0xff, 0xff, 0xb0, 0x01, 0xe8, 0x77, 0xcd,
        ],
        "awakened event consumer",
    )?;
    ensure_bytes(
        decoded,
        0x849a,
        &[
            0xb9, 0x03, 0x02, 0xbe, 0xd6, 0x91, 0xbf, 0x61, 0x8e, 0xe8, 0x0d, 0xb9, 0xe8, 0xae,
            0xaf,
        ],
        "recovered sanity battle consumer",
    )?;
    ensure_bytes(
        decoded,
        0x80a8,
        &[
            0x26, 0xc7, 0x87, 0x84, 0x02, 0xe8, 0x8e, 0x26, 0xc7, 0x87, 0x86, 0x02, 0x20, 0x8f,
            0x26, 0xc7, 0x87, 0x88, 0x02, 0x32, 0x00,
        ],
        "reflected damage event consumer",
    )?;
    ensure_bytes(
        decoded,
        0x31fa,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0x26, 0x8b, 0xb7, 0x84, 0x02, 0xbf, 0xff, 0xff, 0xb0, 0x01,
            0xe8, 0xe5, 0x1b,
        ],
        "timed status initial-message renderer consumer",
    )?;
    ensure_bytes(
        decoded,
        0x3254,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0x26, 0x8b, 0xb7, 0x86, 0x02, 0xbf, 0xff, 0xff, 0xb0, 0x01,
            0xe8, 0x8b, 0x1b,
        ],
        "timed status expiry-message renderer consumer",
    )?;
    for (site, target, kind, label) in [
        (
            0x8028,
            RECOVERED_SANITY_MESSAGE_OFFSET,
            MainTextPointerKind::MovDi,
            "recovered sanity event message",
        ),
        (
            0x84a1,
            RECOVERED_SANITY_MESSAGE_OFFSET,
            MainTextPointerKind::MovDi,
            "recovered sanity battle message",
        ),
        (
            0x8061,
            AWAKENED_MESSAGE_OFFSET,
            MainTextPointerKind::MovDi,
            "awakened event message",
        ),
        (
            0x80ad,
            REFLECTED_DAMAGE_MESSAGE_OFFSET,
            MainTextPointerKind::StoredPointer,
            "reflected damage event message",
        ),
    ] {
        add_verified_immediate_reference(&mut by_string, decoded, site, target, kind, label)?;
    }
    add_verified_continuation_prefix_reference(
        &mut by_string,
        decoded,
        0x80b4,
        REFLECTED_DAMAGE_EXPIRY_MESSAGE_OFFSET,
        REFLECTED_DAMAGE_EXPIRY_SUFFIX_OFFSET,
        "reflected damage expiry message prefix",
    )?;

    for table in find_pointer_tables(decoded, &starts) {
        for (site, target) in table {
            insert_message(
                &mut by_string,
                decoded,
                target,
                MainTextRewriteSite {
                    decoded_offset: site,
                    kind: MainTextPointerKind::Table,
                    string_offset_from_target: 0,
                },
            );
        }
    }
    exclude_internal_save_data_signature(&mut by_string, decoded)?;

    let mut messages = by_string.into_values().collect::<Vec<_>>();
    for message in &mut messages {
        message
            .rewrite_sites
            .sort_by_key(|site| site.decoded_offset);
        message
            .rewrite_sites
            .dedup_by_key(|site| site.decoded_offset);
    }
    validate_messages(decoded, &messages)?;
    Ok(messages)
}

fn add_verified_immediate_reference(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
    site: usize,
    target: usize,
    kind: MainTextPointerKind,
    label: &str,
) -> Result<()> {
    let expected = (target + LOAD_OFFSET) as u16;
    let actual = decoded
        .get(site..site + 2)
        .with_context(|| format!("MAIN.OVL {label} pointer is missing"))?;
    ensure!(
        actual == expected.to_le_bytes(),
        "MAIN.OVL {label} pointer changed at {site:#x}"
    );
    ensure!(
        message_bytes(decoded, target).is_some(),
        "MAIN.OVL {label} has no bounded text target"
    );
    insert_message(
        messages,
        decoded,
        target,
        MainTextRewriteSite {
            decoded_offset: site,
            kind,
            string_offset_from_target: 0,
        },
    );
    Ok(())
}

fn add_verified_message_truncation_site(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
    site: usize,
    target: usize,
    line_number: usize,
    label: &str,
) -> Result<()> {
    let message = messages
        .get_mut(&target)
        .with_context(|| format!("MAIN.OVL {label} has no verified message target"))?;
    let line_offset = message_line_start_offset(&message.raw, line_number)
        .with_context(|| format!("MAIN.OVL {label} line {line_number} is missing"))?;
    let expected = u16::try_from(target + line_offset + LOAD_OFFSET)
        .with_context(|| format!("MAIN.OVL {label} target exceeds the code segment"))?;
    ensure!(
        decoded.get(site..site + 2) == Some(expected.to_le_bytes().as_slice()),
        "MAIN.OVL {label} target changed at {site:#x}"
    );
    message.truncation_sites.push(MainTextTruncationSite {
        decoded_offset: site,
        line_number,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_verified_prefixed_record_text(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
    site: usize,
    selector: u16,
    record_target: usize,
    data_filename_target: usize,
    expected_filename: &[u8],
    label: &str,
) -> Result<()> {
    let expected_record = u16::try_from(record_target + LOAD_OFFSET)
        .with_context(|| format!("MAIN.OVL {label} record target exceeds the code segment"))?;
    let expected_filename_pointer = u16::try_from(data_filename_target + LOAD_OFFSET)
        .with_context(|| format!("MAIN.OVL {label} filename target exceeds the code segment"))?;
    ensure!(
        site >= 2
            && decoded.get(site - 2..site) == Some(selector.to_le_bytes().as_slice())
            && decoded.get(site..site + 2) == Some(expected_record.to_le_bytes().as_slice()),
        "MAIN.OVL {label} selector or record pointer changed"
    );
    let prefix = decoded
        .get(record_target..record_target + 2)
        .with_context(|| format!("MAIN.OVL {label} prefix is missing"))?;
    ensure!(
        prefix == expected_filename_pointer.to_le_bytes(),
        "MAIN.OVL {label} data filename pointer changed"
    );
    ensure!(
        message_bytes(decoded, data_filename_target) == Some(expected_filename),
        "MAIN.OVL {label} data filename changed"
    );

    let string_target = record_target + prefix.len();
    let raw = message_bytes(decoded, string_target)
        .with_context(|| format!("MAIN.OVL {label} has no bounded text"))?
        .to_vec();
    let (text, _, had_errors) = SHIFT_JIS.decode(&raw);
    ensure!(!had_errors, "MAIN.OVL {label} text is not valid Shift-JIS");
    let text = text.into_owned();
    let message = messages.entry(string_target).or_insert_with(|| Message {
        string_decoded_offset: string_target,
        string_logical_offset: string_target + LOAD_OFFSET,
        continuation_decoded_offset: None,
        addressed_prefix_raw: prefix.to_vec(),
        raw,
        text,
        rewrite_sites: Vec::new(),
        truncation_sites: Vec::new(),
    });
    ensure!(
        message.addressed_prefix_raw == prefix,
        "MAIN.OVL {label} conflicts with another addressed prefix"
    );
    message.rewrite_sites.push(MainTextRewriteSite {
        decoded_offset: site,
        kind: MainTextPointerKind::PrefixedRecord,
        string_offset_from_target: prefix.len(),
    });
    Ok(())
}

fn add_verified_continuation_prefix_reference(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
    site: usize,
    target: usize,
    continuation: usize,
    label: &str,
) -> Result<()> {
    let expected = u16::try_from(target + LOAD_OFFSET)
        .with_context(|| format!("MAIN.OVL {label} pointer exceeds the code segment"))?;
    let actual = decoded
        .get(site..site + 2)
        .with_context(|| format!("MAIN.OVL {label} pointer is missing"))?;
    ensure!(
        actual == expected.to_le_bytes(),
        "MAIN.OVL {label} pointer changed at {site:#x}"
    );
    ensure!(
        target < continuation && messages.contains_key(&continuation),
        "MAIN.OVL {label} has no verified continuation"
    );
    let raw = decoded
        .get(target..continuation)
        .with_context(|| format!("MAIN.OVL {label} bytes are missing"))?
        .to_vec();
    let (text, _, had_errors) = SHIFT_JIS.decode(&raw);
    ensure!(!had_errors, "MAIN.OVL {label} is not valid Shift-JIS");
    let old = messages.insert(
        target,
        Message {
            string_decoded_offset: target,
            string_logical_offset: target + LOAD_OFFSET,
            continuation_decoded_offset: Some(continuation),
            addressed_prefix_raw: Vec::new(),
            text: text.into_owned(),
            raw,
            rewrite_sites: vec![MainTextRewriteSite {
                decoded_offset: site,
                kind: MainTextPointerKind::StoredPointer,
                string_offset_from_target: 0,
            }],
            truncation_sites: Vec::new(),
        },
    );
    ensure!(old.is_none(), "MAIN.OVL {label} duplicates a message start");
    Ok(())
}

fn add_dungeon_floor_label_references(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
) -> Result<()> {
    for (record_index, target) in DUNGEON_FLOOR_MESSAGE_OFFSETS.into_iter().enumerate() {
        let record = DUNGEON_FLOOR_RECORD_TABLE_OFFSET
            .checked_add(record_index * DUNGEON_FLOOR_RECORD_STRIDE)
            .context("MAIN.OVL dungeon floor record offset overflow")?;
        let label_site = record + 4;
        let filename_site = record + 6;
        let expected_label = u16::try_from(target + LOAD_OFFSET)
            .context("MAIN.OVL dungeon floor label exceeds the code segment")?;
        ensure!(
            decoded.get(label_site..label_site + 2)
                == Some(expected_label.to_le_bytes().as_slice()),
            "MAIN.OVL dungeon floor label pointer changed in record {record_index}"
        );
        ensure!(
            decoded.get(filename_site..filename_site + 2)
                == Some(
                    DUNGEON_RESOURCE_FILENAME_LOGICAL_OFFSET
                        .to_le_bytes()
                        .as_slice()
                ),
            "MAIN.OVL dungeon floor filename pointer changed in record {record_index}"
        );
        insert_message(
            messages,
            decoded,
            target,
            MainTextRewriteSite {
                decoded_offset: label_site,
                kind: MainTextPointerKind::Table,
                string_offset_from_target: 0,
            },
        );
    }
    Ok(())
}

fn add_damage_reaction_record_references(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
) -> Result<()> {
    for (record_index, (&record_target, &expected_prefix)) in DAMAGE_REACTION_RECORD_OFFSETS
        .iter()
        .zip(DAMAGE_REACTION_RECORD_PREFIXES.iter())
        .enumerate()
    {
        let site = DAMAGE_REACTION_RECORD_TABLE_OFFSET + record_index * 2;
        let expected_record = u16::try_from(record_target + LOAD_OFFSET)
            .context("MAIN.OVL damage-reaction record exceeds the code segment")?;
        ensure!(
            decoded.get(site..site + 2) == Some(expected_record.to_le_bytes().as_slice()),
            "MAIN.OVL damage-reaction record pointer changed at index {record_index}"
        );
        ensure!(
            decoded.get(record_target) == Some(&expected_prefix),
            "MAIN.OVL damage-reaction display prefix changed at index {record_index}"
        );

        let string_target = record_target + 1;
        let raw = message_bytes(decoded, string_target)
            .with_context(|| {
                format!("MAIN.OVL damage-reaction record {record_index} has no bounded message")
            })?
            .to_vec();
        let (text, _, had_errors) = SHIFT_JIS.decode(&raw);
        ensure!(
            !had_errors,
            "MAIN.OVL damage-reaction message {record_index} is not valid Shift-JIS"
        );
        let message = messages.entry(string_target).or_insert_with(|| Message {
            string_decoded_offset: string_target,
            string_logical_offset: string_target + LOAD_OFFSET,
            continuation_decoded_offset: None,
            addressed_prefix_raw: vec![expected_prefix],
            raw: raw.clone(),
            text: text.into_owned(),
            rewrite_sites: Vec::new(),
            truncation_sites: Vec::new(),
        });
        ensure!(
            message.addressed_prefix_raw == [expected_prefix] && message.raw == raw,
            "MAIN.OVL damage-reaction record {record_index} conflicts with another message"
        );
        message.rewrite_sites.push(MainTextRewriteSite {
            decoded_offset: site,
            kind: MainTextPointerKind::PrefixedRecord,
            string_offset_from_target: 1,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_consumer_record_table(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
    table_offset: usize,
    record_stride: usize,
    record_count: usize,
    expected_reference_count: usize,
    expected_message_count: usize,
    label: &str,
) -> Result<()> {
    let table_end = table_offset
        .checked_add(record_stride * record_count)
        .context("MAIN.OVL consumer record table overflow")?;
    ensure!(table_end <= decoded.len(), "{label} lies outside MAIN.OVL");
    let mut references = 0usize;
    let mut targets = BTreeSet::new();
    for record_index in 0..record_count {
        let site = table_offset + record_index * record_stride;
        let logical = u16::from_le_bytes([decoded[site], decoded[site + 1]]) as usize;
        let target = logical.checked_sub(LOAD_OFFSET).with_context(|| {
            format!("{label} record {record_index} points below the load offset")
        })?;
        let raw = message_bytes(decoded, target)
            .with_context(|| format!("{label} record {record_index} has no bounded text target"))?;
        if raw.is_empty() {
            continue;
        }
        insert_message(
            messages,
            decoded,
            target,
            MainTextRewriteSite {
                decoded_offset: site,
                kind: MainTextPointerKind::Table,
                string_offset_from_target: 0,
            },
        );
        references += 1;
        targets.insert(target);
    }
    ensure!(
        references == expected_reference_count && targets.len() == expected_message_count,
        "{label} population changed: found {references} references to {} messages",
        targets.len()
    );
    Ok(())
}

fn insert_message(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
    target: usize,
    site: MainTextRewriteSite,
) {
    let message = messages.entry(target).or_insert_with(|| {
        let raw = message_bytes(decoded, target).unwrap_or_default().to_vec();
        Message {
            string_decoded_offset: target,
            string_logical_offset: target + LOAD_OFFSET,
            continuation_decoded_offset: None,
            addressed_prefix_raw: Vec::new(),
            text: SHIFT_JIS.decode(&raw).0.into_owned(),
            raw,
            rewrite_sites: Vec::new(),
            truncation_sites: Vec::new(),
        }
    });
    message.rewrite_sites.push(site);
}

fn exclude_internal_save_data_signature(
    messages: &mut BTreeMap<usize, Message>,
    decoded: &[u8],
) -> Result<()> {
    ensure_bytes(
        decoded,
        0x0167,
        &[
            0xfc, 0xb4, 0x01, 0xcd, 0x7b, 0x8c, 0xc8, 0x8e, 0xd8, 0xbe, 0xc0, 0x57, 0x8b, 0xfb,
            0xb9, 0x07, 0x00, 0xf3, 0xa7, 0x74, 0x01, 0xc3, 0x8c, 0xc0, 0x8e, 0xd8, 0x8b, 0xf7,
            0xad, 0x8b, 0xd0, 0xbb, 0x7a, 0x9f, 0xb9, 0xf8, 0x01, 0xad, 0x03, 0xd8, 0xe2, 0xfb,
            0x3b, 0xd3, 0xc3,
        ],
        "internal save-data signature validator",
    )?;
    ensure_bytes(
        decoded,
        0x0194,
        &[
            0xb4, 0x01, 0xcd, 0x7b, 0x8c, 0xc8, 0x8e, 0xd8, 0xba, 0xc0, 0x57, 0xb4, 0x00, 0xcd,
            0x7b,
        ],
        "internal save-data signature initializer",
    )?;

    let signature_logical_offset =
        u16::try_from(INTERNAL_SAVE_DATA_SIGNATURE_MESSAGE_OFFSET + LOAD_OFFSET)
            .context("MAIN.OVL internal save-data signature lies beyond the code segment")?;
    let address_word_sites = decoded
        .windows(2)
        .enumerate()
        .filter_map(|(offset, bytes)| {
            (bytes == signature_logical_offset.to_le_bytes()).then_some(offset)
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        address_word_sites == BTreeSet::from([0x0171, 0x019d]),
        "MAIN.OVL internal save-data signature address references changed"
    );

    let signature = messages
        .remove(&INTERNAL_SAVE_DATA_SIGNATURE_MESSAGE_OFFSET)
        .context("MAIN.OVL internal save-data signature was not found by the text scan")?;
    ensure!(
        signature.rewrite_sites
            == [MainTextRewriteSite {
                decoded_offset: 0x0171,
                kind: MainTextPointerKind::MovSi,
                string_offset_from_target: 0,
            }],
        "MAIN.OVL internal save-data signature acquired a text consumer"
    );
    Ok(())
}

fn find_pointer_tables(decoded: &[u8], starts: &HashSet<usize>) -> Vec<Vec<(usize, usize)>> {
    let mut candidates = Vec::<Vec<(usize, usize)>>::new();
    for stride in TABLE_STRIDES {
        let mut site = 0usize;
        while site + 2 <= decoded.len() {
            if let Some(target) = pointer_target(decoded, site, starts) {
                let begins_run =
                    site < stride || pointer_target(decoded, site - stride, starts).is_none();
                if begins_run {
                    let mut entries = vec![(site, target)];
                    let mut next = site + stride;
                    while let Some(target) = pointer_target(decoded, next, starts) {
                        entries.push((next, target));
                        next += stride;
                    }
                    if entries.len() >= MIN_TABLE_ENTRIES {
                        candidates.push(entries);
                    }
                }
            }
            site += 1;
        }
    }
    candidates.sort_by(|left, right| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left[0].0.cmp(&right[0].0))
    });
    let mut covered_targets = HashSet::new();
    let mut accepted = Vec::new();
    for entries in candidates {
        if entries
            .iter()
            .all(|(_, target)| covered_targets.contains(target))
        {
            continue;
        }
        covered_targets.extend(entries.iter().map(|(_, target)| *target));
        accepted.push(entries);
    }
    accepted.sort_by_key(|entries| entries[0].0);
    accepted
}

fn pointer_target(decoded: &[u8], site: usize, starts: &HashSet<usize>) -> Option<usize> {
    if overlaps_consumer_record_table(site) {
        return None;
    }
    let bytes = decoded.get(site..site + 2)?;
    let logical = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    let target = logical.checked_sub(LOAD_OFFSET)?;
    starts.contains(&target).then_some(target)
}

fn overlaps_consumer_record_table(site: usize) -> bool {
    [
        (
            SOUND_CONFIGURATION_RECORD_TABLE_OFFSET,
            SOUND_CONFIGURATION_RECORD_TABLE_OFFSET
                + SOUND_CONFIGURATION_RECORD_STRIDE * SOUND_CONFIGURATION_RECORD_COUNT,
        ),
        (
            DISK_ERROR_POINTER_TABLE_OFFSET,
            DISK_ERROR_POINTER_TABLE_OFFSET + 2 * DISK_ERROR_POINTER_COUNT,
        ),
        (
            SPELL_RECORD_TABLE_OFFSET,
            SPELL_RECORD_TABLE_OFFSET + SPELL_RECORD_STRIDE * SPELL_RECORD_COUNT,
        ),
        (
            ITEM_RECORD_TABLE_OFFSET,
            ITEM_RECORD_TABLE_OFFSET + ITEM_RECORD_STRIDE * ITEM_RECORD_COUNT,
        ),
        (
            DUNGEON_FLOOR_RECORD_TABLE_OFFSET,
            DUNGEON_FLOOR_RECORD_TABLE_OFFSET
                + DUNGEON_FLOOR_RECORD_STRIDE * DUNGEON_FLOOR_RECORD_COUNT,
        ),
        (
            DAMAGE_REACTION_RECORD_TABLE_OFFSET,
            DAMAGE_REACTION_RECORD_TABLE_OFFSET + 2 * DAMAGE_REACTION_RECORD_OFFSETS.len(),
        ),
    ]
    .into_iter()
    .any(|(start, end)| site < end && start < site.saturating_add(2))
}

fn build_main_text_diagnostic_candidates(
    decoded: &[u8],
    messages: &[Message],
) -> Result<Vec<MainTextDiagnosticCandidate>> {
    let mut message_starts = HashSet::new();
    for message in messages {
        message_starts.insert(message.string_decoded_offset);
        message_starts.insert(
            message
                .string_decoded_offset
                .checked_sub(message.addressed_prefix_raw.len())
                .context("MAIN.OVL addressed message prefix lies before the overlay")?,
        );
    }
    let first_spell_pointer: [u8; 2] = decoded
        .get(SPELL_RECORD_TABLE_OFFSET..SPELL_RECORD_TABLE_OFFSET + 2)
        .context("MAIN.OVL first spell pointer is missing")?
        .try_into()
        .expect("a two-byte range converts to an array");
    let first_spell_target = usize::from(u16::from_le_bytes(first_spell_pointer))
        .checked_sub(LOAD_OFFSET)
        .context("MAIN.OVL first spell pointer lies below the load offset")?;
    ensure!(
        first_spell_target == SPELL_FIRST_DISPLAY_PREFIX_OFFSET + 1,
        "MAIN.OVL first spell display prefix changed"
    );
    let candidate_offsets = DIAGNOSTIC_CANDIDATE_OFFSETS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let resolved_entries = RESOLVED_DIAGNOSTIC_CANDIDATE_ENTRIES
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let resolved_offsets = resolved_entries.keys().copied().collect::<BTreeSet<_>>();
    let excluded_offsets = EXCLUDED_DISK_ERROR_CANDIDATE_OFFSETS
        .into_iter()
        .collect::<BTreeSet<_>>();
    let disabled_item_effect_offsets = disabled_item_effect_candidate_offsets(decoded)?;
    let item_effect_different_message_offsets =
        BTreeSet::from([unselected_walking_recovery_message_offset(
            decoded, messages,
        )?]);
    let timed_status_different_expiry_offsets =
        BTreeSet::from([unselected_barrier_expiry_message_offset(decoded, messages)?]);
    let companion_current_action_descriptor_offsets =
        unselected_companion_action_candidate_offsets(decoded, messages)?;
    let internal_save_data_signature_offsets =
        BTreeSet::from([INTERNAL_SAVE_DATA_SIGNATURE_MESSAGE_OFFSET]);
    let discovered_offsets = nul_delimited_kana_starts(decoded, 2)
        .into_iter()
        .chain(NON_KANA_DIAGNOSTIC_CANDIDATE_OFFSETS)
        .filter(|offset| {
            !message_starts.contains(offset)
                && !resolved_offsets.contains(offset)
                && *offset != SPELL_FIRST_DISPLAY_PREFIX_OFFSET
        })
        .collect::<BTreeSet<_>>();
    ensure!(
        resolved_offsets.is_subset(&candidate_offsets)
            && excluded_offsets.is_subset(&candidate_offsets)
            && disabled_item_effect_offsets.is_subset(&candidate_offsets)
            && item_effect_different_message_offsets.is_subset(&candidate_offsets)
            && timed_status_different_expiry_offsets.is_subset(&candidate_offsets)
            && companion_current_action_descriptor_offsets.is_subset(&candidate_offsets)
            && resolved_offsets.is_disjoint(&excluded_offsets)
            && resolved_offsets.is_disjoint(&disabled_item_effect_offsets)
            && resolved_offsets.is_disjoint(&item_effect_different_message_offsets)
            && resolved_offsets.is_disjoint(&timed_status_different_expiry_offsets)
            && resolved_offsets.is_disjoint(&companion_current_action_descriptor_offsets)
            && excluded_offsets.is_disjoint(&disabled_item_effect_offsets)
            && excluded_offsets.is_disjoint(&item_effect_different_message_offsets)
            && excluded_offsets.is_disjoint(&timed_status_different_expiry_offsets)
            && excluded_offsets.is_disjoint(&companion_current_action_descriptor_offsets)
            && disabled_item_effect_offsets.is_disjoint(&item_effect_different_message_offsets)
            && disabled_item_effect_offsets.is_disjoint(&timed_status_different_expiry_offsets)
            && disabled_item_effect_offsets
                .is_disjoint(&companion_current_action_descriptor_offsets)
            && item_effect_different_message_offsets
                .is_disjoint(&timed_status_different_expiry_offsets)
            && item_effect_different_message_offsets
                .is_disjoint(&companion_current_action_descriptor_offsets)
            && timed_status_different_expiry_offsets
                .is_disjoint(&companion_current_action_descriptor_offsets),
        "MAIN.OVL diagnostic candidate classifications are inconsistent"
    );
    ensure!(
        internal_save_data_signature_offsets.is_subset(&candidate_offsets)
            && internal_save_data_signature_offsets.is_disjoint(&resolved_offsets)
            && internal_save_data_signature_offsets.is_disjoint(&excluded_offsets)
            && internal_save_data_signature_offsets.is_disjoint(&disabled_item_effect_offsets)
            && internal_save_data_signature_offsets
                .is_disjoint(&item_effect_different_message_offsets)
            && internal_save_data_signature_offsets
                .is_disjoint(&timed_status_different_expiry_offsets)
            && internal_save_data_signature_offsets
                .is_disjoint(&companion_current_action_descriptor_offsets),
        "MAIN.OVL internal save-data signature classification is inconsistent"
    );
    for (&candidate_offset, &entry_offset) in &resolved_entries {
        let message = messages
            .iter()
            .find(|message| message.string_decoded_offset == entry_offset)
            .with_context(|| {
                format!(
                    "MAIN.OVL resolved diagnostic candidate at {candidate_offset:#x} has no entry"
                )
            })?;
        ensure!(
            candidate_offset + message.addressed_prefix_raw.len() == entry_offset,
            "MAIN.OVL resolved diagnostic candidate at {candidate_offset:#x} lost its entry anchor"
        );
    }
    let expected_unlinked_offsets = candidate_offsets
        .difference(&resolved_offsets)
        .copied()
        .collect::<BTreeSet<_>>();
    ensure!(
        discovered_offsets == expected_unlinked_offsets,
        "MAIN.OVL diagnostic candidate population changed: found {} instead of {}",
        discovered_offsets.len(),
        expected_unlinked_offsets.len()
    );

    let mut candidate_ids = HashSet::new();
    DIAGNOSTIC_CANDIDATE_OFFSETS
        .into_iter()
        .map(|offset| {
            let raw = message_bytes(decoded, offset)
                .with_context(|| {
                    format!("MAIN.OVL diagnostic candidate at {offset:#x} is unbounded")
                })?;
            let (decoded_text, _, had_errors) = SHIFT_JIS.decode(raw);
            ensure!(
                !had_errors,
                "MAIN.OVL diagnostic candidate at {offset:#x} is not valid Shift-JIS"
            );
            let digest = sha256_hex(raw).to_ascii_uppercase();
            let id = format!("MAIN_DIAGNOSTIC_{}", &digest[..12]);
            ensure!(
                candidate_ids.insert(id.clone()),
                "MAIN.OVL diagnostic candidate ID collision for {id}"
            );

            let (state, classification_basis, linked_entry_id, next_action) = if let Some(
                entry_offset,
            ) =
                resolved_entries.get(&offset)
            {
                (
                    MainTextCandidateState::Resolved,
                    MainTextCandidateClassificationBasis::VerifiedPrefixedRecordConsumer,
                    Some(format!("MAIN_{:04X}", *entry_offset + LOAD_OFFSET)),
                    None,
                )
            } else if EXCLUDED_DISK_ERROR_CANDIDATE_OFFSETS.contains(&offset) {
                validate_unreferenced_disk_error_tail(decoded, offset)?;
                (
                    MainTextCandidateState::Excluded,
                    MainTextCandidateClassificationBasis::OutsideBoundedDiskErrorPointerTable,
                    None,
                    None,
                )
            } else if disabled_item_effect_offsets.contains(&offset) {
                (
                    MainTextCandidateState::Excluded,
                    MainTextCandidateClassificationBasis::DisabledItemEffectHandler,
                    None,
                    None,
                )
            } else if item_effect_different_message_offsets.contains(&offset) {
                (
                    MainTextCandidateState::Excluded,
                    MainTextCandidateClassificationBasis::ItemEffectSelectsDifferentMessage,
                    None,
                    None,
                )
            } else if timed_status_different_expiry_offsets.contains(&offset) {
                (
                    MainTextCandidateState::Excluded,
                    MainTextCandidateClassificationBasis::TimedStatusSelectsDifferentExpiryMessage,
                    None,
                    None,
                )
            } else if companion_current_action_descriptor_offsets.contains(&offset) {
                (
                    MainTextCandidateState::Excluded,
                    MainTextCandidateClassificationBasis::CompanionRecordsSelectCurrentActionDescriptor,
                    None,
                    None,
                )
            } else if internal_save_data_signature_offsets.contains(&offset) {
                (
                    MainTextCandidateState::Excluded,
                    MainTextCandidateClassificationBasis::InternalSaveDataSignature,
                    None,
                    None,
                )
            } else {
                let next_action = if offset < 0x8e00 {
                    MainTextCandidateNextAction::ClassifyDataBeforeResourceFilenameTable
                } else {
                    MainTextCandidateNextAction::TraceStatusOrBattleDispatchConsumer
                };
                (
                    MainTextCandidateState::Unresolved,
                    MainTextCandidateClassificationBasis::NoVerifiedConsumer,
                    None,
                    Some(next_action),
                )
            };

            Ok(MainTextDiagnosticCandidate {
                id,
                string_decoded_offset: offset,
                string_logical_offset: offset + LOAD_OFFSET,
                byte_budget: raw.len() + 1,
                raw_hex: encode_hex(raw),
                decoded_text: decoded_text.into_owned(),
                state,
                classification_basis,
                linked_entry_id,
                next_action,
                ko: String::new(),
                status: UNTRANSLATED_STATUS.to_owned(),
                notes: String::new(),
            })
        })
        .collect()
}

fn validate_item_effect_selector_and_dispatcher(decoded: &[u8]) -> Result<()> {
    ensure_bytes(
        decoded,
        0x3d9d,
        &[
            0x83, 0xe1, 0x3f, 0x8b, 0xd9, 0xd1, 0xe3, 0xd1, 0xe3, 0x03, 0xd9, 0xd1, 0xe3, 0x81,
            0xc3, 0x54, 0x5a,
        ],
        "item record selector",
    )?;
    ensure_bytes(
        decoded,
        0x2d04,
        &[0x5b, 0x5a, 0x68, 0xc0, 0x2e, 0xff, 0x67, 0x02],
        "item effect handler dispatcher",
    )
}

fn disabled_item_effect_candidate_offsets(decoded: &[u8]) -> Result<BTreeSet<usize>> {
    validate_item_effect_selector_and_dispatcher(decoded)?;

    let mut offsets = BTreeSet::new();
    for &(record_index, handler_logical_offset, candidate_offsets) in
        DISABLED_ITEM_EFFECT_CANDIDATE_GROUPS
    {
        let handler_site = ITEM_RECORD_TABLE_OFFSET + record_index * ITEM_RECORD_STRIDE + 2;
        let handler = decoded
            .get(handler_site..handler_site + 2)
            .with_context(|| format!("MAIN.OVL item record {record_index} handler is missing"))?;
        ensure!(
            handler == (handler_logical_offset as u16).to_le_bytes(),
            "MAIN.OVL item record {record_index} no longer selects the disabled effect handler"
        );
        let handler_offset = handler_logical_offset
            .checked_sub(LOAD_OFFSET)
            .context("MAIN.OVL disabled item handler lies below the load offset")?;
        ensure!(
            decoded.get(handler_offset) == Some(&0xc3),
            "MAIN.OVL item record {record_index} effect handler is no longer an immediate return"
        );

        for &candidate_offset in candidate_offsets {
            let candidate_logical_offset = u16::try_from(candidate_offset + LOAD_OFFSET)
                .context("MAIN.OVL disabled item text lies beyond the code segment")?;
            ensure!(
                message_bytes(decoded, candidate_offset).is_some_and(|raw| !raw.is_empty()),
                "MAIN.OVL disabled item text at {candidate_logical_offset:#06x} is missing"
            );
            ensure!(
                !decoded
                    .windows(2)
                    .any(|bytes| bytes == candidate_logical_offset.to_le_bytes()),
                "MAIN.OVL disabled item text at {candidate_logical_offset:#06x} acquired an address reference"
            );
            ensure!(
                offsets.insert(candidate_offset),
                "MAIN.OVL disabled item text at {candidate_logical_offset:#06x} has conflicting records"
            );
        }
    }
    Ok(offsets)
}

fn unselected_walking_recovery_message_offset(
    decoded: &[u8],
    messages: &[Message],
) -> Result<usize> {
    validate_item_effect_selector_and_dispatcher(decoded)?;

    let record_site =
        ITEM_RECORD_TABLE_OFFSET + WALKING_RECOVERY_ITEM_RECORD_INDEX * ITEM_RECORD_STRIDE;
    ensure_bytes(
        decoded,
        record_site,
        &[0x6e, 0x62, 0x6c, 0x84, 0x16, 0x00, 0x20, 0xff, 0x20, 0x03],
        "walking recovery item record",
    )?;
    ensure_bytes(
        decoded,
        0x8373,
        &[
            0x26, 0xc7, 0x87, 0x20, 0x02, 0x41, 0x00, 0x26, 0xc7, 0x87, 0x22, 0x02, 0xb5, 0x84,
        ],
        "walking recovery event installation",
    )?;
    ensure_bytes(
        decoded,
        CONSUMABLE_ITEM_EFFECT_LOGICAL_OFFSET - LOAD_OFFSET + 0x3e,
        &[
            0xbe, 0x71, 0x90, 0xbf, 0xff, 0xff, 0xb0, 0x01, 0xe9, 0x3b, 0xca,
        ],
        "consumable item message render",
    )?;
    ensure_bytes(
        decoded,
        WALKING_RECOVERY_EVENT_LOGICAL_OFFSET - LOAD_OFFSET,
        &[0x26, 0xff, 0x4f, 0x04, 0x74, 0x01, 0xc3],
        "walking recovery event",
    )?;

    let selected_message = messages
        .iter()
        .find(|message| message.string_decoded_offset == CONSUMABLE_ITEM_MESSAGE_OFFSET)
        .context("consumable item message has no verified renderer consumer")?;
    ensure!(
        selected_message.rewrite_sites.iter().any(|site| {
            site.decoded_offset == 0x83ab && site.kind == MainTextPointerKind::MovSi
        }),
        "consumable item message lost its direct renderer reference"
    );

    let candidate_offset = UNSELECTED_WALKING_RECOVERY_MESSAGE_OFFSET;
    let candidate_logical_offset = u16::try_from(candidate_offset + LOAD_OFFSET)
        .context("MAIN.OVL unselected walking recovery text lies beyond the code segment")?;
    ensure!(
        message_bytes(decoded, candidate_offset).is_some_and(|raw| !raw.is_empty()),
        "MAIN.OVL unselected walking recovery text at {candidate_logical_offset:#06x} is missing"
    );
    ensure!(
        messages
            .iter()
            .all(|message| message.string_decoded_offset != candidate_offset),
        "MAIN.OVL walking recovery text at {candidate_logical_offset:#06x} acquired a renderer consumer"
    );
    let address_word_sites = decoded
        .windows(2)
        .enumerate()
        .filter_map(|(site, bytes)| {
            (bytes == candidate_logical_offset.to_le_bytes()).then_some(site)
        })
        .collect::<Vec<_>>();
    ensure!(
        address_word_sites == [0x6575],
        "MAIN.OVL walking recovery text at {candidate_logical_offset:#06x} acquired an address-word site"
    );
    ensure_bytes(
        decoded,
        0x6574,
        &[0xeb, 0x4e, 0x90],
        "non-reference bytes matching the walking recovery text address",
    )?;
    Ok(candidate_offset)
}

fn unselected_barrier_expiry_message_offset(decoded: &[u8], messages: &[Message]) -> Result<usize> {
    ensure_bytes(
        decoded,
        0x80a8,
        &[
            0x26, 0xc7, 0x87, 0x84, 0x02, 0xe8, 0x8e, 0x26, 0xc7, 0x87, 0x86, 0x02, 0x20, 0x8f,
            0x26, 0xc7, 0x87, 0x88, 0x02, 0x32, 0x00,
        ],
        "reflected damage event message selection",
    )?;
    ensure_bytes(
        decoded,
        0x3254,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0x26, 0x8b, 0xb7, 0x86, 0x02, 0xbf, 0xff, 0xff, 0xb0, 0x01,
            0xe8, 0x8b, 0x1b,
        ],
        "timed status expiry-message renderer consumer",
    )?;

    let selected_message = messages
        .iter()
        .find(|message| message.string_decoded_offset == REFLECTED_DAMAGE_EXPIRY_MESSAGE_OFFSET)
        .context("reflected damage expiry message has no verified renderer consumer")?;
    ensure!(
        selected_message.continuation_decoded_offset == Some(REFLECTED_DAMAGE_EXPIRY_SUFFIX_OFFSET)
            && selected_message.rewrite_sites.iter().any(|site| {
                site.decoded_offset == 0x80b4 && site.kind == MainTextPointerKind::StoredPointer
            }),
        "reflected damage expiry message lost its stored-pointer consumer"
    );

    let candidate_offset = UNSELECTED_BARRIER_EXPIRY_MESSAGE_OFFSET;
    let candidate_logical_offset = u16::try_from(candidate_offset + LOAD_OFFSET)
        .context("MAIN.OVL unselected barrier expiry text lies beyond the code segment")?;
    let candidate_raw = message_bytes(decoded, candidate_offset).with_context(|| {
        format!(
            "MAIN.OVL unselected barrier expiry text at {candidate_logical_offset:#06x} is missing"
        )
    })?;
    ensure!(
        candidate_offset + candidate_raw.len() + 1 == REFLECTED_DAMAGE_EXPIRY_MESSAGE_OFFSET,
        "MAIN.OVL unselected barrier expiry text no longer precedes the selected expiry message"
    );
    ensure!(
        messages
            .iter()
            .all(|message| message.string_decoded_offset != candidate_offset),
        "MAIN.OVL barrier expiry text at {candidate_logical_offset:#06x} acquired a renderer consumer"
    );
    ensure!(
        !decoded
            .windows(2)
            .any(|bytes| bytes == candidate_logical_offset.to_le_bytes()),
        "MAIN.OVL barrier expiry text at {candidate_logical_offset:#06x} acquired an address-word site"
    );
    Ok(candidate_offset)
}

fn unselected_companion_action_candidate_offsets(
    decoded: &[u8],
    messages: &[Message],
) -> Result<BTreeSet<usize>> {
    validate_item_effect_selector_and_dispatcher(decoded)?;
    ensure_bytes(
        decoded,
        0x3557,
        &[
            0x8c, 0xc8, 0x8e, 0xd8, 0x8b, 0x77, 0x04, 0x8a, 0x6f, 0x07, 0xb1, 0x00, 0xb4, 0x01,
            0xcd, 0x7b,
        ],
        "companion descriptor selector",
    )?;
    ensure_bytes(
        decoded,
        0x3594,
        &[
            0xff, 0x54, 0x04, 0xff, 0x74, 0x06, 0x26, 0x8b, 0x77, 0x38, 0xff, 0x14, 0x58, 0xff,
            0x74, 0x06,
        ],
        "companion descriptor replacement consumer",
    )?;
    ensure_bytes(
        decoded,
        0x35cd,
        &[
            0x26, 0x8b, 0x77, 0x38, 0xff, 0x14, 0x8b, 0x74, 0x06, 0xbf, 0xff, 0xff, 0xb0, 0x01,
        ],
        "companion descriptor current-action consumer",
    )?;
    ensure_bytes(
        decoded,
        0x3612,
        &[
            0x26, 0x8b, 0x77, 0x38, 0x0b, 0xf6, 0x74, 0x1c, 0x26, 0xff, 0x77, 0x3a, 0x2e, 0xff,
            0x54, 0x02,
        ],
        "companion descriptor battle-action consumer",
    )?;

    for record_index in CURRENT_COMPANION_RECORD_RANGE.clone() {
        let record_site = ITEM_RECORD_TABLE_OFFSET + record_index * ITEM_RECORD_STRIDE;
        ensure_bytes(
            decoded,
            record_site,
            &[0x52, 0x63, 0x57, 0x36, 0x39, 0x87, 0xa0, 0xff, 0x00, 0x00],
            "current companion item record",
        )?;
    }
    ensure_bytes(
        decoded,
        CURRENT_COMPANION_DESCRIPTOR_LOGICAL_OFFSET - LOAD_OFFSET,
        &[0x0b, 0x88, 0x0c, 0x88, 0x0b, 0x88, 0x52, 0x63],
        "current companion action descriptor",
    )?;
    ensure!(
        decoded.get(0x880b - LOAD_OFFSET) == Some(&0xc3),
        "current companion descriptor cleanup action is no longer an immediate return"
    );
    ensure_bytes(
        decoded,
        0x880c - LOAD_OFFSET,
        &[0xb4, 0x01, 0xcd, 0x7b, 0x26, 0xc6, 0x87, 0xa5, 0x01, 0x00],
        "current companion battle action",
    )?;

    for (message_offset, rewrite_site) in CURRENT_COMPANION_ACTION_MESSAGES {
        let message = messages
            .iter()
            .find(|message| message.string_decoded_offset == message_offset)
            .with_context(|| {
                format!(
                    "current companion action message at {:#06x} has no verified consumer",
                    message_offset + LOAD_OFFSET
                )
            })?;
        ensure!(
            message.rewrite_sites.iter().any(|site| {
                site.decoded_offset == rewrite_site && site.kind == MainTextPointerKind::MovSi
            }),
            "current companion action message at {:#06x} lost its direct renderer reference",
            message_offset + LOAD_OFFSET
        );
    }

    let mut offsets = BTreeSet::new();
    for candidate_offset in UNSELECTED_COMPANION_ACTION_CANDIDATE_OFFSETS {
        let candidate_logical_offset = candidate_offset + LOAD_OFFSET;
        ensure!(
            message_bytes(decoded, candidate_offset).is_some_and(|raw| !raw.is_empty()),
            "MAIN.OVL unselected companion action text at {candidate_logical_offset:#06x} is missing"
        );
        ensure!(
            messages
                .iter()
                .all(|message| message.string_decoded_offset != candidate_offset),
            "MAIN.OVL companion action text at {candidate_logical_offset:#06x} acquired a renderer consumer"
        );
        ensure!(
            offsets.insert(candidate_offset),
            "MAIN.OVL companion action text at {candidate_logical_offset:#06x} is duplicated"
        );
    }
    Ok(offsets)
}

fn validate_in_place_candidate_slots(
    decoded: &[u8],
    messages: &[Message],
    candidates: &[MainTextDiagnosticCandidate],
) -> Result<()> {
    let message_spans = messages
        .iter()
        .map(|message| {
            let start = message
                .string_decoded_offset
                .checked_sub(message.addressed_prefix_raw.len())
                .context("MAIN.OVL addressed message prefix underflow")?;
            let end = message.string_decoded_offset
                + message.raw.len()
                + usize::from(message.continuation_decoded_offset.is_none());
            Ok((start, end))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut candidate_spans = Vec::new();
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.state == MainTextCandidateState::Unresolved)
    {
        let raw = message_bytes(decoded, candidate.string_decoded_offset)
            .with_context(|| format!("{} has no bounded in-place slot", candidate.id))?;
        ensure!(
            candidate.byte_budget == raw.len() + 1,
            "{} in-place slot budget changed",
            candidate.id
        );
        let end = candidate
            .string_decoded_offset
            .checked_add(candidate.byte_budget)
            .context("MAIN.OVL candidate slot overflow")?;
        ensure!(
            decoded
                .get(end - 1)
                .is_some_and(|byte| is_terminator(*byte)),
            "{} in-place slot lost its renderer terminator",
            candidate.id
        );
        ensure!(
            !message_spans.iter().any(|&(start, message_end)| {
                candidate.string_decoded_offset < message_end && start < end
            }),
            "{} in-place slot overlaps a consumer-linked message",
            candidate.id
        );
        candidate_spans.push((candidate.string_decoded_offset, end));
    }
    candidate_spans.sort_unstable();
    for pair in candidate_spans.windows(2) {
        ensure!(
            pair[1].0 >= pair[0].1,
            "MAIN.OVL in-place candidate slots overlap at 0x{:04X} and 0x{:04X}",
            pair[0].0,
            pair[1].0
        );
    }
    Ok(())
}

fn validate_unreferenced_disk_error_tail(decoded: &[u8], candidate_offset: usize) -> Result<()> {
    const LAST_DISK_ERROR_MESSAGE_OFFSET: usize = 0x4d43;

    let table_end = DISK_ERROR_POINTER_TABLE_OFFSET + 2 * DISK_ERROR_POINTER_COUNT;
    let table = decoded
        .get(DISK_ERROR_POINTER_TABLE_OFFSET..table_end)
        .context("MAIN.OVL disk error pointer table is missing")?;
    let targets = table
        .chunks_exact(2)
        .map(|raw| usize::from(u16::from_le_bytes([raw[0], raw[1]])))
        .collect::<Vec<_>>();
    ensure!(
        targets.last() == Some(&(LAST_DISK_ERROR_MESSAGE_OFFSET + LOAD_OFFSET)),
        "MAIN.OVL disk error pointer table no longer ends at the F0 message"
    );
    let candidate_logical_offset = candidate_offset + LOAD_OFFSET;
    ensure!(
        !targets.contains(&candidate_logical_offset),
        "MAIN.OVL excluded disk error tail entered the bounded pointer table"
    );

    let preceding_offset = match candidate_offset {
        0x4d64 => LAST_DISK_ERROR_MESSAGE_OFFSET,
        0x4d77 => 0x4d64,
        _ => bail!("unsupported excluded disk error tail at {candidate_offset:#x}"),
    };
    let preceding = message_bytes(decoded, preceding_offset)
        .context("MAIN.OVL preceding disk error tail message is unbounded")?;
    ensure!(
        preceding_offset + preceding.len() + 1 == candidate_offset,
        "MAIN.OVL excluded disk error tail is no longer adjacent"
    );

    let logical_word = u16::try_from(candidate_logical_offset)
        .context("MAIN.OVL disk error tail lies outside the code segment")?
        .to_le_bytes();
    ensure!(
        !decoded
            .windows(logical_word.len())
            .any(|window| window == logical_word),
        "MAIN.OVL excluded disk error tail gained a word reference"
    );
    Ok(())
}

fn ensure_bytes(decoded: &[u8], offset: usize, expected: &[u8], label: &str) -> Result<()> {
    ensure!(
        decoded.get(offset..offset + expected.len()) == Some(expected),
        "MAIN.OVL {label} changed at {offset:#x}"
    );
    Ok(())
}

fn validate_messages(decoded: &[u8], messages: &[Message]) -> Result<()> {
    let mut spans = Vec::new();
    let mut site_owners = BTreeMap::<usize, usize>::new();
    for message in messages {
        ensure!(
            !message.rewrite_sites.is_empty(),
            "message 0x{:04X} has no verified rewrite site",
            message.string_decoded_offset
        );
        let end = message.string_decoded_offset + message.raw.len();
        ensure!(
            decoded.get(message.string_decoded_offset..end) == Some(message.raw.as_slice()),
            "message 0x{:04X} raw bytes changed",
            message.string_decoded_offset
        );
        let span_start = message
            .string_decoded_offset
            .checked_sub(message.addressed_prefix_raw.len())
            .context("MAIN.OVL addressed message prefix underflow")?;
        ensure!(
            decoded.get(span_start..message.string_decoded_offset)
                == Some(message.addressed_prefix_raw.as_slice()),
            "message 0x{:04X} addressed prefix changed",
            message.string_decoded_offset
        );
        let span_end = if let Some(continuation) = message.continuation_decoded_offset {
            ensure!(
                continuation == end
                    && messages
                        .iter()
                        .any(|candidate| candidate.string_decoded_offset == continuation),
                "message prefix 0x{:04X} has no adjacent continuation",
                message.string_decoded_offset
            );
            end
        } else {
            ensure!(
                decoded.get(end).is_some_and(|byte| is_terminator(*byte)),
                "message 0x{:04X} has no renderer terminator",
                message.string_decoded_offset
            );
            end + 1
        };
        spans.push((span_start, span_end));
    }
    spans.sort_unstable();
    for pair in spans.windows(2) {
        ensure!(
            pair[1].0 >= pair[0].1,
            "relocatable MAIN.OVL message slots overlap at 0x{:04X} and 0x{:04X}",
            pair[0].0,
            pair[1].0
        );
    }
    for message in messages {
        for site in &message.rewrite_sites {
            ensure!(
                site.string_offset_from_target == 0
                    || site.string_offset_from_target == message.addressed_prefix_raw.len(),
                "rewrite site 0x{:04X} has no matching addressed prefix",
                site.decoded_offset
            );
            let target_logical_offset = message
                .string_logical_offset
                .checked_sub(site.string_offset_from_target)
                .context("MAIN.OVL rewrite target underflow")?;
            let expected = u16::try_from(target_logical_offset)
                .context("MAIN.OVL rewrite target exceeds the code segment")?
                .to_le_bytes();
            let site_end = site.decoded_offset + 2;
            ensure!(
                decoded.get(site.decoded_offset..site_end) == Some(expected.as_slice()),
                "rewrite site 0x{:04X} does not hold MAIN.OVL message 0x{:04X}",
                site.decoded_offset,
                message.string_decoded_offset
            );
            ensure!(
                !spans
                    .iter()
                    .any(|&(start, end)| site.decoded_offset < end && start < site_end),
                "rewrite site 0x{:04X} overlaps a MAIN.OVL text slot",
                site.decoded_offset
            );
            if let Some(owner) =
                site_owners.insert(site.decoded_offset, message.string_decoded_offset)
            {
                ensure!(
                    owner == message.string_decoded_offset,
                    "rewrite site 0x{:04X} has two message owners",
                    site.decoded_offset
                );
            }
        }
        for site in &message.truncation_sites {
            let line_offset = message_line_start_offset(&message.raw, site.line_number)
                .with_context(|| {
                    format!(
                        "MAIN.OVL message 0x{:04X} has no line {} for truncation site 0x{:04X}",
                        message.string_decoded_offset, site.line_number, site.decoded_offset
                    )
                })?;
            let target_logical_offset = message
                .string_logical_offset
                .checked_add(line_offset)
                .context("MAIN.OVL truncation target overflow")?;
            let expected = u16::try_from(target_logical_offset)
                .context("MAIN.OVL truncation target exceeds the code segment")?
                .to_le_bytes();
            let site_end = site.decoded_offset + 2;
            ensure!(
                decoded.get(site.decoded_offset..site_end) == Some(expected.as_slice()),
                "truncation site 0x{:04X} does not hold MAIN.OVL message 0x{:04X} line {}",
                site.decoded_offset,
                message.string_decoded_offset,
                site.line_number
            );
            ensure!(
                !spans
                    .iter()
                    .any(|&(start, end)| site.decoded_offset < end && start < site_end),
                "truncation site 0x{:04X} overlaps a MAIN.OVL text slot",
                site.decoded_offset
            );
            ensure!(
                site_owners
                    .insert(site.decoded_offset, message.string_decoded_offset)
                    .is_none(),
                "truncation site 0x{:04X} has another message owner",
                site.decoded_offset
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SjisRun {
    offset: usize,
    kana_count: usize,
}

fn scan_sjis_runs(decoded: &[u8], min_double: usize) -> Vec<SjisRun> {
    let mut runs = Vec::new();
    let mut cursor = 0usize;
    while cursor < decoded.len() {
        let start = cursor;
        let mut double_count = 0usize;
        let mut kana_count = 0usize;
        while cursor < decoded.len() {
            let byte = decoded[cursor];
            if is_sjis_lead(byte)
                && cursor + 1 < decoded.len()
                && is_sjis_trail(decoded[cursor + 1])
            {
                if is_sjis_kana(byte, decoded[cursor + 1]) {
                    kana_count += 1;
                }
                double_count += 1;
                cursor += 2;
            } else if (0x20..=0x7e).contains(&byte) || byte == 0x0a {
                cursor += 1;
            } else {
                break;
            }
        }
        if double_count >= min_double {
            runs.push(SjisRun {
                offset: start,
                kana_count,
            });
        }
        if cursor == start {
            cursor += 1;
        }
    }
    runs
}

fn nul_delimited_kana_starts(decoded: &[u8], minimum_kana: usize) -> HashSet<usize> {
    let mut starts = HashSet::new();
    for offset in 0..decoded.len() {
        let follows_real_nul =
            offset > 0 && decoded[offset - 1] == 0 && !is_control_parameter_at(decoded, offset - 1);
        if offset != 0 && !follows_real_nul {
            continue;
        }
        let mut cursor = offset;
        let mut kana_count = 0usize;
        while cursor < decoded.len() {
            let byte = decoded[cursor];
            if is_sjis_lead(byte)
                && cursor + 1 < decoded.len()
                && is_sjis_trail(decoded[cursor + 1])
            {
                if is_sjis_kana(byte, decoded[cursor + 1]) {
                    kana_count += 1;
                }
                cursor += 2;
            } else if (0x20..=0x7e).contains(&byte) || byte == 0x0a {
                cursor += 1;
            } else {
                break;
            }
        }
        if kana_count >= minimum_kana && cursor > offset {
            starts.insert(offset);
        }
    }
    starts
}

fn is_sjis_lead(byte: u8) -> bool {
    (0x81..=0x9f).contains(&byte) || (0xe0..=0xfc).contains(&byte)
}

fn is_sjis_trail(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte) || (0x80..=0xfc).contains(&byte)
}

fn is_sjis_kana(lead: u8, trail: u8) -> bool {
    (lead == 0x82 && (0x9f..=0xf1).contains(&trail))
        || (lead == 0x83 && (0x40..=0x96).contains(&trail))
}

fn control_arity(byte: u8) -> Option<usize> {
    if byte >= 0x20 {
        return None;
    }
    Some(match byte & 0x0f {
        0x01 => 2,
        0x02 | 0x04 | 0x0c => 1,
        _ => 0,
    })
}

fn is_terminator(byte: u8) -> bool {
    byte < 0x20 && byte & 0x0f == 0
}

fn message_bytes(bytes: &[u8], start: usize) -> Option<&[u8]> {
    let mut cursor = start;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        let Some(arity) = control_arity(byte) else {
            cursor += 1;
            continue;
        };
        if is_terminator(byte) {
            return Some(&bytes[start..cursor]);
        }
        cursor = cursor.checked_add(1 + arity)?;
        if cursor > bytes.len() {
            return None;
        }
    }
    None
}

fn is_control_parameter_at(bytes: &[u8], index: usize) -> bool {
    (1..=2).any(|distance| {
        index
            .checked_sub(distance)
            .and_then(|control| control_arity(bytes[control]))
            .is_some_and(|arity| distance <= arity)
    })
}

fn validate_message_body(bytes: &[u8]) -> Result<()> {
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        let Some(arity) = control_arity(byte) else {
            cursor += 1;
            continue;
        };
        ensure!(
            !is_terminator(byte),
            "message body contains terminator 0x{byte:02X} at byte {cursor}"
        );
        let end = cursor + 1 + arity;
        ensure!(
            end <= bytes.len(),
            "truncated renderer control 0x{byte:02X} at byte {cursor}"
        );
        cursor = end;
    }
    Ok(())
}

fn message_line_start_offset(bytes: &[u8], line_number: usize) -> Option<usize> {
    if line_number == 0 {
        return None;
    }
    if line_number == 1 {
        return Some(0);
    }

    let mut current_line = 1usize;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\n' {
            current_line += 1;
            if current_line == line_number {
                return Some(cursor + 1);
            }
            cursor += 1;
            continue;
        }
        if let Some(arity) = control_arity(bytes[cursor]) {
            cursor = cursor.checked_add(1 + arity)?;
            if cursor > bytes.len() {
                return None;
            }
            continue;
        }
        cursor += if is_sjis_lead(bytes[cursor])
            && cursor + 1 < bytes.len()
            && is_sjis_trail(bytes[cursor + 1])
        {
            2
        } else {
            1
        };
    }
    None
}

pub(crate) fn renderer_control_sequences(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    validate_message_body(bytes)?;
    let mut sequences = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(arity) = control_arity(byte) {
            let end = cursor + 1 + arity;
            sequences.push(bytes[cursor..end].to_vec());
            cursor = end;
        } else {
            cursor += 1;
        }
    }
    Ok(sequences)
}

pub(crate) fn ensure_message_avoids_renderer_sheet_codes(bytes: &[u8]) -> Result<()> {
    validate_message_body(bytes)?;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if let Some(arity) = control_arity(bytes[cursor]) {
            cursor += 1 + arity;
            continue;
        }
        if cursor + 1 < bytes.len() && contains_renderer_sheet_code(&bytes[cursor..cursor + 2]) {
            anyhow::bail!(
                "message uses reserved renderer font code {:02X} {:02X} at byte {cursor}",
                bytes[cursor],
                bytes[cursor + 1]
            );
        }
        cursor += if is_sjis_lead(bytes[cursor])
            && cursor + 1 < bytes.len()
            && is_sjis_trail(bytes[cursor + 1])
        {
            2
        } else {
            1
        };
    }
    Ok(())
}

fn validate_protected_catalog(
    expected: &MainTextCatalog,
    supplied: &MainTextCatalog,
) -> Result<()> {
    let mut normalized = supplied.clone();
    for entry in &mut normalized.entries {
        entry.ko.clear();
        entry.status = UNTRANSLATED_STATUS.to_owned();
        entry.notes.clear();
    }
    for candidate in &mut normalized.diagnostic_candidates {
        candidate.ko.clear();
        candidate.status = UNTRANSLATED_STATUS.to_owned();
        candidate.notes.clear();
    }
    ensure!(
        normalized == *expected,
        "MAIN.OVL catalog protected fields differ from the exact source extraction"
    );
    Ok(())
}

fn validate_candidate_translation_states(catalog: &MainTextCatalog) -> Result<()> {
    for candidate in &catalog.diagnostic_candidates {
        if candidate.state != MainTextCandidateState::Unresolved {
            ensure!(
                candidate.ko.is_empty() && candidate.status == UNTRANSLATED_STATUS,
                "{} is not an editable unresolved candidate slot",
                candidate.id
            );
            continue;
        }
        if candidate.ko.is_empty() {
            ensure!(
                candidate.status == UNTRANSLATED_STATUS,
                "{} has no Korean text but status is {:?}",
                candidate.id,
                candidate.status
            );
        } else {
            ensure!(
                candidate.status == DRAFT_STATUS,
                "{} Korean text requires status {DRAFT_STATUS:?}",
                candidate.id
            );
        }
    }
    Ok(())
}

fn write_after_validation(path: &Path, bytes: &[u8], label: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {label} directory: {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("failed to write {label}: {}", path.display()))
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len().is_multiple_of(2),
        "hex string has an odd length"
    );
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex digit {:?}", byte as char),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_controls_skip_parameters_that_look_like_terminators() {
        let message = [0x01, 0x0e, 0x00, 0x04, 0x07, b'A', 0x04, 0x02, 0x00];
        assert_eq!(message_bytes(&message, 0), Some(&message[..8]));
        assert!(is_control_parameter_at(&message, 2));
    }

    #[test]
    fn renderer_sheet_collision_check_ignores_control_parameters() {
        assert!(ensure_message_avoids_renderer_sheet_codes(&[0x01, 0xeb, 0x40]).is_ok());
        assert!(
            ensure_message_avoids_renderer_sheet_codes(&[b'A', 0xeb, 0x40])
                .unwrap_err()
                .to_string()
                .contains("reserved renderer font code")
        );
    }

    #[test]
    fn replacement_validation_rejects_a_terminator_inside_the_body() {
        let error = validate_message_body(b"A\0B").unwrap_err();
        assert!(error.to_string().contains("terminator"));
    }

    #[test]
    fn fixed_cursor_menu_rejects_a_line_without_its_cursor_cell() {
        let error = validate_fixed_cursor_menu_text(
            "MAIN_4271",
            VOLUME_MENU_MESSAGE_OFFSET,
            " 크게\n 작게\n 끄기\r",
        )
        .unwrap_err();

        assert!(error.to_string().contains("full-width leading cell"));
    }

    #[test]
    fn sound_description_rejects_text_beyond_the_menu_width() {
        let error = validate_fixed_cursor_menu_text(
            "MAIN_428D",
            BEEP1_MENU_MESSAGE_OFFSET,
            "　BEEP1(음량 작음､음질 좋음)\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("display columns"));
    }

    #[test]
    fn sound_description_accepts_text_within_the_menu_width() {
        validate_fixed_cursor_menu_text(
            "MAIN_428D",
            BEEP1_MENU_MESSAGE_OFFSET,
            "　BEEP1(저음량､고음질)\n",
        )
        .unwrap();
    }

    #[test]
    fn hex_encoding_is_exact_and_case_tolerant_on_input() {
        assert_eq!(encode_hex(&[0x00, 0x8f, 0xff]), "008fff");
        assert_eq!(decode_hex("008FfF").unwrap(), [0x00, 0x8f, 0xff]);
    }

    #[test]
    fn gaiji_code_conversion_matches_the_pc98_external_character_rows() {
        assert_eq!(jis_to_shift_jis(0x7621).unwrap(), [0xeb, 0x9f]);
        assert_eq!(jis_to_shift_jis(0x767e).unwrap(), [0xeb, 0xfc]);
        assert_eq!(jis_to_shift_jis(0x7721).unwrap(), [0xec, 0x40]);
        assert_eq!(jis_to_shift_jis(0x777e).unwrap(), [0xec, 0x9e]);
    }

    #[test]
    fn main_text_encoder_requires_an_assigned_glyph_for_every_hangul_syllable() {
        let codebook = [('가', [0xeb, 0x9f])].into_iter().collect();
        assert_eq!(
            encode_renderer_text("A가\n", &codebook).unwrap(),
            [b'A', 0xeb, 0x9f, b'\n']
        );
        assert!(
            encode_renderer_text("나", &codebook)
                .unwrap_err()
                .to_string()
                .contains("unassigned Hangul")
        );
    }

    #[test]
    fn bios_gaiji_build_omits_the_direct_japanese_topic_particle() {
        let mut decoded = vec![0_u8; 0x1400];
        for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
            decoded[site..site + expected.len()].copy_from_slice(&expected);
        }

        remove_enemy_status_topic_particle_glyphs(&mut decoded).unwrap();

        for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
            assert_eq!(&decoded[site..site + expected.len()], &[0x90; 6]);
        }
    }

    #[test]
    fn bios_gaiji_particle_removal_rejects_an_unknown_renderer_sequence() {
        let mut decoded = vec![0_u8; 0x1400];
        for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
            decoded[site..site + expected.len()].copy_from_slice(&expected);
        }
        decoded[ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES[1].0] = 0xcc;
        let original = decoded.clone();

        let error = remove_enemy_status_topic_particle_glyphs(&mut decoded).unwrap_err();

        assert!(error.to_string().contains("renderer changed"));
        assert_eq!(decoded, original);
    }

    #[test]
    fn continuing_prefix_replacement_preserves_the_following_message_entry() {
        let mut decoded = b"xxxxTAIL".to_vec();
        let entry = MainTextEntry {
            id: "MAIN_0100".to_owned(),
            string_decoded_offset: 0,
            string_logical_offset: LOAD_OFFSET,
            byte_budget: 4,
            continues_at_logical_offset: Some(LOAD_OFFSET + 4),
            addressed_prefix_raw_hex: None,
            raw_hex: "78787878".to_owned(),
            source_text: "xxxx".to_owned(),
            rewrite_sites: Vec::new(),
            truncation_sites: Vec::new(),
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        };

        write_in_place_text_replacement(&mut decoded, &entry, b"AB").unwrap();

        assert_eq!(&decoded, b"AB  TAIL");
    }

    #[test]
    fn prefixed_record_replacement_preserves_the_addressed_prefix() {
        let mut decoded = b"PRxxxx\0".to_vec();
        let entry = MainTextEntry {
            id: "MAIN_0102".to_owned(),
            string_decoded_offset: 2,
            string_logical_offset: LOAD_OFFSET + 2,
            byte_budget: 5,
            continues_at_logical_offset: None,
            addressed_prefix_raw_hex: Some("5052".to_owned()),
            raw_hex: "78787878".to_owned(),
            source_text: "xxxx".to_owned(),
            rewrite_sites: Vec::new(),
            truncation_sites: Vec::new(),
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        };

        write_in_place_text_replacement(&mut decoded, &entry, b"AB").unwrap();

        assert_eq!(&decoded, b"PRAB\0\0\0");
    }

    #[test]
    fn translated_line_start_moves_conditional_truncation_target() {
        let source = b"A\nBB\nCCC";
        let replacement = b"X\nYYYY\nZ";
        let mut decoded = vec![0_u8; 0x40];
        decoded[0x10..0x12].copy_from_slice(&0x0125_u16.to_le_bytes());
        let entry = MainTextEntry {
            id: "MAIN_0120".to_owned(),
            string_decoded_offset: 0x20,
            string_logical_offset: 0x0120,
            byte_budget: 16,
            continues_at_logical_offset: None,
            addressed_prefix_raw_hex: None,
            raw_hex: encode_hex(source),
            source_text: "A\nBB\nCCC".to_owned(),
            rewrite_sites: Vec::new(),
            truncation_sites: vec![MainTextTruncationSite {
                decoded_offset: 0x10,
                line_number: 3,
            }],
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        };

        rewrite_message_truncation_sites(
            &mut decoded,
            &entry,
            source,
            replacement,
            entry.string_logical_offset,
        )
        .unwrap();

        assert_eq!(&decoded[0x10..0x12], &0x0127_u16.to_le_bytes());
    }

    #[test]
    fn overlong_translation_moves_the_record_and_every_consumer_target() {
        let source = b"A\nBB";
        let replacement = b"LONG\nTEXT";
        let mut decoded = vec![0_u8; 0x40];
        decoded[0x04..0x06].copy_from_slice(&0x0120_u16.to_le_bytes());
        decoded[0x06..0x08].copy_from_slice(&0x011e_u16.to_le_bytes());
        decoded[0x08..0x0a].copy_from_slice(&0x0122_u16.to_le_bytes());
        decoded[0x1e..0x20].copy_from_slice(b"PR");
        decoded[0x20..0x25].copy_from_slice(b"A\nBB\0");
        let entry = MainTextEntry {
            id: "MAIN_0120".to_owned(),
            string_decoded_offset: 0x20,
            string_logical_offset: 0x0120,
            byte_budget: 5,
            continues_at_logical_offset: None,
            addressed_prefix_raw_hex: Some("5052".to_owned()),
            raw_hex: encode_hex(source),
            source_text: "A\nBB".to_owned(),
            rewrite_sites: vec![
                MainTextRewriteSite {
                    decoded_offset: 0x04,
                    kind: MainTextPointerKind::MovSi,
                    string_offset_from_target: 0,
                },
                MainTextRewriteSite {
                    decoded_offset: 0x06,
                    kind: MainTextPointerKind::PrefixedRecord,
                    string_offset_from_target: 2,
                },
            ],
            truncation_sites: vec![MainTextTruncationSite {
                decoded_offset: 0x08,
                line_number: 2,
            }],
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        };

        assert!(apply_main_text_replacement(&mut decoded, &entry, source, replacement).unwrap());

        assert_eq!(&decoded[0x20..0x25], &[0; 5]);
        assert_eq!(&decoded[0x40..0x42], b"PR");
        assert_eq!(&decoded[0x42..0x4b], replacement);
        assert_eq!(decoded[0x4b], 0);
        assert_eq!(&decoded[0x04..0x06], &0x0142_u16.to_le_bytes());
        assert_eq!(&decoded[0x06..0x08], &0x0140_u16.to_le_bytes());
        assert_eq!(&decoded[0x08..0x0a], &0x0147_u16.to_le_bytes());
    }

    #[test]
    fn runtime_relocation_pool_places_payload_after_the_work_buffer() {
        let mut decoded = vec![0xaa; 4];
        let range = 8..16;

        start_runtime_relocation_pool(&mut decoded, range.clone()).unwrap();
        decoded.extend_from_slice(b"KO\0");
        finish_runtime_relocation_pool(&mut decoded, range).unwrap();

        assert_eq!(&decoded[..4], &[0xaa; 4]);
        assert_eq!(&decoded[4..8], &[0; 4]);
        assert_eq!(&decoded[8..11], b"KO\0");
        assert_eq!(&decoded[11..], &[0; 5]);
    }

    #[test]
    fn runtime_relocation_pool_rejects_a_payload_past_its_safe_end() {
        let mut decoded = vec![0xaa; 4];
        let range = 8..12;
        start_runtime_relocation_pool(&mut decoded, range.clone()).unwrap();
        decoded.extend_from_slice(b"TOO-LONG");
        let before = decoded.clone();

        let error = finish_runtime_relocation_pool(&mut decoded, range).unwrap_err();

        assert!(error.to_string().contains("beyond the safe range"));
        assert_eq!(decoded, before);
    }

    #[test]
    fn overlong_continuing_prefix_is_rejected_before_writing() {
        let mut decoded = b"ABCDTAIL".to_vec();
        let original = decoded.clone();
        let entry = MainTextEntry {
            id: "MAIN_0100".to_owned(),
            string_decoded_offset: 0,
            string_logical_offset: LOAD_OFFSET,
            byte_budget: 4,
            continues_at_logical_offset: Some(LOAD_OFFSET + 4),
            addressed_prefix_raw_hex: None,
            raw_hex: "41424344".to_owned(),
            source_text: "ABCD".to_owned(),
            rewrite_sites: Vec::new(),
            truncation_sites: Vec::new(),
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        };

        let error =
            apply_main_text_replacement(&mut decoded, &entry, b"ABCD", b"ABCDE").unwrap_err();

        assert!(error.to_string().contains("continuing prefix slot"));
        assert_eq!(decoded, original);
    }

    #[test]
    fn candidate_slot_replacement_clears_the_old_tail_and_keeps_the_neighbor() {
        let mut decoded = b"abcdef\0NEXT\0".to_vec();

        write_nul_terminated_text_replacement(&mut decoded, 0, 7, "MAIN_DIAGNOSTIC_TEST", b"AB")
            .unwrap();

        assert_eq!(&decoded, b"AB\0\0\0\0\0NEXT\0");
    }

    #[test]
    fn only_unresolved_candidates_accept_draft_text() {
        let mut catalog = MainTextCatalog {
            schema: CATALOG_SCHEMA.to_owned(),
            resource: RESOURCE_NAME.to_owned(),
            source_packed_sha256: String::new(),
            source_decoded_sha256: String::new(),
            decoded_size: 0,
            load_offset: LOAD_OFFSET,
            renderer_logical_offset: RENDERER_LOGICAL_OFFSET,
            strict_renderer_messages: 0,
            entry_count: 0,
            rewrite_site_count: 0,
            total_slot_bytes: 0,
            diagnostic_candidate_count: 1,
            resolved_candidate_count: 0,
            excluded_candidate_count: 0,
            unresolved_candidate_count: 1,
            diagnostic_candidates: vec![MainTextDiagnosticCandidate {
                id: "MAIN_DIAGNOSTIC_TEST".to_owned(),
                string_decoded_offset: 0,
                string_logical_offset: LOAD_OFFSET,
                byte_budget: 3,
                raw_hex: "82a0".to_owned(),
                decoded_text: "あ".to_owned(),
                state: MainTextCandidateState::Unresolved,
                classification_basis: MainTextCandidateClassificationBasis::NoVerifiedConsumer,
                linked_entry_id: None,
                next_action: Some(MainTextCandidateNextAction::TraceStatusOrBattleDispatchConsumer),
                ko: "가".to_owned(),
                status: DRAFT_STATUS.to_owned(),
                notes: String::new(),
            }],
            entries: Vec::new(),
        };

        validate_candidate_translation_states(&catalog).unwrap();
        catalog.diagnostic_candidates[0].state = MainTextCandidateState::Excluded;

        assert!(
            validate_candidate_translation_states(&catalog)
                .unwrap_err()
                .to_string()
                .contains("not an editable unresolved candidate slot")
        );
    }

    #[test]
    fn disabled_item_effect_text_requires_ret_dispatch_and_no_address_reference() {
        let mut decoded = vec![0; 0x9300];
        decoded[0x3d9d..0x3dae].copy_from_slice(&[
            0x83, 0xe1, 0x3f, 0x8b, 0xd9, 0xd1, 0xe3, 0xd1, 0xe3, 0x03, 0xd9, 0xd1, 0xe3, 0x81,
            0xc3, 0x54, 0x5a,
        ]);
        decoded[0x2d04..0x2d0c].copy_from_slice(&[0x5b, 0x5a, 0x68, 0xc0, 0x2e, 0xff, 0x67, 0x02]);
        for &(record_index, handler_logical_offset, candidate_offsets) in
            DISABLED_ITEM_EFFECT_CANDIDATE_GROUPS
        {
            let handler_site = ITEM_RECORD_TABLE_OFFSET + record_index * ITEM_RECORD_STRIDE + 2;
            decoded[handler_site..handler_site + 2]
                .copy_from_slice(&(handler_logical_offset as u16).to_le_bytes());
            decoded[handler_logical_offset - LOAD_OFFSET] = 0xc3;
            for &candidate_offset in candidate_offsets {
                decoded[candidate_offset..candidate_offset + 3].copy_from_slice(&[0x82, 0xa0, 0]);
            }
        }

        let offsets = disabled_item_effect_candidate_offsets(&decoded).unwrap();
        assert_eq!(offsets.len(), 15);
        assert!(offsets.contains(&0x905e));
        assert!(offsets.contains(&0x9239));

        decoded[0x8598 - LOAD_OFFSET] = 0x90;
        assert!(
            disabled_item_effect_candidate_offsets(&decoded)
                .unwrap_err()
                .to_string()
                .contains("no longer an immediate return")
        );
    }

    #[test]
    fn unselected_item_effect_text_requires_a_different_rendered_message() {
        let mut decoded = vec![0; 0x9000];
        decoded[0x3d9d..0x3dae].copy_from_slice(&[
            0x83, 0xe1, 0x3f, 0x8b, 0xd9, 0xd1, 0xe3, 0xd1, 0xe3, 0x03, 0xd9, 0xd1, 0xe3, 0x81,
            0xc3, 0x54, 0x5a,
        ]);
        decoded[0x2d04..0x2d0c].copy_from_slice(&[0x5b, 0x5a, 0x68, 0xc0, 0x2e, 0xff, 0x67, 0x02]);
        decoded[0x59ae..0x59b8]
            .copy_from_slice(&[0x6e, 0x62, 0x6c, 0x84, 0x16, 0x00, 0x20, 0xff, 0x20, 0x03]);
        decoded[0x8373..0x8381].copy_from_slice(&[
            0x26, 0xc7, 0x87, 0x20, 0x02, 0x41, 0x00, 0x26, 0xc7, 0x87, 0x22, 0x02, 0xb5, 0x84,
        ]);
        decoded[0x83aa..0x83b5].copy_from_slice(&[
            0xbe, 0x71, 0x90, 0xbf, 0xff, 0xff, 0xb0, 0x01, 0xe9, 0x3b, 0xca,
        ]);
        decoded[0x83b5..0x83bc].copy_from_slice(&[0x26, 0xff, 0x4f, 0x04, 0x74, 0x01, 0xc3]);
        decoded[0x6574..0x6577].copy_from_slice(&[0xeb, 0x4e, 0x90]);
        decoded[0x8f4e..0x8f51].copy_from_slice(&[0x82, 0xa0, 0]);
        decoded[0x8f71..0x8f74].copy_from_slice(&[0x82, 0xa2, 0]);
        let selected_message = Message {
            string_decoded_offset: 0x8f71,
            string_logical_offset: 0x9071,
            continuation_decoded_offset: None,
            addressed_prefix_raw: Vec::new(),
            raw: vec![0x82, 0xa2],
            text: "い".to_owned(),
            rewrite_sites: vec![MainTextRewriteSite {
                decoded_offset: 0x83ab,
                kind: MainTextPointerKind::MovSi,
                string_offset_from_target: 0,
            }],
            truncation_sites: Vec::new(),
        };

        let offset = unselected_walking_recovery_message_offset(
            &decoded,
            std::slice::from_ref(&selected_message),
        )
        .unwrap();
        assert_eq!(offset, 0x8f4e);

        let mut messages = vec![selected_message];
        messages.push(Message {
            string_decoded_offset: 0x8f4e,
            string_logical_offset: 0x904e,
            continuation_decoded_offset: None,
            addressed_prefix_raw: Vec::new(),
            raw: vec![0x82, 0xa0],
            text: "あ".to_owned(),
            rewrite_sites: vec![MainTextRewriteSite {
                decoded_offset: 0x100,
                kind: MainTextPointerKind::MovSi,
                string_offset_from_target: 0,
            }],
            truncation_sites: Vec::new(),
        });
        assert!(
            unselected_walking_recovery_message_offset(&decoded, &messages)
                .unwrap_err()
                .to_string()
                .contains("acquired a renderer consumer")
        );
    }

    #[test]
    fn unselected_barrier_expiry_requires_the_stored_timed_status_message() {
        let mut decoded = vec![0; 0x8e40];
        decoded[0x80a8..0x80bd].copy_from_slice(&[
            0x26, 0xc7, 0x87, 0x84, 0x02, 0xe8, 0x8e, 0x26, 0xc7, 0x87, 0x86, 0x02, 0x20, 0x8f,
            0x26, 0xc7, 0x87, 0x88, 0x02, 0x32, 0x00,
        ]);
        decoded[0x3254..0x3265].copy_from_slice(&[
            0x8c, 0xc8, 0x8e, 0xd8, 0x26, 0x8b, 0xb7, 0x86, 0x02, 0xbf, 0xff, 0xff, 0xb0, 0x01,
            0xe8, 0x8b, 0x1b,
        ]);
        decoded[0x8e0b..0x8e1f].fill(0x82);
        decoded[0x8e20..0x8e2c].fill(0x84);
        let selected_message = Message {
            string_decoded_offset: 0x8e20,
            string_logical_offset: 0x8f20,
            continuation_decoded_offset: Some(0x8e2c),
            addressed_prefix_raw: Vec::new(),
            raw: vec![0x84; 12],
            text: "ばよひひひー".to_owned(),
            rewrite_sites: vec![MainTextRewriteSite {
                decoded_offset: 0x80b4,
                kind: MainTextPointerKind::StoredPointer,
                string_offset_from_target: 0,
            }],
            truncation_sites: Vec::new(),
        };

        assert_eq!(
            unselected_barrier_expiry_message_offset(
                &decoded,
                std::slice::from_ref(&selected_message)
            )
            .unwrap(),
            0x8e0b
        );

        let mut messages = vec![selected_message];
        messages.push(Message {
            string_decoded_offset: 0x8e0b,
            string_logical_offset: 0x8f0b,
            continuation_decoded_offset: None,
            addressed_prefix_raw: Vec::new(),
            raw: vec![0x82; 20],
            text: "結界の　効果が切れた".to_owned(),
            rewrite_sites: vec![MainTextRewriteSite {
                decoded_offset: 0x100,
                kind: MainTextPointerKind::StoredPointer,
                string_offset_from_target: 0,
            }],
            truncation_sites: Vec::new(),
        });
        assert!(
            unselected_barrier_expiry_message_offset(&decoded, &messages)
                .unwrap_err()
                .to_string()
                .contains("acquired a renderer consumer")
        );
    }

    #[test]
    fn companion_candidates_require_current_descriptor_records_and_messages() {
        let mut decoded = vec![0; 0x9600];
        decoded[0x3d9d..0x3dae].copy_from_slice(&[
            0x83, 0xe1, 0x3f, 0x8b, 0xd9, 0xd1, 0xe3, 0xd1, 0xe3, 0x03, 0xd9, 0xd1, 0xe3, 0x81,
            0xc3, 0x54, 0x5a,
        ]);
        decoded[0x2d04..0x2d0c].copy_from_slice(&[0x5b, 0x5a, 0x68, 0xc0, 0x2e, 0xff, 0x67, 0x02]);
        decoded[0x3557..0x3567].copy_from_slice(&[
            0x8c, 0xc8, 0x8e, 0xd8, 0x8b, 0x77, 0x04, 0x8a, 0x6f, 0x07, 0xb1, 0x00, 0xb4, 0x01,
            0xcd, 0x7b,
        ]);
        decoded[0x3594..0x35a4].copy_from_slice(&[
            0xff, 0x54, 0x04, 0xff, 0x74, 0x06, 0x26, 0x8b, 0x77, 0x38, 0xff, 0x14, 0x58, 0xff,
            0x74, 0x06,
        ]);
        decoded[0x35cd..0x35db].copy_from_slice(&[
            0x26, 0x8b, 0x77, 0x38, 0xff, 0x14, 0x8b, 0x74, 0x06, 0xbf, 0xff, 0xff, 0xb0, 0x01,
        ]);
        decoded[0x3612..0x3622].copy_from_slice(&[
            0x26, 0x8b, 0x77, 0x38, 0x0b, 0xf6, 0x74, 0x1c, 0x26, 0xff, 0x77, 0x3a, 0x2e, 0xff,
            0x54, 0x02,
        ]);
        for record_index in CURRENT_COMPANION_RECORD_RANGE.clone() {
            let record_site = ITEM_RECORD_TABLE_OFFSET + record_index * ITEM_RECORD_STRIDE;
            decoded[record_site..record_site + ITEM_RECORD_STRIDE]
                .copy_from_slice(&[0x52, 0x63, 0x57, 0x36, 0x39, 0x87, 0xa0, 0xff, 0x00, 0x00]);
        }
        decoded[0x8639..0x8641].copy_from_slice(&[0x0b, 0x88, 0x0c, 0x88, 0x0b, 0x88, 0x52, 0x63]);
        decoded[0x870b] = 0xc3;
        decoded[0x870c..0x8716]
            .copy_from_slice(&[0xb4, 0x01, 0xcd, 0x7b, 0x26, 0xc6, 0x87, 0xa5, 0x01, 0x00]);
        for candidate_offset in UNSELECTED_COMPANION_ACTION_CANDIDATE_OFFSETS {
            decoded[candidate_offset..candidate_offset + 3].copy_from_slice(&[0x82, 0xa0, 0]);
        }
        let messages = CURRENT_COMPANION_ACTION_MESSAGES
            .into_iter()
            .map(|(message_offset, rewrite_site)| Message {
                string_decoded_offset: message_offset,
                string_logical_offset: message_offset + LOAD_OFFSET,
                continuation_decoded_offset: None,
                addressed_prefix_raw: Vec::new(),
                raw: vec![0x82, 0xa2],
                text: "い".to_owned(),
                rewrite_sites: vec![MainTextRewriteSite {
                    decoded_offset: rewrite_site,
                    kind: MainTextPointerKind::MovSi,
                    string_offset_from_target: 0,
                }],
                truncation_sites: Vec::new(),
            })
            .collect::<Vec<_>>();

        let offsets = unselected_companion_action_candidate_offsets(&decoded, &messages).unwrap();
        assert_eq!(
            offsets,
            BTreeSet::from(UNSELECTED_COMPANION_ACTION_CANDIDATE_OFFSETS)
        );

        let first_record =
            ITEM_RECORD_TABLE_OFFSET + CURRENT_COMPANION_RECORD_RANGE.start * ITEM_RECORD_STRIDE;
        decoded[first_record + 4] ^= 1;
        assert!(
            unselected_companion_action_candidate_offsets(&decoded, &messages)
                .unwrap_err()
                .to_string()
                .contains("current companion item record")
        );
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn typed_main_entry_hook_preserves_the_original_prologue_and_records() {
        let entry = assemble_main_entry_prologue().unwrap();
        let mut decoded = entry.bytes().to_vec();
        decoded.resize(0x400, 0);
        let glyph = MainTextGaijiGlyph {
            character: '가',
            jis_code: 0x7621,
            shift_jis_code: [0xeb, 0x9f],
            bitmap: rasterize_character('가').unwrap(),
        };

        let installer = install_main_entry_gaiji_hook(&mut decoded, &[glyph]).unwrap();

        assert_eq!(installer, 0x500);
        assert_ne!(&decoded[..entry.bytes().len()], entry.bytes());
        assert!(decoded.ends_with(&rasterize_character('가').unwrap()));
    }
}
