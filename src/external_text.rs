use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};
use v30::{
    AssembledProgram, Assembler, CodeLocation, Instruction, JmpTarget, Operand, Register8,
    Register16, SegmentRegister, decode_bytes,
};

use crate::catalog_bundle::{read_catalog_json, resolve_catalog_part_path};
use crate::compile_lz::{decode_exact_compile_lz, encode_compile_lz};
use crate::expected_write::{FixedRangeExpectedWrite, apply_fixed_range_expected_writes};
use crate::font::{FontReport, GLYPH_BYTES, font_report, rasterize_character};
use crate::game_data::{MAIN_COM_PACKED_STREAM_OFFSET, decode_main_com_image};
use crate::lha_sfx::extract_lha_sfx;
use crate::official_patch::apply_official_freeze_fix;
use crate::renderer_font::BiosGaijiRecord;
use crate::renderer_font::{encode_renderer_text, is_modern_hangul, jis_to_shift_jis};
use crate::source_disk::{load_verified_source, sha256_hex};

const CATALOG_SCHEMA: &str = "pc98_madou_docho.external_text_catalog";
const CATALOG_BUNDLE_SCHEMA: &str = "pc98_madou_docho.external_text_catalog_bundle";
const PROGRAM_FILE_SCHEMA: &str = "pc98_madou_docho.external_text_program";
const UNTRANSLATED_STATUS: &str = "untranslated";
const COM_ORIGIN: usize = 0x100;
const GAIJI_FIRST_ROW: u8 = 0x76;
const GAIJI_LAST_ROW: u8 = 0x77;
const JIS_FIRST_CELL: u8 = 0x21;
const JIS_LAST_CELL: u8 = 0x7e;
const GAIJI_RECORD_PREFIX: [u8; 2] = [0x02, 0x02];
const GAIJI_CAPACITY: usize =
    (GAIJI_LAST_ROW - GAIJI_FIRST_ROW + 1) as usize * (JIS_LAST_CELL - JIS_FIRST_CELL + 1) as usize;
const DSH_RESIDENT_END_WORD_OFFSET: usize = 0x0237;
const DSH_ORIGINAL_RESIDENT_END: u16 = 0x04d5;

const DSH_SHA256: &str = "6266c2a213b1fb288e49cec0ef7c02ee701ace20d448284d4de730aa543ec806";
const MEGDOS_SHA256: &str = "ef414945c33d6a0fe199a7e75ea88c963dfc5957031fd7ac25495c45bc088ac2";
const BPLAY_SHA256: &str = "60ca8542f68e2cad28f285923634f733a855c72fa2322e6a50a15475a9d66031";
const FPLAY_SHA256: &str = "a1f4e4f52df4ca8d0f927ee22d2c9140e5892233806ec2e127ba811993ebd441";
const UPDATED_BSAMP_SHA256: &str =
    "c8f1f5bea4f41d968f3790b1ceefcdf204a8e011015489346b402f4450c9cb8e";
const MENU_SHA256: &str = "ecf2ce84f0f005ba400e68137f9d21134c1825f48bf6d930720ddd2c9f97595d";
const MAIN_SHA256: &str = "50954a2dce3f4d424558718b753029ae27b0a33273563cb2b3792318da1e6957";
const MAIN_UNPACKED_SHA256: &str =
    "0302fc6f45a02528de222f9483564c807be8038a4b48224b2e3ce4ac7669ee30";
const MAIN_UNPACKED_SIZE: usize = 10_992;

const MENU_TEXT_RENDERER_OFFSET: usize = 0x0c60;
const MENU_DISK_ERROR_TABLE_OFFSET: usize = 0x0571;
const MENU_DISK_ERROR_TABLE_ADDRESS: usize = 0x0671;
const MENU_DISK_ERROR_COUNT: usize = 13;
const MENU_DISK_ERROR_CONSUMER_OFFSET: usize = 0x050d;
const MENU_PROCESS_ERROR_CONSUMER_OFFSET: usize = 0x07aa;
const MENU_FLOPPY_ERROR_TABLE_OFFSET: usize = 0x0e7e;
const MENU_FLOPPY_ERROR_TABLE_ADDRESS: usize = 0x0f7e;
const MENU_FLOPPY_ERROR_COUNT: usize = 18;
const MENU_FLOPPY_ERROR_CONSUMER_OFFSET: usize = 0x0e51;

const MENU_DIRECT_DOS_OUTPUTS: [(usize, usize); 9] = [
    (0x02f3, 0x01ea),
    (0x0396, 0x0283),
    (0x0411, 0x0307),
    (0x0453, 0x0349),
    (0x04ae, 0x03a2),
    (0x07de, 0x06d2),
    (0x0934, 0x07d2),
    (0x08fc, 0x07d9),
    (0x0a03, 0x08f7),
];
const MENU_PROCESS_ERROR_OUTPUTS: [(usize, usize); 6] = [
    (0x0943, 0x07aa),
    (0x095c, 0x07b2),
    (0x0971, 0x07ba),
    (0x0984, 0x07c2),
    (0x0993, 0x07ca),
    (0x0934, 0x07d2),
];
const MENU_FIXED_RENDERER_OUTPUTS: [(usize, usize, ExternalTextTerminator, &str); 4] = [
    (
        0x0282,
        0x0158,
        ExternalTextTerminator::AtSign,
        "wrong_disk_prompt",
    ),
    (
        0x079d,
        0x0519,
        ExternalTextTerminator::DosDollar,
        "disk_retry_prompt",
    ),
    (
        0x0f05,
        0x0dfd,
        ExternalTextTerminator::DosDollar,
        "hard_disk_guard",
    ),
    (
        0x0f5f,
        0x0e44,
        ExternalTextTerminator::DosDollar,
        "floppy_drive_label",
    ),
];

const BPLAY_DIRECT_SCAN: std::ops::Range<usize> = 0x2700..0x2d79;
const FPLAY_DIRECT_SCAN: std::ops::Range<usize> = 0x2700..0x2cae;
const BPLAY_TIME_CONSUMER: usize = 0x2d2c;
const FPLAY_TIME_CONSUMER: usize = 0x2c61;
const BPLAY_TIME_TABLE_OFFSET: usize = 0x2d79;
const FPLAY_TIME_TABLE_OFFSET: usize = 0x2cae;
const TIME_TABLE_RECORDS: usize = 10;
const TIME_TABLE_RECORD_SIZE: usize = 4;

const BPLAY_ALTERNATE_PATTERN: &[u8] = &[
    0xb4, 0x09, 0xba, 0xf9, 0x2a, 0x80, 0x3e, 0xbb, 0x01, 0x00, 0x74, 0x03, 0xba, 0x54, 0x2b, 0xcd,
    0x21,
];
const BPLAY_TIME_TABLE_PATTERN: &[u8] = &[
    0x00, 0x01, 0xa1, 0x2e, 0x00, 0x02, 0xcb, 0x2e, 0x01, 0x02, 0xb3, 0x2f, 0x00, 0x03, 0xf9, 0x2e,
    0x00, 0x04, 0x1d, 0x2f, 0x00, 0x06, 0x4d, 0x2f, 0x00, 0x08, 0x81, 0x2f, 0x00, 0x16, 0xb3, 0x2f,
    0x00, 0x17, 0xba, 0x2f, 0x00, 0x18, 0xd8, 0x2f,
];
const FPLAY_TIME_TABLE_PATTERN: &[u8] = &[
    0x00, 0x01, 0xd6, 0x2d, 0x00, 0x02, 0xec, 0x2d, 0x01, 0x02, 0xb6, 0x2e, 0x00, 0x03, 0x08, 0x2e,
    0x00, 0x04, 0x36, 0x2e, 0x00, 0x06, 0x58, 0x2e, 0x00, 0x08, 0x78, 0x2e, 0x00, 0x16, 0xb6, 0x2e,
    0x00, 0x17, 0xbd, 0x2e, 0x00, 0x18, 0xe7, 0x2e,
];

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTextCatalog {
    pub schema: String,
    pub program_count: usize,
    pub entry_count: usize,
    pub reference_count: usize,
    pub unresolved_program_count: usize,
    pub programs: Vec<ExternalTextProgram>,
    pub unresolved_programs: Vec<UnresolvedExternalTextProgram>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalTextCatalogBundle {
    schema: String,
    catalog: ExternalTextCatalog,
    program_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalTextProgramFile {
    schema: String,
    program: ExternalTextProgram,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTextProgram {
    pub name: String,
    pub source_sha256: String,
    pub source_size: usize,
    pub storage: ExternalTextStorage,
    pub view: ExternalTextView,
    pub view_sha256: String,
    pub view_size: usize,
    pub entry_count: usize,
    pub reference_count: usize,
    pub target_like_nonreferences: Vec<ExternalTextNonReference>,
    pub entries: Vec<ExternalTextEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTextNonReference {
    pub entry_id: String,
    pub offset: usize,
    pub observed_value: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTextStorage {
    SourceDiskFile,
    OfficialFreezeFixResult,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalTextView {
    SourceFile,
    SelfUnpackedCom { packed_stream_offset: usize },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTextEntry {
    pub id: String,
    pub text_offset: usize,
    pub runtime_address: usize,
    pub terminator: ExternalTextTerminator,
    pub byte_budget: usize,
    pub raw_hex: String,
    pub source_text: String,
    pub references: Vec<ExternalTextReference>,
    pub ko: String,
    pub status: String,
    pub notes: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTextTerminator {
    DosDollar,
    Null,
    AtSign,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTextRegister {
    Si,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalTextReference {
    ComAddressImmediate {
        instruction_offset: usize,
        target_word_offset: usize,
        role: String,
    },
    ComAddressRegisterImmediate {
        instruction_offset: usize,
        target_word_offset: usize,
        register: ExternalTextRegister,
        role: String,
    },
    FileOffsetImmediate {
        instruction_offset: usize,
        target_word_offset: usize,
        role: String,
    },
    ComAddressTable {
        table_offset: usize,
        table_index: usize,
        target_word_offset: usize,
        consumer_offset: usize,
        role: String,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedExternalTextProgram {
    pub name: String,
    pub source_sha256: String,
    pub source_size: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExternalTextCatalogValidation {
    pub program_count: usize,
    pub entry_count: usize,
    pub reference_count: usize,
    pub unresolved_program_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExternalTextProgramRebuildReport {
    pub name: String,
    pub changed_entries: usize,
    pub relocated_entries: usize,
    pub updated_references: usize,
    pub original_view_sha256: String,
    pub updated_view_sha256: String,
    pub original_view_size: usize,
    pub updated_view_size: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExternalTextGaijiGlyphReport {
    pub character: char,
    pub jis_code: String,
    pub shift_jis_code: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExternalTextProgramBuildReport {
    pub name: String,
    pub text: ExternalTextProgramRebuildReport,
    pub gaiji_glyph_count: usize,
    pub gaiji_installer_logical_offset: usize,
    pub gaiji_glyphs: Vec<ExternalTextGaijiGlyphReport>,
    pub updated_sha256: String,
    pub updated_size: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ExternalTextBuildReport {
    pub changed_programs: usize,
    pub changed_entries: usize,
    pub relocated_entries: usize,
    pub deferred_programs: Vec<String>,
    pub programs: Vec<ExternalTextProgramBuildReport>,
    pub font: FontReport,
}

pub(crate) struct ExternalMainComDraft {
    text: ExternalTextProgramRebuildReport,
    gaiji_glyphs: Vec<ExternalTextGaijiGlyphReport>,
    bios_gaiji: Vec<BiosGaijiRecord>,
    loader_preimage_sha256: String,
}

impl ExternalMainComDraft {
    pub(crate) fn bios_gaiji(&self) -> &[BiosGaijiRecord] {
        &self.bios_gaiji
    }

    pub(crate) fn loader_preimage_sha256(&self) -> &str {
        &self.loader_preimage_sha256
    }

    pub(crate) fn finish(
        self,
        final_main_com: &[u8],
        gaiji_installer_logical_offset: usize,
    ) -> ExternalTextProgramBuildReport {
        ExternalTextProgramBuildReport {
            name: "MAIN.COM".to_owned(),
            text: self.text,
            gaiji_glyph_count: self.gaiji_glyphs.len(),
            gaiji_installer_logical_offset,
            gaiji_glyphs: self.gaiji_glyphs,
            updated_sha256: sha256_hex(final_main_com),
            updated_size: final_main_com.len(),
        }
    }
}

impl ExternalTextBuildReport {
    pub(crate) fn include_main_com(
        &mut self,
        program: ExternalTextProgramBuildReport,
    ) -> Result<()> {
        ensure!(
            program.name == "MAIN.COM",
            "external text completion is not MAIN.COM"
        );
        ensure!(
            self.deferred_programs == ["MAIN.COM"],
            "external text report does not defer exactly MAIN.COM"
        );
        ensure!(
            self.programs.iter().all(|item| item.name != program.name),
            "external text report already contains MAIN.COM"
        );
        self.changed_programs += 1;
        self.changed_entries += program.text.changed_entries;
        self.relocated_entries += program.text.relocated_entries;
        self.deferred_programs.clear();
        self.programs.push(program);
        self.programs
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ExternalTextGaijiGlyph {
    character: char,
    jis_code: u16,
    shift_jis_code: [u8; 2],
    bitmap: [u8; GLYPH_BYTES],
}

struct ExternalTextGaijiPlan {
    codebook: BTreeMap<char, [u8; 2]>,
    glyphs: Vec<ExternalTextGaijiGlyph>,
}

#[derive(Clone, Copy)]
enum ExternalEntryBehavior {
    NearJump { target: u16 },
    CliThenNearJump { target: u16 },
    MenuPrologue,
}

#[derive(Clone, Copy)]
struct ExternalEntryProfile {
    logical_origin: u16,
    behavior: ExternalEntryBehavior,
}

struct PendingEntry {
    runtime_address: usize,
    terminator: ExternalTextTerminator,
    raw: Vec<u8>,
    references: Vec<ExternalTextReference>,
}

struct ProgramBuilder<'a> {
    name: &'static str,
    source_sha256: &'static str,
    source_size: usize,
    storage: ExternalTextStorage,
    view: ExternalTextView,
    bytes: &'a [u8],
    entries: BTreeMap<usize, PendingEntry>,
}

pub fn extract_external_text_catalog(source_path: &Path) -> Result<ExternalTextCatalog> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    let dsh = required(&source.tool_files, "DSH.COM")?;
    let megdos = required(&source.system_files, "MEGDOS.SYS")?;
    let bplay = required(&source.tool_files, "BPLAY.COM")?;
    let fplay = required(&source.tool_files, "FPLAY.COM")?;
    let original_bsamp = required(&payload, "BSAMP.COM")?;
    let updated_bsamp = apply_official_freeze_fix(original_bsamp)?.updated_bsamp;
    let menu = required(&payload, "MENU.COM")?;
    let main = required(&payload, "MAIN.COM")?;

    verify_file("DSH.COM", dsh, DSH_SHA256)?;
    verify_file("MEGDOS.SYS", megdos, MEGDOS_SHA256)?;
    verify_file("BPLAY.COM", bplay, BPLAY_SHA256)?;
    verify_file("FPLAY.COM", fplay, FPLAY_SHA256)?;
    verify_file("BSAMP.COM", &updated_bsamp, UPDATED_BSAMP_SHA256)?;
    verify_file("MENU.COM", menu, MENU_SHA256)?;
    verify_file("MAIN.COM", main, MAIN_SHA256)?;

    let mut programs = vec![
        catalog_shell(dsh)?,
        catalog_system_loader(megdos)?,
        catalog_playback_driver("BPLAY.COM", bplay)?,
        catalog_playback_driver("FPLAY.COM", fplay)?,
        catalog_sampling_driver(&updated_bsamp)?,
        catalog_main_program(main)?,
        catalog_menu_program(menu)?,
    ];
    programs.sort_by(|left, right| left.name.cmp(&right.name));
    let entry_count = programs.iter().map(|program| program.entry_count).sum();
    let reference_count = programs.iter().map(|program| program.reference_count).sum();
    ensure!(
        programs
            .iter()
            .map(|program| {
                (
                    program.name.as_str(),
                    program.entry_count,
                    program.reference_count,
                )
            })
            .collect::<Vec<_>>()
            == [
                ("BPLAY.COM", 20, 23),
                ("BSAMP.COM", 3, 3),
                ("DSH.COM", 8, 8),
                ("FPLAY.COM", 21, 26),
                ("MAIN.COM", 3, 3),
                ("MEGDOS.SYS", 7, 8),
                ("MENU.COM", 47, 50),
            ],
        "external program text population changed"
    );
    ensure!(
        entry_count == 109 && reference_count == 121,
        "external program text totals changed"
    );
    let unresolved_programs = Vec::new();
    Ok(ExternalTextCatalog {
        schema: CATALOG_SCHEMA.to_owned(),
        program_count: programs.len(),
        entry_count,
        reference_count,
        unresolved_program_count: unresolved_programs.len(),
        programs,
        unresolved_programs,
    })
}

pub fn write_external_text_catalog(
    source_path: &Path,
    output_path: &Path,
) -> Result<ExternalTextCatalog> {
    let catalog = extract_external_text_catalog(source_path)?;
    let encoded = serde_json::to_vec_pretty(&catalog).context("serialize external text catalog")?;
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create external text catalog directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(output_path, encoded).with_context(|| {
        format!(
            "failed to write external text catalog: {}",
            output_path.display()
        )
    })?;
    Ok(catalog)
}

pub fn validate_external_text_catalog(
    source_path: &Path,
    catalog_path: &Path,
) -> Result<ExternalTextCatalogValidation> {
    let expected = extract_external_text_catalog(source_path)?;
    let supplied = read_external_text_catalog(catalog_path)?;
    validate_translation_states(&supplied)?;
    let mut protected = supplied;
    for program in &mut protected.programs {
        for entry in &mut program.entries {
            entry.ko.clear();
            entry.status = UNTRANSLATED_STATUS.to_owned();
            entry.notes.clear();
        }
    }
    ensure!(
        protected == expected,
        "external text catalog protected fields differ from the exact source extraction"
    );
    Ok(ExternalTextCatalogValidation {
        program_count: expected.program_count,
        entry_count: expected.entry_count,
        reference_count: expected.reference_count,
        unresolved_program_count: expected.unresolved_program_count,
    })
}

pub(crate) fn apply_external_source_file_drafts(
    system_files: &mut BTreeMap<String, Vec<u8>>,
    tool_files: &mut BTreeMap<String, Vec<u8>>,
    payload: &mut BTreeMap<String, Vec<u8>>,
    catalog_path: &Path,
) -> Result<ExternalTextBuildReport> {
    let catalog = read_external_text_catalog(catalog_path)?;
    validate_translation_states(&catalog)?;

    let mut programs = Vec::new();
    let mut deferred_programs = Vec::new();
    for program in &catalog.programs {
        if program.view != ExternalTextView::SourceFile {
            deferred_programs.push(program.name.clone());
            continue;
        }
        let original = match program.name.as_str() {
            "MEGDOS.SYS" => {
                ensure!(
                    program.storage == ExternalTextStorage::SourceDiskFile,
                    "MEGDOS.SYS external text has an unexpected storage owner"
                );
                required(system_files, &program.name)?.to_vec()
            }
            "BPLAY.COM" | "DSH.COM" | "FPLAY.COM" => {
                ensure!(
                    program.storage == ExternalTextStorage::SourceDiskFile,
                    "{} external text has an unexpected storage owner",
                    program.name
                );
                required(tool_files, &program.name)?.to_vec()
            }
            "BSAMP.COM" => {
                ensure!(
                    program.storage == ExternalTextStorage::OfficialFreezeFixResult,
                    "BSAMP.COM external text is not based on the official freeze-fix result"
                );
                required(payload, &program.name)?.to_vec()
            }
            "MENU.COM" => {
                ensure!(
                    program.storage == ExternalTextStorage::SourceDiskFile,
                    "MENU.COM external text has an unexpected storage owner"
                );
                required(payload, &program.name)?.to_vec()
            }
            name => bail!("{name} has no source-file external text owner"),
        };

        let gaiji_plan = build_external_text_gaiji_plan(program)?;
        let (text_view, text) =
            rebuild_external_text_source_file(program, &original, &gaiji_plan.codebook)?;
        ensure!(
            text.changed_entries > 0,
            "{} external development build has no Korean draft changes",
            program.name
        );
        if program.name == "MEGDOS.SYS" {
            ensure!(
                text.relocated_entries == 0,
                "MEGDOS.SYS translated text must remain inside its resident source slots"
            );
        }
        let (updated, gaiji_installer_logical_offset) =
            install_external_text_gaiji(&program.name, &text_view, &gaiji_plan)?;

        match program.name.as_str() {
            "MEGDOS.SYS" => {
                system_files.insert(program.name.clone(), updated.clone());
            }
            "BPLAY.COM" | "DSH.COM" | "FPLAY.COM" => {
                tool_files.insert(program.name.clone(), updated.clone());
            }
            "BSAMP.COM" | "MENU.COM" => {
                payload.insert(program.name.clone(), updated.clone());
            }
            _ => unreachable!("source-file owner was checked above"),
        }
        let gaiji_glyphs = gaiji_plan
            .glyphs
            .iter()
            .map(|glyph| ExternalTextGaijiGlyphReport {
                character: glyph.character,
                jis_code: format!("{:04X}", glyph.jis_code),
                shift_jis_code: format!(
                    "{:02X}{:02X}",
                    glyph.shift_jis_code[0], glyph.shift_jis_code[1]
                ),
            })
            .collect::<Vec<_>>();
        programs.push(ExternalTextProgramBuildReport {
            name: program.name.clone(),
            text,
            gaiji_glyph_count: gaiji_glyphs.len(),
            gaiji_installer_logical_offset,
            gaiji_glyphs,
            updated_sha256: sha256_hex(&updated),
            updated_size: updated.len(),
        });
    }

    programs.sort_by(|left, right| left.name.cmp(&right.name));
    deferred_programs.sort();
    ensure!(
        programs
            .iter()
            .map(|program| program.name.as_str())
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "BPLAY.COM",
                "BSAMP.COM",
                "DSH.COM",
                "FPLAY.COM",
                "MEGDOS.SYS",
                "MENU.COM",
            ]),
        "external source-file development build has an unexpected program set"
    );
    ensure!(
        deferred_programs == ["MAIN.COM"],
        "external source-file development build has an unexpected deferred program set"
    );
    Ok(ExternalTextBuildReport {
        changed_programs: programs.len(),
        changed_entries: programs
            .iter()
            .map(|program| program.text.changed_entries)
            .sum(),
        relocated_entries: programs
            .iter()
            .map(|program| program.text.relocated_entries)
            .sum(),
        deferred_programs,
        programs,
        font: font_report()?,
    })
}

pub(crate) fn apply_external_main_com_draft(
    payload: &mut BTreeMap<String, Vec<u8>>,
    catalog_path: &Path,
) -> Result<ExternalMainComDraft> {
    let catalog = read_external_text_catalog(catalog_path)?;
    validate_translation_states(&catalog)?;
    let program = catalog
        .programs
        .iter()
        .find(|program| program.name == "MAIN.COM")
        .context("external text catalog is missing MAIN.COM")?;
    ensure!(
        program.storage == ExternalTextStorage::SourceDiskFile,
        "MAIN.COM external text has an unexpected storage owner"
    );
    ensure!(
        program.view
            == (ExternalTextView::SelfUnpackedCom {
                packed_stream_offset: MAIN_COM_PACKED_STREAM_OFFSET,
            }),
        "MAIN.COM external text has an unexpected analysis view"
    );

    let original = required(payload, "MAIN.COM")?.to_vec();
    ensure!(
        original.len() == program.source_size && sha256_hex(&original) == program.source_sha256,
        "MAIN.COM bytes differ from the protected external text source"
    );
    let original_view =
        decode_main_com_image(&original).context("decode external MAIN.COM body")?;
    let gaiji_plan = build_external_text_gaiji_plan(program)?;
    let (updated_view, text) =
        rebuild_external_text_view(program, &original_view, program.view, &gaiji_plan.codebook)?;
    ensure!(
        text.changed_entries > 0,
        "MAIN.COM external development build has no Korean draft changes"
    );

    let repacked_body = encode_compile_lz(&updated_view);
    let roundtrip = decode_exact_compile_lz(&repacked_body)
        .context("rebuilt external MAIN.COM body is not one exact Compile-LZ stream")?;
    ensure!(
        roundtrip.streams == [updated_view.clone()],
        "rebuilt external MAIN.COM body does not decode to the translated view"
    );
    let mut updated = original[..MAIN_COM_PACKED_STREAM_OFFSET].to_vec();
    updated.extend_from_slice(&repacked_body);
    ensure!(
        decode_main_com_image(&updated)? == updated_view,
        "external MAIN.COM repacked readback differs from the translated view"
    );
    let loader_preimage_sha256 = sha256_hex(&updated);
    payload.insert("MAIN.COM".to_owned(), updated);

    let gaiji_glyphs = gaiji_plan
        .glyphs
        .iter()
        .map(|glyph| ExternalTextGaijiGlyphReport {
            character: glyph.character,
            jis_code: format!("{:04X}", glyph.jis_code),
            shift_jis_code: format!(
                "{:02X}{:02X}",
                glyph.shift_jis_code[0], glyph.shift_jis_code[1]
            ),
        })
        .collect();
    let bios_gaiji = gaiji_plan
        .glyphs
        .into_iter()
        .map(|glyph| BiosGaijiRecord {
            jis_code: glyph.jis_code,
            bitmap: glyph.bitmap,
        })
        .collect();
    Ok(ExternalMainComDraft {
        text,
        gaiji_glyphs,
        bios_gaiji,
        loader_preimage_sha256,
    })
}

pub(crate) fn read_external_text_catalog(path: &Path) -> Result<ExternalTextCatalog> {
    if !path.is_dir() {
        return read_catalog_json(path, "external text catalog");
    }

    let bundle_path = path.join("catalog.json");
    let bundle: ExternalTextCatalogBundle =
        read_catalog_json(&bundle_path, "external text catalog bundle")?;
    ensure!(
        bundle.schema == CATALOG_BUNDLE_SCHEMA,
        "unsupported external text catalog bundle schema {:?}",
        bundle.schema
    );
    ensure!(
        bundle.catalog.programs.is_empty(),
        "external text catalog bundle metadata must not contain inline programs"
    );
    ensure!(
        !bundle.program_files.is_empty(),
        "external text catalog bundle has no program files"
    );

    let mut catalog = bundle.catalog;
    let mut listed_files = HashSet::new();
    let mut program_names = HashSet::new();
    for relative_path in bundle.program_files {
        ensure!(
            listed_files.insert(relative_path.clone()),
            "external text catalog bundle lists {relative_path:?} more than once"
        );
        let program_path =
            resolve_catalog_part_path(path, &relative_path, "external text program file")?;
        let program_file: ExternalTextProgramFile =
            read_catalog_json(&program_path, "external text program file")?;
        ensure!(
            program_file.schema == PROGRAM_FILE_SCHEMA,
            "unsupported external text program schema {:?} in {}",
            program_file.schema,
            program_path.display()
        );
        ensure!(
            program_names.insert(program_file.program.name.clone()),
            "external text program {} appears more than once",
            program_file.program.name
        );
        ensure!(
            program_file.program.entries.len() == program_file.program.entry_count,
            "{} declares {} entries but contains {}",
            program_file.program.name,
            program_file.program.entry_count,
            program_file.program.entries.len()
        );
        catalog.programs.push(program_file.program);
    }

    ensure!(
        catalog.programs.len() == catalog.program_count,
        "external text catalog bundle declares {} programs but contains {}",
        catalog.program_count,
        catalog.programs.len()
    );
    let entry_count = catalog
        .programs
        .iter()
        .map(|program| program.entries.len())
        .sum::<usize>();
    ensure!(
        entry_count == catalog.entry_count,
        "external text catalog bundle declares {} entries but contains {entry_count}",
        catalog.entry_count
    );
    let reference_count = catalog
        .programs
        .iter()
        .map(|program| program.reference_count)
        .sum::<usize>();
    ensure!(
        reference_count == catalog.reference_count,
        "external text catalog bundle declares {} references but contains {reference_count}",
        catalog.reference_count
    );
    ensure!(
        catalog.unresolved_programs.len() == catalog.unresolved_program_count,
        "external text catalog bundle declares {} unresolved programs but contains {}",
        catalog.unresolved_program_count,
        catalog.unresolved_programs.len()
    );
    Ok(catalog)
}

pub fn rebuild_external_text_source_file(
    program: &ExternalTextProgram,
    original_view: &[u8],
    codebook: &BTreeMap<char, [u8; 2]>,
) -> Result<(Vec<u8>, ExternalTextProgramRebuildReport)> {
    rebuild_external_text_view(
        program,
        original_view,
        ExternalTextView::SourceFile,
        codebook,
    )
}

fn rebuild_external_text_view(
    program: &ExternalTextProgram,
    original_view: &[u8],
    expected_view: ExternalTextView,
    codebook: &BTreeMap<char, [u8; 2]>,
) -> Result<(Vec<u8>, ExternalTextProgramRebuildReport)> {
    ensure!(
        program.view == expected_view,
        "{} external text has an unexpected analysis view",
        program.name
    );
    ensure!(
        original_view.len() == program.view_size
            && sha256_hex(original_view) == program.view_sha256,
        "{} bytes differ from the protected external text view",
        program.name
    );
    ensure!(
        program.entry_count == program.entries.len()
            && program.reference_count
                == program
                    .entries
                    .iter()
                    .map(|entry| entry.references.len())
                    .sum::<usize>(),
        "{} external text population metadata is inconsistent",
        program.name
    );

    let mut source_slots = Vec::with_capacity(program.entries.len());
    for entry in &program.entries {
        validate_translation_state(entry)?;
        let body = read_terminated(original_view, entry.text_offset, entry.terminator)
            .with_context(|| format!("{} source boundary changed", entry.id))?;
        ensure!(
            body.len() + 1 == entry.byte_budget && encode_hex(body) == entry.raw_hex,
            "{} source bytes differ from the protected external text entry",
            entry.id
        );
        let slot_end = entry
            .text_offset
            .checked_add(entry.byte_budget)
            .with_context(|| format!("{} source slot overflows its view", entry.id))?;
        ensure!(
            slot_end <= original_view.len(),
            "{} source slot lies outside its view",
            entry.id
        );
        source_slots.push((entry.text_offset, slot_end));
    }
    let mut ordered_source_slots = source_slots.clone();
    ordered_source_slots.sort_unstable();
    for pair in ordered_source_slots.windows(2) {
        ensure!(
            pair[0].1 <= pair[1].0,
            "{} external text source slots overlap at {:#x}",
            program.name,
            pair[1].0
        );
    }

    let mut reference_site_owners = BTreeMap::new();
    for entry in &program.entries {
        for reference in &entry.references {
            let site = reference_target_word_offset(reference);
            let site_end = site
                .checked_add(2)
                .with_context(|| format!("{} reference site overflows its view", entry.id))?;
            if let Some(owner) = reference_site_owners.insert(site, entry.id.as_str()) {
                ensure!(
                    owner == entry.id,
                    "{} reference site {site:#x} is claimed by both {owner} and {}",
                    program.name,
                    entry.id
                );
            }
            ensure!(
                !source_slots
                    .iter()
                    .any(|(start, end)| site < *end && site_end > *start),
                "{} reference site {site:#x} overlaps an external text slot",
                entry.id
            );
            let expected_target = protected_reference_target(entry, reference)?;
            let actual_target = usize::from(read_u16(original_view, site)?);
            ensure!(
                actual_target == expected_target,
                "{} reference site {site:#x} holds {actual_target:#x}, expected {expected_target:#x}",
                entry.id
            );
        }
    }

    let mut appended = Vec::new();
    let mut writes = Vec::new();
    let mut placements = Vec::new();
    let mut updated_references = 0usize;
    let mut planned_reference_sites = BTreeSet::new();
    for entry in &program.entries {
        if entry.ko.is_empty() {
            continue;
        }
        let source_body = read_terminated(original_view, entry.text_offset, entry.terminator)?;
        let encoded = encode_renderer_text(&entry.ko, codebook)
            .with_context(|| format!("{} Korean text cannot be encoded", entry.id))?;
        let terminator = terminator_byte(entry.terminator);
        ensure!(
            !encoded.contains(&terminator),
            "{} Korean text contains its consumer terminator 0x{terminator:02X}",
            entry.id
        );
        ensure_external_controls_preserved(source_body, &encoded)
            .with_context(|| format!("{} Korean text changed its control stream", entry.id))?;
        if encoded == source_body {
            continue;
        }
        if encoded.len() < entry.byte_budget {
            let source_end = entry.text_offset + entry.byte_budget;
            let expected_source = original_view[entry.text_offset..source_end].to_vec();
            let mut replacement = vec![0; entry.byte_budget];
            replacement[..encoded.len()].copy_from_slice(&encoded);
            replacement[encoded.len()] = terminator;
            writes.push(FixedRangeExpectedWrite {
                writer: "external text replacement",
                purpose: "in-place external text",
                offset: entry.text_offset,
                expected_source,
                replacement,
            });
            placements.push((entry, entry.text_offset, encoded, false));
            continue;
        }
        let destination = original_view
            .len()
            .checked_add(appended.len())
            .with_context(|| format!("{} translated placement overflows", entry.id))?;
        appended.extend_from_slice(&encoded);
        appended.push(terminator);

        for reference in &entry.references {
            let site = reference_target_word_offset(reference);
            if !planned_reference_sites.insert(site) {
                continue;
            }
            let expected = u16::try_from(protected_reference_target(entry, reference)?)
                .with_context(|| format!("{} protected reference exceeds 16 bits", entry.id))?;
            let replacement = u16::try_from(relocated_reference_target(destination, reference)?)
                .with_context(|| format!("{} relocated reference exceeds 16 bits", entry.id))?;
            writes.push(FixedRangeExpectedWrite {
                writer: "external text relocation",
                purpose: "external text reference",
                offset: site,
                expected_source: expected.to_le_bytes().to_vec(),
                replacement: replacement.to_le_bytes().to_vec(),
            });
            updated_references += 1;
        }
        placements.push((entry, destination, encoded, true));
    }

    let mut updated = apply_fixed_range_expected_writes(original_view, &writes)?;
    updated.extend_from_slice(&appended);
    for (entry, destination, encoded, _) in &placements {
        let readback = read_terminated(&updated, *destination, entry.terminator)?;
        ensure!(
            readback == encoded,
            "{} relocated text readback differs from the planned bytes",
            entry.id
        );
        for reference in &entry.references {
            let site = reference_target_word_offset(reference);
            let expected = relocated_reference_target(*destination, reference)?;
            ensure!(
                usize::from(read_u16(&updated, site)?) == expected,
                "{} relocated reference readback differs at {site:#x}",
                entry.id
            );
        }
    }

    let report = ExternalTextProgramRebuildReport {
        name: program.name.clone(),
        changed_entries: placements.len(),
        relocated_entries: placements
            .iter()
            .filter(|(_, _, _, relocated)| *relocated)
            .count(),
        updated_references,
        original_view_sha256: sha256_hex(original_view),
        updated_view_sha256: sha256_hex(&updated),
        original_view_size: original_view.len(),
        updated_view_size: updated.len(),
    };
    Ok((updated, report))
}

fn build_external_text_gaiji_plan(program: &ExternalTextProgram) -> Result<ExternalTextGaijiPlan> {
    let characters = program
        .entries
        .iter()
        .flat_map(|entry| entry.ko.chars())
        .filter(|character| is_modern_hangul(*character))
        .collect::<BTreeSet<_>>();
    ensure!(
        !characters.is_empty(),
        "{} translation has no Hangul gaiji demand",
        program.name
    );
    ensure!(
        characters.len() <= GAIJI_CAPACITY,
        "{} translation needs {} Hangul glyphs but BIOS gaiji capacity is {GAIJI_CAPACITY}",
        program.name,
        characters.len()
    );
    let cells_per_row = usize::from(JIS_LAST_CELL - JIS_FIRST_CELL + 1);
    let glyphs = characters
        .into_iter()
        .enumerate()
        .map(|(index, character)| {
            let row = GAIJI_FIRST_ROW + u8::try_from(index / cells_per_row)?;
            let cell = JIS_FIRST_CELL + u8::try_from(index % cells_per_row)?;
            let jis_code = u16::from_be_bytes([row, cell]);
            Ok(ExternalTextGaijiGlyph {
                character,
                jis_code,
                shift_jis_code: jis_to_shift_jis(jis_code)?,
                bitmap: rasterize_character(character).with_context(|| {
                    format!("rasterize {} external Hangul {character:?}", program.name)
                })?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let codebook = glyphs
        .iter()
        .map(|glyph| (glyph.character, glyph.shift_jis_code))
        .collect();
    Ok(ExternalTextGaijiPlan { codebook, glyphs })
}

fn install_external_text_gaiji(
    name: &str,
    text_view: &[u8],
    plan: &ExternalTextGaijiPlan,
) -> Result<(Vec<u8>, usize)> {
    let profile = external_entry_profile(name)?;
    let expected_entry = assemble_external_entry_precondition(profile)?;
    ensure!(
        text_view.get(..expected_entry.len()) == Some(expected_entry.as_slice()),
        "{name} entry bytes differ from the typed gaiji-hook precondition"
    );
    let installer_logical_offset = usize::from(profile.logical_origin)
        .checked_add(text_view.len())
        .with_context(|| format!("{name} gaiji installer address overflow"))?;
    let installer_logical_offset = u16::try_from(installer_logical_offset)
        .with_context(|| format!("{name} gaiji installer exceeds its segment"))?;
    let placeholder_addresses = vec![0_u16; plan.glyphs.len()];
    let placeholder = assemble_external_gaiji_installer(
        installer_logical_offset,
        &placeholder_addresses,
        &plan.glyphs,
        profile,
    )?;
    let record_size = GAIJI_RECORD_PREFIX.len() + GLYPH_BYTES;
    let record_base = usize::from(installer_logical_offset)
        .checked_add(placeholder.bytes().len())
        .with_context(|| format!("{name} gaiji record address overflow"))?;
    let record_addresses = (0..plan.glyphs.len())
        .map(|index| {
            u16::try_from(record_base + index * record_size)
                .with_context(|| format!("{name} gaiji record exceeds its segment"))
        })
        .collect::<Result<Vec<_>>>()?;
    let installer = assemble_external_gaiji_installer(
        installer_logical_offset,
        &record_addresses,
        &plan.glyphs,
        profile,
    )?;
    ensure!(
        installer.bytes().len() == placeholder.bytes().len(),
        "{name} gaiji installer changed size after record placement"
    );
    let hijack = assemble_external_entry_hijack(
        profile.logical_origin,
        u16::try_from(expected_entry.len()).context("external entry length overflow")?,
        installer_logical_offset,
    )?;
    ensure!(
        hijack.len() == expected_entry.len(),
        "{name} gaiji entry hijack is not length preserving"
    );

    let mut updated = text_view.to_vec();
    updated[..hijack.len()].copy_from_slice(&hijack);
    let installer_offset = updated.len();
    updated.extend_from_slice(installer.bytes());
    for glyph in &plan.glyphs {
        updated.extend_from_slice(&GAIJI_RECORD_PREFIX);
        updated.extend_from_slice(&glyph.bitmap);
    }
    if name == "DSH.COM" {
        let resident_end = u16::try_from(
            updated
                .len()
                .checked_add(COM_ORIGIN)
                .context("DSH.COM resident end address overflow")?,
        )
        .context("DSH.COM resident end exceeds its segment")?;
        updated = apply_fixed_range_expected_writes(
            &updated,
            &[FixedRangeExpectedWrite {
                writer: "DSH.COM resident-size update",
                purpose: "retain relocated external text and gaiji hook",
                offset: DSH_RESIDENT_END_WORD_OFFSET,
                expected_source: DSH_ORIGINAL_RESIDENT_END.to_le_bytes().to_vec(),
                replacement: resident_end.to_le_bytes().to_vec(),
            }],
        )?;
    }
    ensure!(
        updated.get(..hijack.len()) == Some(hijack.as_slice()),
        "{name} gaiji entry hijack readback differs"
    );
    let installer_end = installer_offset + installer.bytes().len();
    ensure!(
        updated.get(installer_offset..installer_end) == Some(installer.bytes()),
        "{name} gaiji installer readback differs"
    );
    let mut cursor = installer_end;
    for glyph in &plan.glyphs {
        let end = cursor + record_size;
        let record = updated
            .get(cursor..end)
            .with_context(|| format!("{name} gaiji record readback lies outside output"))?;
        ensure!(
            record[..GAIJI_RECORD_PREFIX.len()] == GAIJI_RECORD_PREFIX
                && record[GAIJI_RECORD_PREFIX.len()..] == glyph.bitmap,
            "{name} gaiji record readback differs for {:?}",
            glyph.character
        );
        cursor = end;
    }
    ensure!(
        cursor == updated.len(),
        "{name} gaiji tail has unknown bytes"
    );
    Ok((updated, usize::from(installer_logical_offset)))
}

fn external_entry_profile(name: &str) -> Result<ExternalEntryProfile> {
    let profile = match name {
        "BPLAY.COM" => ExternalEntryProfile {
            logical_origin: COM_ORIGIN as u16,
            behavior: ExternalEntryBehavior::NearJump { target: 0x2800 },
        },
        "FPLAY.COM" => ExternalEntryProfile {
            logical_origin: COM_ORIGIN as u16,
            behavior: ExternalEntryBehavior::NearJump { target: 0x2807 },
        },
        "BSAMP.COM" => ExternalEntryProfile {
            logical_origin: COM_ORIGIN as u16,
            behavior: ExternalEntryBehavior::CliThenNearJump { target: 0x15ec },
        },
        "DSH.COM" => ExternalEntryProfile {
            logical_origin: COM_ORIGIN as u16,
            behavior: ExternalEntryBehavior::NearJump { target: 0x030b },
        },
        "MEGDOS.SYS" => ExternalEntryProfile {
            logical_origin: 0,
            behavior: ExternalEntryBehavior::NearJump { target: 0x0100 },
        },
        "MENU.COM" => ExternalEntryProfile {
            logical_origin: COM_ORIGIN as u16,
            behavior: ExternalEntryBehavior::MenuPrologue,
        },
        _ => bail!("{name} has no supported external gaiji entry profile"),
    };
    Ok(profile)
}

fn assemble_external_entry_precondition(profile: ExternalEntryProfile) -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    match profile.behavior {
        ExternalEntryBehavior::NearJump { target } => {
            assembler.emit(Instruction::Jmp {
                target: JmpTarget::Rel16(near_displacement(profile.logical_origin, target)),
            });
        }
        ExternalEntryBehavior::CliThenNearJump { target } => {
            assembler.emit(Instruction::Cli).emit(Instruction::Jmp {
                target: JmpTarget::Rel16(near_displacement(
                    profile.logical_origin.wrapping_add(1),
                    target,
                )),
            });
        }
        ExternalEntryBehavior::MenuPrologue => {
            emit_menu_entry_prologue(&mut assembler);
        }
    }
    assemble_external_at(
        &assembler,
        profile.logical_origin,
        "external entry precondition",
    )
}

fn assemble_external_entry_hijack(origin: u16, entry_len: u16, installer: u16) -> Result<Vec<u8>> {
    ensure!(entry_len >= 3, "external entry is shorter than a near jump");
    let mut assembler = Assembler::new();
    assembler.emit(Instruction::Jmp {
        target: JmpTarget::Rel16(near_displacement(origin, installer)),
    });
    for _ in 3..entry_len {
        assembler.emit(Instruction::Nop);
    }
    assemble_external_at(&assembler, origin, "external gaiji entry hijack")
}

fn assemble_external_gaiji_installer(
    origin: u16,
    record_addresses: &[u16],
    glyphs: &[ExternalTextGaijiGlyph],
    profile: ExternalEntryProfile,
) -> Result<AssembledProgram> {
    ensure!(
        !glyphs.is_empty() && record_addresses.len() == glyphs.len(),
        "external gaiji installer has mismatched records"
    );
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Pushf)
        .emit(Instruction::Pusha)
        .emit(Instruction::Mov {
            dest: Operand::Reg16(Register16::BX),
            src: Operand::Sreg(SegmentRegister::CS),
        });
    for (&record_address, glyph) in record_addresses.iter().zip(glyphs) {
        assembler
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
    match profile.behavior {
        ExternalEntryBehavior::NearJump { target } => {
            emit_absolute_near_return(&mut assembler, target);
        }
        ExternalEntryBehavior::CliThenNearJump { target } => {
            assembler.emit(Instruction::Cli);
            emit_absolute_near_return(&mut assembler, target);
        }
        ExternalEntryBehavior::MenuPrologue => {
            emit_menu_entry_prologue(&mut assembler);
            emit_absolute_near_return(&mut assembler, COM_ORIGIN as u16 + 4);
        }
    }
    assembler
        .assemble(CodeLocation {
            seg: 0,
            off: origin,
        })
        .context("assemble typed external gaiji installer")
}

fn emit_menu_entry_prologue(assembler: &mut Assembler) {
    assembler
        .emit(Instruction::Mov {
            dest: Operand::Reg16(Register16::AX),
            src: Operand::Sreg(SegmentRegister::CS),
        })
        .emit(Instruction::Mov {
            dest: Operand::Sreg(SegmentRegister::DS),
            src: Operand::Reg16(Register16::AX),
        });
}

fn emit_absolute_near_return(assembler: &mut Assembler, target: u16) {
    assembler
        .emit(Instruction::Push {
            src: Operand::Imm16(target),
        })
        .emit(Instruction::Ret { pop: 0 });
}

fn assemble_external_at(assembler: &Assembler, origin: u16, purpose: &str) -> Result<Vec<u8>> {
    assembler
        .assemble(CodeLocation {
            seg: 0,
            off: origin,
        })
        .with_context(|| format!("assemble typed V30 {purpose} at 0x{origin:04X}"))
        .map(|program| program.bytes().to_vec())
}

const fn near_displacement(origin: u16, target: u16) -> i16 {
    target.wrapping_sub(origin.wrapping_add(3)) as i16
}

fn catalog_shell(bytes: &[u8]) -> Result<ExternalTextProgram> {
    ensure_bytes(
        "DSH.COM",
        bytes,
        0x0151,
        &[0xba, 0xf0, 0x03, 0xb4, 0x09, 0xcd, 0x21],
    )?;
    ensure_bytes(
        "DSH.COM",
        bytes,
        0x020c,
        &[0xba, 0xa0, 0x03, 0xb4, 0x09, 0xcd, 0x21],
    )?;
    ensure_bytes(
        "DSH.COM",
        bytes,
        0x0269,
        &[
            0xba, 0x28, 0x04, 0x3c, 0x01, 0x74, 0x18, 0xba, 0x43, 0x04, 0x3c, 0x02, 0x74, 0x11,
            0xba, 0x5b, 0x04, 0x3c, 0x08, 0x74, 0x0a, 0xba, 0x72, 0x04, 0x3c, 0x0a, 0x74, 0x03,
            0xba, 0x83, 0x04, 0xb4, 0x09, 0xcd, 0x21,
        ],
    )?;
    let mut builder = ProgramBuilder::new(
        "DSH.COM",
        DSH_SHA256,
        ExternalTextStorage::SourceDiskFile,
        bytes,
    );
    for (address, instruction_offset, role) in [
        (0x03a0, 0x020c, "startup_banner"),
        (0x03f0, 0x0151, "interrupt_error"),
        (0x040f, 0x024a, "memory_resize_failure"),
        (0x0428, 0x0269, "exec_error_invalid_function"),
        (0x0443, 0x0270, "exec_error_file_not_found"),
        (0x045b, 0x0277, "exec_error_insufficient_memory"),
        (0x0472, 0x027e, "exec_error_invalid_environment"),
        (0x0483, 0x0285, "exec_error_invalid_format"),
    ] {
        builder.add_com_reference(address, instruction_offset, role)?;
    }
    builder.finish()
}

fn catalog_system_loader(bytes: &[u8]) -> Result<ExternalTextProgram> {
    ensure_bytes(
        "MEGDOS.SYS",
        bytes,
        0x02a9,
        &[
            0x83, 0xf8, 0x08, 0xba, 0x37, 0x09, 0x74, 0x03, 0xba, 0x2a, 0x08, 0xb0, 0x00, 0xe8,
            0x36, 0x04,
        ],
    )?;
    ensure_bytes(
        "MEGDOS.SYS",
        bytes,
        0x05ae,
        &[
            0xba, 0xc4, 0x08, 0xeb, 0x03, 0xba, 0xed, 0x08, 0xeb, 0x03, 0xba, 0x10, 0x09, 0x0e,
            0x1f, 0xb0, 0x00, 0xe8, 0x2d, 0x01,
        ],
    )?;
    let mut builder = ProgramBuilder::new(
        "MEGDOS.SYS",
        MEGDOS_SHA256,
        ExternalTextStorage::SourceDiskFile,
        bytes,
    );
    for (text_offset, instruction_offset, role) in [
        (0x082a, 0x02b1, "shell_launch_failure"),
        (0x0854, 0x0330, "config_file_too_large"),
        (0x0891, 0x037e, "invalid_config_command"),
        (0x08c4, 0x05ae, "file_not_found_or_invalid"),
        (0x08ed, 0x05b3, "too_many_disk_drives"),
        (0x0910, 0x05b8, "sector_size_too_large"),
        (0x0937, 0x06ae, "insufficient_memory"),
        (0x0937, 0x02ac, "shell_launch_insufficient_memory"),
    ] {
        builder.add_file_offset_reference(text_offset, instruction_offset, role)?;
    }
    builder.finish()
}

fn catalog_playback_driver(name: &'static str, bytes: &[u8]) -> Result<ExternalTextProgram> {
    let (source_sha256, scan, time_table_offset, time_consumer, time_pattern) = match name {
        "BPLAY.COM" => (
            BPLAY_SHA256,
            BPLAY_DIRECT_SCAN,
            BPLAY_TIME_TABLE_OFFSET,
            BPLAY_TIME_CONSUMER,
            BPLAY_TIME_TABLE_PATTERN,
        ),
        "FPLAY.COM" => (
            FPLAY_SHA256,
            FPLAY_DIRECT_SCAN,
            FPLAY_TIME_TABLE_OFFSET,
            FPLAY_TIME_CONSUMER,
            FPLAY_TIME_TABLE_PATTERN,
        ),
        _ => unreachable!("playback profile has a supported name"),
    };
    let mut builder = ProgramBuilder::new(
        name,
        source_sha256,
        ExternalTextStorage::SourceDiskFile,
        bytes,
    );
    collect_direct_dos_outputs(bytes, scan, &mut builder)?;
    if name == "BPLAY.COM" {
        ensure_bytes(name, bytes, 0x27bf, BPLAY_ALTERNATE_PATTERN)?;
        builder.add_com_reference(0x2af9, 0x27c1, "conditional_startup_banner")?;
        builder.add_com_reference(0x2b54, 0x27cb, "conditional_startup_banner")?;
    }
    ensure_bytes(
        name,
        bytes,
        time_consumer,
        &[0xb4, 0x09, 0x8b, 0x14, 0xcd, 0x21],
    )?;
    ensure_bytes(name, bytes, time_table_offset, time_pattern)?;
    for table_index in 0..TIME_TABLE_RECORDS {
        let target_word_offset = time_table_offset + table_index * TIME_TABLE_RECORD_SIZE + 2;
        let address = usize::from(read_u16(bytes, target_word_offset)?);
        builder.add_com_table_reference(
            address,
            time_table_offset,
            table_index,
            target_word_offset,
            time_consumer,
            "time_message_table",
        )?;
    }
    builder.finish()
}

fn catalog_sampling_driver(bytes: &[u8]) -> Result<ExternalTextProgram> {
    ensure_bytes(
        "BSAMP.COM",
        bytes,
        0x14f7,
        &[
            0x8c, 0x0e, 0xba, 0x15, 0xba, 0x94, 0x17, 0xb4, 0x09, 0xcd, 0x21,
        ],
    )?;
    let mut builder = ProgramBuilder::new(
        "BSAMP.COM",
        UPDATED_BSAMP_SHA256,
        ExternalTextStorage::OfficialFreezeFixResult,
        bytes,
    );
    for (address, instruction_offset, role) in [
        (0x1794, 0x14fb, "startup_banner"),
        (0x1806, 0x153a, "unload_status"),
        (0x17e9, 0x1554, "resident_status"),
    ] {
        builder.add_com_reference(address, instruction_offset, role)?;
    }
    builder.finish()
}

fn catalog_main_program(packed: &[u8]) -> Result<ExternalTextProgram> {
    let unpacked = decode_main_com_image(packed)?;
    ensure!(
        unpacked.len() == MAIN_UNPACKED_SIZE && sha256_hex(&unpacked) == MAIN_UNPACKED_SHA256,
        "MAIN.COM self-unpacked program differs from the supported view"
    );
    for (instruction_offset, pattern) in [
        (0x001f, &[0xba, 0x2b, 0x01, 0xb4, 0x09, 0xcd, 0x21][..]),
        (0x0065, &[0xba, 0xf0, 0x01, 0xb4, 0x09, 0xcd, 0x21][..]),
        (0x05bb, &[0xba, 0xd8, 0x06, 0xb4, 0x09, 0xcd, 0x21][..]),
    ] {
        ensure_bytes(
            "MAIN.COM self-unpacked program",
            &unpacked,
            instruction_offset,
            pattern,
        )?;
    }
    for (offset, pattern) in [
        (0x00c3, &[0x80, 0x0e, 0x66, 0x2b, 0x01][..]),
        (0x0661, &[0xc7, 0x06, 0xf0, 0x01, 0xf0, 0x07][..]),
        (0x26a2, &[0xf6, 0x06, 0x66, 0x2b, 0x01][..]),
    ] {
        ensure_bytes(
            "MAIN.COM target-like nonreference",
            &unpacked,
            offset,
            pattern,
        )?;
    }

    let mut builder = ProgramBuilder::new_view(
        "MAIN.COM",
        MAIN_SHA256,
        packed.len(),
        ExternalTextStorage::SourceDiskFile,
        ExternalTextView::SelfUnpackedCom {
            packed_stream_offset: MAIN_COM_PACKED_STREAM_OFFSET,
        },
        &unpacked,
    );
    for (address, instruction_offset, role) in [
        (0x012b, 0x001f, "memory_resize_failure"),
        (0x01f0, 0x0065, "invalid_command_line_help"),
        (0x06d8, 0x05bb, "game_file_not_found"),
    ] {
        builder.add_com_reference(address, instruction_offset, role)?;
    }
    builder.finish()
}

fn catalog_menu_program(bytes: &[u8]) -> Result<ExternalTextProgram> {
    ensure!(
        MENU_DISK_ERROR_TABLE_OFFSET + COM_ORIGIN == MENU_DISK_ERROR_TABLE_ADDRESS
            && MENU_FLOPPY_ERROR_TABLE_OFFSET + COM_ORIGIN == MENU_FLOPPY_ERROR_TABLE_ADDRESS,
        "MENU.COM pointer-table address basis changed"
    );
    ensure_bytes(
        "MENU.COM",
        bytes,
        MENU_TEXT_RENDERER_OFFSET,
        &[0x8a, 0x3e, 0x57, 0x12, 0x8a, 0x1e, 0x58, 0x12],
    )?;
    ensure_bytes(
        "MENU.COM",
        bytes,
        MENU_DISK_ERROR_CONSUMER_OFFSET,
        &[0xbb, 0x71, 0x06, 0x03, 0xc0, 0x03, 0xd8, 0x8b, 0x37],
    )?;
    ensure_bytes(
        "MENU.COM",
        bytes,
        MENU_PROCESS_ERROR_CONSUMER_OFFSET,
        &[0xba, 0x43, 0x09, 0x3d, 0x01, 0x00],
    )?;
    ensure_bytes(
        "MENU.COM",
        bytes,
        MENU_FLOPPY_ERROR_CONSUMER_OFFSET,
        &[0xbb, 0x7e, 0x0f, 0x03, 0xc0, 0x03, 0xd8, 0x2e, 0x8b, 0x37],
    )?;

    let mut builder = ProgramBuilder::new(
        "MENU.COM",
        MENU_SHA256,
        ExternalTextStorage::SourceDiskFile,
        bytes,
    );
    for (address, instruction_offset) in MENU_DIRECT_DOS_OUTPUTS {
        builder.add_com_reference(address, instruction_offset, "direct_dos_output")?;
    }
    for table_index in 0..MENU_DISK_ERROR_COUNT {
        let target_word_offset = MENU_DISK_ERROR_TABLE_OFFSET + table_index * 2;
        builder.add_com_table_reference(
            usize::from(read_u16(bytes, target_word_offset)?),
            MENU_DISK_ERROR_TABLE_OFFSET,
            table_index,
            target_word_offset,
            MENU_DISK_ERROR_CONSUMER_OFFSET,
            "disk_error_pointer_table",
        )?;
    }
    for (address, instruction_offset) in MENU_PROCESS_ERROR_OUTPUTS {
        builder.add_com_reference(address, instruction_offset, "process_error_switch")?;
    }
    for table_index in 0..MENU_FLOPPY_ERROR_COUNT {
        let target_word_offset = MENU_FLOPPY_ERROR_TABLE_OFFSET + table_index * 2;
        builder.add_com_table_reference(
            usize::from(read_u16(bytes, target_word_offset)?),
            MENU_FLOPPY_ERROR_TABLE_OFFSET,
            table_index,
            target_word_offset,
            MENU_FLOPPY_ERROR_CONSUMER_OFFSET,
            "floppy_bios_error_pointer_table",
        )?;
    }
    for (address, instruction_offset, terminator, role) in MENU_FIXED_RENDERER_OUTPUTS {
        builder.add_com_register_reference(
            address,
            instruction_offset,
            ExternalTextRegister::Si,
            terminator,
            role,
        )?;
    }
    builder.finish()
}

fn collect_direct_dos_outputs(
    bytes: &[u8],
    scan: std::ops::Range<usize>,
    builder: &mut ProgramBuilder<'_>,
) -> Result<()> {
    ensure!(
        scan.end <= bytes.len(),
        "{} direct output scan exceeds the program",
        builder.name
    );
    for offset in scan {
        let tail = &bytes[offset..];
        if tail.len() >= 7 && tail[0..3] == [0xb4, 0x09, 0xba] && tail[5..7] == [0xcd, 0x21] {
            builder.add_com_reference(
                usize::from(u16::from_le_bytes([tail[3], tail[4]])),
                offset + 2,
                "direct_dos_output",
            )?;
        }
        if tail.len() >= 7
            && tail[0] == 0xba
            && tail[3..5] == [0xb4, 0x09]
            && tail[5..7] == [0xcd, 0x21]
        {
            builder.add_com_reference(
                usize::from(u16::from_le_bytes([tail[1], tail[2]])),
                offset,
                "direct_dos_output",
            )?;
        }
    }
    Ok(())
}

impl<'a> ProgramBuilder<'a> {
    fn new(
        name: &'static str,
        source_sha256: &'static str,
        storage: ExternalTextStorage,
        bytes: &'a [u8],
    ) -> Self {
        Self::new_view(
            name,
            source_sha256,
            bytes.len(),
            storage,
            ExternalTextView::SourceFile,
            bytes,
        )
    }

    fn new_view(
        name: &'static str,
        source_sha256: &'static str,
        source_size: usize,
        storage: ExternalTextStorage,
        view: ExternalTextView,
        bytes: &'a [u8],
    ) -> Self {
        Self {
            name,
            source_sha256,
            source_size,
            storage,
            view,
            bytes,
            entries: BTreeMap::new(),
        }
    }

    fn add_com_reference(
        &mut self,
        runtime_address: usize,
        instruction_offset: usize,
        role: &str,
    ) -> Result<()> {
        let text_offset = runtime_address
            .checked_sub(COM_ORIGIN)
            .with_context(|| format!("{} text address lies below COM origin", self.name))?;
        ensure!(
            self.bytes.get(instruction_offset) == Some(&0xba),
            "{} reference at {instruction_offset:#x} is not MOV DX, imm16",
            self.name
        );
        ensure!(
            usize::from(read_u16(self.bytes, instruction_offset + 1)?) == runtime_address,
            "{} reference target changed at {instruction_offset:#x}",
            self.name
        );
        self.add_reference(
            text_offset,
            runtime_address,
            ExternalTextTerminator::DosDollar,
            ExternalTextReference::ComAddressImmediate {
                instruction_offset,
                target_word_offset: instruction_offset + 1,
                role: role.to_owned(),
            },
        )
    }

    fn add_file_offset_reference(
        &mut self,
        text_offset: usize,
        instruction_offset: usize,
        role: &str,
    ) -> Result<()> {
        ensure!(
            self.bytes.get(instruction_offset) == Some(&0xba),
            "{} reference at {instruction_offset:#x} is not MOV DX, imm16",
            self.name
        );
        ensure!(
            usize::from(read_u16(self.bytes, instruction_offset + 1)?) == text_offset,
            "{} reference target changed at {instruction_offset:#x}",
            self.name
        );
        self.add_reference(
            text_offset,
            text_offset,
            ExternalTextTerminator::Null,
            ExternalTextReference::FileOffsetImmediate {
                instruction_offset,
                target_word_offset: instruction_offset + 1,
                role: role.to_owned(),
            },
        )
    }

    fn add_com_register_reference(
        &mut self,
        runtime_address: usize,
        instruction_offset: usize,
        register: ExternalTextRegister,
        terminator: ExternalTextTerminator,
        role: &str,
    ) -> Result<()> {
        let text_offset = runtime_address
            .checked_sub(COM_ORIGIN)
            .with_context(|| format!("{} text address lies below COM origin", self.name))?;
        let expected_register = match register {
            ExternalTextRegister::Si => Register16::SI,
        };
        let decoded = decode_bytes(
            self.bytes
                .get(instruction_offset..)
                .with_context(|| format!("{} reference lies outside the view", self.name))?,
        )?;
        ensure!(
            decoded.byte_len == 3
                && decoded.prefixes.is_empty()
                && decoded.instruction
                    == (Instruction::Mov {
                        dest: Operand::Reg16(expected_register),
                        src: Operand::Imm16(u16::try_from(runtime_address)?),
                    }),
            "{} reference at {instruction_offset:#x} is not the expected typed MOV {}, imm16",
            self.name,
            expected_register.name()
        );
        self.add_reference(
            text_offset,
            runtime_address,
            terminator,
            ExternalTextReference::ComAddressRegisterImmediate {
                instruction_offset,
                target_word_offset: instruction_offset + 1,
                register,
                role: role.to_owned(),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_com_table_reference(
        &mut self,
        runtime_address: usize,
        table_offset: usize,
        table_index: usize,
        target_word_offset: usize,
        consumer_offset: usize,
        role: &str,
    ) -> Result<()> {
        let text_offset = runtime_address
            .checked_sub(COM_ORIGIN)
            .with_context(|| format!("{} table text address lies below COM origin", self.name))?;
        self.add_reference(
            text_offset,
            runtime_address,
            ExternalTextTerminator::DosDollar,
            ExternalTextReference::ComAddressTable {
                table_offset,
                table_index,
                target_word_offset,
                consumer_offset,
                role: role.to_owned(),
            },
        )
    }

    fn add_reference(
        &mut self,
        text_offset: usize,
        runtime_address: usize,
        terminator: ExternalTextTerminator,
        reference: ExternalTextReference,
    ) -> Result<()> {
        let raw = read_terminated(self.bytes, text_offset, terminator)?;
        let pending = self
            .entries
            .entry(text_offset)
            .or_insert_with(|| PendingEntry {
                runtime_address,
                terminator,
                raw: raw.to_vec(),
                references: Vec::new(),
            });
        ensure!(
            pending.runtime_address == runtime_address
                && pending.terminator == terminator
                && pending.raw == raw,
            "{} text at {text_offset:#x} has conflicting boundaries",
            self.name
        );
        ensure!(
            !pending.references.contains(&reference),
            "{} text at {text_offset:#x} has a duplicate reference",
            self.name
        );
        pending.references.push(reference);
        Ok(())
    }

    fn finish(self) -> Result<ExternalTextProgram> {
        let stem = self.name.trim_end_matches(".COM").trim_end_matches(".SYS");
        let mut target_like_nonreferences = Vec::new();
        if matches!(
            self.name,
            "BPLAY.COM" | "FPLAY.COM" | "MAIN.COM" | "MENU.COM"
        ) {
            for (text_offset, pending) in &self.entries {
                let mut expected = pending
                    .references
                    .iter()
                    .map(reference_target_word_offset)
                    .collect::<Vec<_>>();
                expected.sort_unstable();
                expected.dedup();
                let mut actual = find_all_u16(
                    self.bytes,
                    u16::try_from(pending.runtime_address)
                        .context("playback-driver text address exceeds 16 bits")?,
                );
                actual.sort_unstable();
                let unexpected = actual
                    .iter()
                    .copied()
                    .filter(|offset| !expected.contains(offset))
                    .collect::<Vec<_>>();
                for offset in unexpected {
                    let reason =
                        known_target_like_nonreference(self.name, pending.runtime_address, offset)?;
                    target_like_nonreferences.push(ExternalTextNonReference {
                        entry_id: format!("{stem}_{text_offset:04X}"),
                        offset,
                        observed_value: pending.runtime_address,
                        reason: reason.to_owned(),
                    });
                }
                actual.retain(|offset| expected.contains(offset));
                ensure!(
                    actual == expected,
                    "{} text address {:#06x} has unclassified target-like words: expected {expected:?}, found {actual:?}",
                    self.name,
                    pending.runtime_address
                );
            }
        }
        ensure!(
            (self.name != "FPLAY.COM" || target_like_nonreferences.len() == 1)
                && (self.name != "BPLAY.COM" || target_like_nonreferences.is_empty())
                && (self.name != "MAIN.COM" || target_like_nonreferences.len() == 3)
                && (self.name != "MENU.COM" || target_like_nonreferences.is_empty()),
            "{} target-like nonreference population changed",
            self.name
        );
        let entries = self
            .entries
            .into_iter()
            .map(|(text_offset, pending)| {
                let source_text = SHIFT_JIS
                    .decode_without_bom_handling_and_without_replacement(&pending.raw)
                    .with_context(|| {
                        format!(
                            "{} has invalid Shift-JIS text at {text_offset:#x}",
                            self.name
                        )
                    })?
                    .into_owned();
                ensure!(
                    !source_text.is_empty(),
                    "{} has an empty consumer-linked text at {text_offset:#x}",
                    self.name
                );
                Ok(ExternalTextEntry {
                    id: format!("{stem}_{text_offset:04X}"),
                    text_offset,
                    runtime_address: pending.runtime_address,
                    terminator: pending.terminator,
                    byte_budget: pending.raw.len() + 1,
                    raw_hex: encode_hex(&pending.raw),
                    source_text,
                    references: pending.references,
                    ko: String::new(),
                    status: UNTRANSLATED_STATUS.to_owned(),
                    notes: String::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let ids: BTreeSet<_> = entries.iter().map(|entry| entry.id.as_str()).collect();
        ensure!(
            ids.len() == entries.len(),
            "{} generated duplicate text IDs",
            self.name
        );
        let reference_count = entries.iter().map(|entry| entry.references.len()).sum();
        Ok(ExternalTextProgram {
            name: self.name.to_owned(),
            source_sha256: self.source_sha256.to_owned(),
            source_size: self.source_size,
            storage: self.storage,
            view: self.view,
            view_sha256: sha256_hex(self.bytes),
            view_size: self.bytes.len(),
            entry_count: entries.len(),
            reference_count,
            target_like_nonreferences,
            entries,
        })
    }
}

fn read_terminated(
    bytes: &[u8],
    offset: usize,
    terminator: ExternalTextTerminator,
) -> Result<&[u8]> {
    let marker = terminator_byte(terminator);
    let tail = bytes
        .get(offset..)
        .with_context(|| format!("external text starts outside storage at {offset:#x}"))?;
    let length = tail
        .iter()
        .position(|byte| *byte == marker)
        .with_context(|| format!("external text at {offset:#x} has no terminator"))?;
    Ok(&tail[..length])
}

fn terminator_byte(terminator: ExternalTextTerminator) -> u8 {
    match terminator {
        ExternalTextTerminator::DosDollar => b'$',
        ExternalTextTerminator::Null => 0,
        ExternalTextTerminator::AtSign => b'@',
    }
}

fn protected_reference_target(
    entry: &ExternalTextEntry,
    reference: &ExternalTextReference,
) -> Result<usize> {
    let target = match reference {
        ExternalTextReference::ComAddressImmediate { .. }
        | ExternalTextReference::ComAddressRegisterImmediate { .. }
        | ExternalTextReference::ComAddressTable { .. } => {
            ensure!(
                entry.text_offset.checked_add(COM_ORIGIN) == Some(entry.runtime_address),
                "{} protected COM address basis is inconsistent",
                entry.id
            );
            entry.runtime_address
        }
        ExternalTextReference::FileOffsetImmediate { .. } => {
            ensure!(
                entry.runtime_address == entry.text_offset,
                "{} protected file-offset basis is inconsistent",
                entry.id
            );
            entry.text_offset
        }
    };
    Ok(target)
}

fn relocated_reference_target(
    destination: usize,
    reference: &ExternalTextReference,
) -> Result<usize> {
    match reference {
        ExternalTextReference::ComAddressImmediate { .. }
        | ExternalTextReference::ComAddressRegisterImmediate { .. }
        | ExternalTextReference::ComAddressTable { .. } => destination
            .checked_add(COM_ORIGIN)
            .context("relocated COM address overflows"),
        ExternalTextReference::FileOffsetImmediate { .. } => Ok(destination),
    }
}

fn ensure_external_controls_preserved(source: &[u8], replacement: &[u8]) -> Result<()> {
    ensure!(
        external_control_stream(replacement)? == external_control_stream(source)?,
        "ordered ANSI and control bytes differ"
    );
    Ok(())
}

fn external_control_stream(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut controls = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if is_shift_jis_lead(byte) {
            let trail = *bytes
                .get(cursor + 1)
                .with_context(|| format!("truncated Shift-JIS character at byte {cursor:#x}"))?;
            ensure!(
                is_shift_jis_trail(trail),
                "invalid Shift-JIS trail byte 0x{trail:02X} at byte {:#x}",
                cursor + 1
            );
            cursor += 2;
            continue;
        }
        if byte == 0x1b {
            ensure!(
                bytes.get(cursor + 1) == Some(&b'['),
                "ESC at byte {cursor:#x} is not an ANSI CSI sequence"
            );
            let relative_end = bytes[cursor + 2..]
                .iter()
                .position(|candidate| (0x40..=0x7e).contains(candidate))
                .with_context(|| {
                    format!("ANSI CSI sequence at byte {cursor:#x} is unterminated")
                })?;
            let end = cursor + 2 + relative_end + 1;
            controls.push(bytes[cursor..end].to_vec());
            cursor = end;
            continue;
        }
        if byte == b'@' {
            controls.push(vec![byte]);
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            let command_length = match bytes.get(cursor + 1..) {
                Some([b'P', ..]) => Some(2),
                Some([b'C', parameter, ..]) if parameter.is_ascii_digit() => Some(3),
                _ => None,
            };
            if let Some(length) = command_length {
                controls.push(bytes[cursor..cursor + length].to_vec());
                cursor += length;
                continue;
            }
        }
        if byte < 0x20 || byte == 0x7f {
            controls.push(vec![byte]);
        }
        cursor += 1;
    }
    Ok(controls)
}

const fn is_shift_jis_lead(byte: u8) -> bool {
    matches!(byte, 0x81..=0x9f | 0xe0..=0xfc)
}

const fn is_shift_jis_trail(byte: u8) -> bool {
    matches!(byte, 0x40..=0x7e | 0x80..=0xfc)
}

fn required<'a>(files: &'a BTreeMap<String, Vec<u8>>, name: &str) -> Result<&'a [u8]> {
    files
        .get(name)
        .map(Vec::as_slice)
        .with_context(|| format!("external text source is missing {name}"))
}

fn verify_file(name: &str, bytes: &[u8], expected_sha256: &str) -> Result<()> {
    ensure!(
        sha256_hex(bytes) == expected_sha256,
        "{name} differs from the supported external text source"
    );
    Ok(())
}

fn ensure_bytes(name: &str, bytes: &[u8], offset: usize, expected: &[u8]) -> Result<()> {
    ensure!(
        bytes.get(offset..offset + expected.len()) == Some(expected),
        "{name} consumer signature changed at {offset:#x}"
    );
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let raw: [u8; 2] = bytes
        .get(offset..offset + 2)
        .with_context(|| format!("truncated 16-bit field at {offset:#x}"))?
        .try_into()
        .expect("a two-byte range converts to an array");
    Ok(u16::from_le_bytes(raw))
}

fn reference_target_word_offset(reference: &ExternalTextReference) -> usize {
    match reference {
        ExternalTextReference::ComAddressImmediate {
            target_word_offset, ..
        }
        | ExternalTextReference::ComAddressRegisterImmediate {
            target_word_offset, ..
        }
        | ExternalTextReference::FileOffsetImmediate {
            target_word_offset, ..
        }
        | ExternalTextReference::ComAddressTable {
            target_word_offset, ..
        } => *target_word_offset,
    }
}

fn find_all_u16(bytes: &[u8], value: u16) -> Vec<usize> {
    let needle = value.to_le_bytes();
    bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == needle).then_some(offset))
        .collect()
}

fn known_target_like_nonreference(
    name: &str,
    runtime_address: usize,
    offset: usize,
) -> Result<&'static str> {
    match (name, runtime_address, offset) {
        ("FPLAY.COM", 0x2e58, 0x1d40) => {
            Ok("POP AX byte followed by a CS prefix, not a 16-bit operand")
        }
        ("MAIN.COM", 0x012b, 0x00c6) => {
            Ok("high byte of the 0x2B66 memory operand followed by the OR immediate 01h")
        }
        ("MAIN.COM", 0x012b, 0x26a5) => {
            Ok("high byte of the 0x2B66 memory operand followed by the TEST immediate 01h")
        }
        ("MAIN.COM", 0x01f0, 0x0663) => {
            Ok("destination address of a MOV that installs an interrupt-vector offset")
        }
        _ => bail!(
            "{name} text address {runtime_address:#06x} has an unclassified target-like word at {offset:#x}"
        ),
    }
}

fn validate_translation_states(catalog: &ExternalTextCatalog) -> Result<()> {
    for program in &catalog.programs {
        for entry in &program.entries {
            validate_translation_state(entry)?;
        }
    }
    Ok(())
}

fn validate_translation_state(entry: &ExternalTextEntry) -> Result<()> {
    match entry.status.as_str() {
        "untranslated" => ensure!(
            entry.ko.is_empty(),
            "{}: untranslated entry must have empty Korean text",
            entry.id
        ),
        "in_progress" | "needs_review" | "needs_human_review" | "distribution_eligible" => ensure!(
            !entry.ko.is_empty(),
            "{}: {} entry must have Korean text",
            entry.id,
            entry.status
        ),
        other => bail!("{}: unsupported translation status {other:?}", entry.id),
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_translated(program: &mut ExternalTextProgram, text: &str) {
        program.entries[0].ko = text.to_owned();
        program.entries[0].status = "needs_human_review".to_owned();
    }

    #[test]
    fn builder_keeps_two_references_to_one_dos_string() {
        let mut bytes = vec![0; 0x130];
        bytes[0x20..0x24].copy_from_slice(b"abc$");
        bytes[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        bytes[0x10..0x13].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "test",
            ExternalTextStorage::SourceDiskFile,
            &bytes,
        );
        builder.add_com_reference(0x120, 0, "first").unwrap();
        builder.add_com_reference(0x120, 0x10, "second").unwrap();

        let program = builder.finish().unwrap();

        assert_eq!(program.entry_count, 1);
        assert_eq!(program.reference_count, 2);
        assert_eq!(program.entries[0].source_text, "abc");
    }

    #[test]
    fn builder_records_a_typed_si_reference_and_at_sign_boundary() {
        let mut bytes = vec![0; 0x130];
        bytes[0x20..0x24].copy_from_slice(b"abc@");
        bytes[0x00..0x03].copy_from_slice(&[0xbe, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "test",
            ExternalTextStorage::SourceDiskFile,
            &bytes,
        );
        builder
            .add_com_register_reference(
                0x120,
                0,
                ExternalTextRegister::Si,
                ExternalTextTerminator::AtSign,
                "fixed_renderer",
            )
            .unwrap();

        let program = builder.finish().unwrap();

        assert_eq!(program.entries[0].source_text, "abc");
        assert_eq!(
            program.entries[0].terminator,
            ExternalTextTerminator::AtSign
        );
        assert!(matches!(
            program.entries[0].references.as_slice(),
            [ExternalTextReference::ComAddressRegisterImmediate {
                register: ExternalTextRegister::Si,
                ..
            }]
        ));
    }

    #[test]
    fn relocation_updates_every_com_reference_and_keeps_original_input() {
        let mut source = vec![0; 0x130];
        source[0x20..0x24].copy_from_slice(b"abc$");
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        source[0x10..0x13].copy_from_slice(&[0xba, 0x20, 0x01]);
        let immutable_source = source.clone();
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_com_reference(0x120, 0, "first").unwrap();
        builder.add_com_reference(0x120, 0x10, "second").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "가나");
        let codebook = BTreeMap::from([('가', [0xeb, 0x41]), ('나', [0xeb, 0x42])]);

        let (updated, report) =
            rebuild_external_text_source_file(&program, &source, &codebook).unwrap();

        let relocated_address = u16::from_le_bytes([updated[1], updated[2]]);
        assert_eq!(
            relocated_address,
            u16::from_le_bytes([updated[0x11], updated[0x12]])
        );
        let relocated_offset = usize::from(relocated_address) - COM_ORIGIN;
        assert_eq!(
            &updated[relocated_offset..relocated_offset + 5],
            &[0xeb, 0x41, 0xeb, 0x42, b'$']
        );
        assert_eq!(source, immutable_source);
        assert_eq!(report.changed_entries, 1);
        assert_eq!(report.updated_references, 2);
    }

    #[test]
    fn file_offset_reference_targets_appended_null_text() {
        let mut source = vec![0; 0x40];
        source[0x20..0x24].copy_from_slice(b"abc\0");
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x00]);
        let mut builder = ProgramBuilder::new(
            "TEST.SYS",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_file_offset_reference(0x20, 0, "error").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "longer");

        let (updated, _) =
            rebuild_external_text_source_file(&program, &source, &BTreeMap::new()).unwrap();

        let relocated_offset = usize::from(u16::from_le_bytes([updated[1], updated[2]]));
        assert_eq!(
            &updated[relocated_offset..relocated_offset + 7],
            b"longer\0"
        );
    }

    #[test]
    fn one_reference_site_with_two_roles_is_rewritten_once() {
        let mut source = vec![0; 0x40];
        source[0x20..0x24].copy_from_slice(b"abc$");
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_com_reference(0x120, 0, "direct").unwrap();
        builder.add_com_reference(0x120, 0, "switch").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "longer");

        let (_, report) =
            rebuild_external_text_source_file(&program, &source, &BTreeMap::new()).unwrap();

        assert_eq!(report.updated_references, 1);
    }

    #[test]
    fn fitting_translation_stays_in_its_original_slot() {
        let mut source = vec![0; 0x40];
        source[0x20..0x24].copy_from_slice(b"abc$");
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_com_reference(0x120, 0, "message").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "xy");

        let (updated, report) =
            rebuild_external_text_source_file(&program, &source, &BTreeMap::new()).unwrap();

        assert_eq!(u16::from_le_bytes([updated[1], updated[2]]), 0x120);
        assert_eq!(&updated[0x20..0x24], b"xy$\0");
        assert_eq!(updated.len(), source.len());
        assert_eq!(report.changed_entries, 1);
        assert_eq!(report.relocated_entries, 0);
        assert_eq!(report.updated_references, 0);
    }

    #[test]
    fn translation_preserves_ansi_and_line_controls() {
        let source_text = b"\x1b[31mabc\r\n\x1b[0m$";
        let mut source = vec![0; 0x80];
        source[0x20..0x20 + source_text.len()].copy_from_slice(source_text);
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_com_reference(0x120, 0, "colored_line").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "\u{1b}[31m가\r\n\u{1b}[0m");
        let codebook = BTreeMap::from([('가', [0xeb, 0x41])]);

        rebuild_external_text_source_file(&program, &source, &codebook).unwrap();

        mark_translated(&mut program, "\u{1b}[31m가\u{1b}[0m");
        let error = rebuild_external_text_source_file(&program, &source, &codebook).unwrap_err();
        assert!(error.to_string().contains("control stream"));
    }

    #[test]
    fn source_preserved_review_leaves_program_bytes_unchanged() {
        let mut source = vec![0; 0x40];
        source[0x20..0x24].copy_from_slice(b"abc$");
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_com_reference(0x120, 0, "banner").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "abc");

        let (updated, report) =
            rebuild_external_text_source_file(&program, &source, &BTreeMap::new()).unwrap();

        assert_eq!(updated, source);
        assert_eq!(report.changed_entries, 0);
        assert_eq!(report.updated_references, 0);
        assert_eq!(report.original_view_sha256, report.updated_view_sha256);
    }

    #[test]
    fn translation_preserves_menu_renderer_commands() {
        let source_text = b"\\P\0\0\\C6abc@\\C7$";
        let mut source = vec![0; 0x80];
        source[0x20..0x20 + source_text.len()].copy_from_slice(source_text);
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder
            .add_com_reference(0x120, 0, "menu_renderer")
            .unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "\\P\0\0\\C6가@\\C7");
        let codebook = BTreeMap::from([('가', [0xeb, 0x41])]);

        rebuild_external_text_source_file(&program, &source, &codebook).unwrap();

        mark_translated(&mut program, "\\P\0\0\\C6가\\C7");
        let error = rebuild_external_text_source_file(&program, &source, &codebook).unwrap_err();
        assert!(error.to_string().contains("control stream"));
    }

    #[test]
    fn shift_jis_trail_bytes_are_not_renderer_commands() {
        ensure_external_controls_preserved(&[0x81, b'@', 0x81, b'\\'], b"plain").unwrap();
    }

    #[test]
    fn translation_cannot_embed_its_consumer_terminator() {
        let mut source = vec![0; 0x80];
        source[0x20..0x24].copy_from_slice(b"abc@");
        source[0x00..0x03].copy_from_slice(&[0xbe, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder
            .add_com_register_reference(
                0x120,
                0,
                ExternalTextRegister::Si,
                ExternalTextTerminator::AtSign,
                "fixed_renderer",
            )
            .unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "mail@example");

        let error =
            rebuild_external_text_source_file(&program, &source, &BTreeMap::new()).unwrap_err();

        assert!(error.to_string().contains("consumer terminator"));
    }

    #[test]
    fn gaiji_installer_resumes_the_typed_entry_target_from_any_tail_location() {
        let profile = ExternalEntryProfile {
            logical_origin: 0x100,
            behavior: ExternalEntryBehavior::NearJump { target: 0x4321 },
        };
        let glyph = ExternalTextGaijiGlyph {
            character: '가',
            jis_code: 0x7621,
            shift_jis_code: [0xeb, 0x40],
            bitmap: [0; GLYPH_BYTES],
        };

        let first = assemble_external_gaiji_installer(
            0x200,
            &[0x300],
            std::slice::from_ref(&glyph),
            profile,
        )
        .unwrap();
        let second = assemble_external_gaiji_installer(
            0x500,
            &[0x600],
            std::slice::from_ref(&glyph),
            profile,
        )
        .unwrap();

        assert!(first.bytes().ends_with(&[0x68, 0x21, 0x43, 0xc3]));
        assert!(second.bytes().ends_with(&[0x68, 0x21, 0x43, 0xc3]));
    }

    #[test]
    fn com_relocation_rejects_an_unrepresentable_runtime_address() {
        let mut source = vec![0; 0xff01];
        source[0x20..0x24].copy_from_slice(b"abc$");
        source[0x00..0x03].copy_from_slice(&[0xba, 0x20, 0x01]);
        let mut builder = ProgramBuilder::new(
            "TEST.COM",
            "fixture",
            ExternalTextStorage::SourceDiskFile,
            &source,
        );
        builder.add_com_reference(0x120, 0, "message").unwrap();
        let mut program = builder.finish().unwrap();
        mark_translated(&mut program, "longer");

        let error =
            rebuild_external_text_source_file(&program, &source, &BTreeMap::new()).unwrap_err();

        assert!(error.to_string().contains("exceeds 16 bits"));
    }
}
