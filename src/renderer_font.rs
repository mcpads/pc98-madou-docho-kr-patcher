use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use encoding_rs::SHIFT_JIS;
use serde::Serialize;
use v30::{
    Assembler, CallTarget, CodeLocation, Condition, EffectiveAddress, EffectiveAddressBase,
    EffectiveAddressDisplacement, Instruction, JmpTarget, LoopCondition, Operand, OperandSize,
    Register8, Register16, SegmentRegister, ShiftCount,
};

use crate::compile_lz::{decode_exact_compile_lz, encode_compile_lz};
use crate::font::{FontReport, GLYPH_BYTES, font_report, rasterize_character};
use crate::game_data::{MAIN_COM_PACKED_STREAM_OFFSET, decode_main_com_image};
use crate::josa::{
    KoreanParticle, PARTICLE_MARKERS, has_batchim, is_particle_marker, plan_renderer_characters,
};
use crate::source_disk::sha256_hex;

pub(crate) use crate::josa::is_modern_hangul;

pub(crate) const RENDERER_FONT_FILE: &str = "KFONT.BIN";
pub(crate) const RENDERER_GLYPH_CAPACITY: usize = 940;

const MAIN_COM_NAME: &str = "MAIN.COM";
const MAIN_COM_SHA256: &str = "50954a2dce3f4d424558718b753029ae27b0a33273563cb2b3792318da1e6957";
const MAIN_COM_ENTRY_LOGICAL_OFFSET: u16 = 0x0100;
const RENDERER_SHEET_BASE_ROW: u8 = 0x75;
const RENDERER_SHEET_ROWS: usize = 10;
const JIS_FIRST_CELL: u8 = 0x21;
const JIS_LAST_CELL: u8 = 0x7e;
const CELLS_PER_ROW: usize = 94;
const RENDERER_SHEET_BYTES: usize = RENDERER_GLYPH_CAPACITY * GLYPH_BYTES;
const BIOS_GAIJI_RECORD_PREFIX: [u8; 2] = [0x02, 0x02];
const DOS_PARAGRAPH_BYTES: usize = 16;
const RENDERER_SHEET_PARAGRAPHS: u16 = RENDERER_SHEET_BYTES.div_ceil(DOS_PARAGRAPH_BYTES) as u16;
const MAIN_COM_RESIDENT_END_LOGICAL_OFFSET: u16 = 0x2cff;
const MAIN_COM_OVERLAY_SEGMENT_PARAGRAPHS: u16 =
    (MAIN_COM_RESIDENT_END_LOGICAL_OFFSET & 0xfff0) / DOS_PARAGRAPH_BYTES as u16;
const MAIN_COM_WORKING_PARAGRAPHS_FROM_OVERLAY: u16 = 0x6a00;
const MAIN_COM_FONT_SEGMENT_PARAGRAPHS: u16 =
    MAIN_COM_OVERLAY_SEGMENT_PARAGRAPHS + MAIN_COM_WORKING_PARAGRAPHS_FROM_OVERLAY;
const MAIN_COM_ORIGINAL_WORKING_PARAGRAPHS: u16 = 0x6a00;
const MAIN_COM_UPDATED_WORKING_PARAGRAPHS: u16 =
    MAIN_COM_ORIGINAL_WORKING_PARAGRAPHS + RENDERER_SHEET_PARAGRAPHS;
const MAIN_COM_MEMORY_RESIZE_LOGICAL_OFFSET: u16 = 0x0114;
const MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET: usize =
    MAIN_COM_MEMORY_RESIZE_LOGICAL_OFFSET as usize - MAIN_COM_ENTRY_LOGICAL_OFFSET as usize;
const MAIN_JIS_CAPTURE_LOGICAL_OFFSET: u16 = 0x5025;
const MAIN_JIS_CAPTURE_DECODED_OFFSET: usize =
    MAIN_JIS_CAPTURE_LOGICAL_OFFSET as usize - MAIN_COM_ENTRY_LOGICAL_OFFSET as usize;
const MAIN_DRAW_HOOK_LOGICAL_OFFSET: u16 = 0x5096;
const MAIN_DRAW_HOOK_DECODED_OFFSET: usize =
    MAIN_DRAW_HOOK_LOGICAL_OFFSET as usize - MAIN_COM_ENTRY_LOGICAL_OFFSET as usize;
const MAIN_DRAW_FINISH_LOGICAL_OFFSET: u16 = 0x50c1;
const MAIN_OVERLAY_LOAD_OFFSET: usize = 0x0100;
const ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES: [(usize, [u8; 6]); 2] = [
    (0x11d0, [0xb8, 0x4f, 0x24, 0xe8, 0x4a, 0x3d]),
    (0x13f5, [0xb8, 0x4f, 0x24, 0xe8, 0x25, 0x3b]),
];

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RendererFontReport {
    pub glyph_count: usize,
    pub glyph_capacity: usize,
    pub sheet_size: usize,
    pub font_file: String,
    pub font_file_size: usize,
    pub font_file_sha256: String,
    pub reserved_font_paragraphs: u16,
    pub font_segment_delta_from_main_overlay: String,
    pub main_com_loader_logical_offset: usize,
    pub main_draw_hook_logical_offset: String,
    pub original_main_com_sha256: String,
    pub updated_main_com_sha256: String,
    pub original_main_com_size: usize,
    pub updated_main_com_size: usize,
    pub font: FontReport,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct BiosGaijiRecord {
    pub(crate) jis_code: u16,
    pub(crate) bitmap: [u8; GLYPH_BYTES],
}

pub(crate) struct RendererFontPlan {
    codebook: BTreeMap<char, [u8; 2]>,
    font_file: Vec<u8>,
    font: FontReport,
    particle_selector: RendererParticleSelector,
}

impl RendererFontPlan {
    pub(crate) fn codebook(&self) -> &BTreeMap<char, [u8; 2]> {
        &self.codebook
    }

    pub(crate) fn glyph_count(&self) -> usize {
        self.codebook.len()
    }

    pub(crate) fn particle_selector(&self) -> &RendererParticleSelector {
        &self.particle_selector
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct RendererParticleSelector {
    first_hangul_jis: u16,
    first_with_batchim_jis: u16,
    marker_jis: [u16; 4],
    form_jis: [[u16; 2]; 4],
}

impl RendererParticleSelector {
    fn from_plan(
        planned_characters: &[char],
        jis_by_character: &BTreeMap<char, u16>,
    ) -> Result<Self> {
        let first_hangul = planned_characters
            .first()
            .copied()
            .context("renderer font plan has no Hangul glyph")?;
        let first_with_batchim = planned_characters
            .iter()
            .copied()
            .find(|character| is_modern_hangul(*character) && has_batchim(*character).unwrap())
            .context("renderer font plan has no batchim-bearing glyph")?;
        let first_hangul_jis = *jis_by_character
            .get(&first_hangul)
            .context("renderer font plan lost its first Hangul JIS code")?;
        let first_with_batchim_jis = *jis_by_character
            .get(&first_with_batchim)
            .context("renderer font plan lost its batchim boundary")?;

        let marker_jis: [u16; 4] = PARTICLE_MARKERS
            .map(|marker| {
                jis_by_character.get(&marker).copied().with_context(|| {
                    format!("renderer font plan lacks marker U+{:04X}", marker as u32)
                })
            })
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .expect("particle marker count is fixed");
        for markers in marker_jis.windows(2) {
            ensure!(
                markers[1] == next_renderer_jis(markers[0])?,
                "renderer particle marker codes are not consecutive"
            );
        }

        let form_jis: [[u16; 2]; 4] = KoreanParticle::ALL
            .map(|particle| {
                let (without_batchim, with_batchim) = particle.forms();
                Ok([
                    *jis_by_character.get(&without_batchim).with_context(|| {
                        format!("renderer font lacks particle form {without_batchim:?}")
                    })?,
                    *jis_by_character.get(&with_batchim).with_context(|| {
                        format!("renderer font lacks particle form {with_batchim:?}")
                    })?,
                ])
            })
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .expect("particle pair count is fixed");

        ensure!(
            first_hangul_jis < first_with_batchim_jis && first_with_batchim_jis < marker_jis[0],
            "renderer particle ranges overlap or are out of order"
        );
        Ok(Self {
            first_hangul_jis,
            first_with_batchim_jis,
            marker_jis,
            form_jis,
        })
    }

    fn forms(&self, particle: KoreanParticle) -> [u16; 2] {
        self.form_jis[particle.index()]
    }
}

fn next_renderer_jis(jis: u16) -> Result<u16> {
    let [row, cell] = jis.to_be_bytes();
    if cell < JIS_LAST_CELL {
        return Ok(u16::from_be_bytes([row, cell + 1]));
    }
    ensure!(
        row < RENDERER_SHEET_BASE_ROW + RENDERER_SHEET_ROWS as u8 - 1,
        "renderer JIS range has no following cell"
    );
    Ok(u16::from_be_bytes([row + 1, JIS_FIRST_CELL]))
}

pub(crate) fn build_renderer_font_plan(characters: &BTreeSet<char>) -> Result<RendererFontPlan> {
    ensure!(
        !characters.is_empty(),
        "renderer translation demand has no Hangul syllables"
    );
    let planned_characters = plan_renderer_characters(characters)?;
    ensure!(
        planned_characters.len() <= RENDERER_GLYPH_CAPACITY,
        "renderer translation demand needs {} Hangul glyphs but capacity is {RENDERER_GLYPH_CAPACITY}",
        planned_characters.len()
    );
    ensure!(
        RENDERER_SHEET_ROWS * CELLS_PER_ROW == RENDERER_GLYPH_CAPACITY,
        "renderer glyph geometry is inconsistent"
    );

    let mut codebook = BTreeMap::new();
    let mut jis_by_character = BTreeMap::new();
    let mut sheet = vec![0_u8; RENDERER_SHEET_BYTES];
    for (slot, character) in planned_characters.iter().copied().enumerate() {
        let row = RENDERER_SHEET_BASE_ROW + u8::try_from(slot / CELLS_PER_ROW)?;
        let cell = JIS_FIRST_CELL + u8::try_from(slot % CELLS_PER_ROW)?;
        let jis_code = u16::from_be_bytes([row, cell]);
        let shift_jis_code = jis_to_shift_jis(jis_code)?;
        ensure!(
            shift_jis_code[1] != 0x7f,
            "renderer font generated an illegal Shift-JIS trail byte"
        );
        let bitmap = if is_particle_marker(character) {
            [0_u8; GLYPH_BYTES]
        } else {
            rasterize_character(character)
                .with_context(|| format!("rasterize renderer Hangul {character:?}"))?
        };
        let start = slot * GLYPH_BYTES;
        sheet[start..start + GLYPH_BYTES].copy_from_slice(&bitmap);
        ensure!(
            codebook.insert(character, shift_jis_code).is_none(),
            "renderer font contains duplicate Hangul {character:?}"
        );
        ensure!(
            jis_by_character.insert(character, jis_code).is_none(),
            "renderer font contains duplicate JIS assignment for {character:?}"
        );
    }

    let particle_selector =
        RendererParticleSelector::from_plan(&planned_characters, &jis_by_character)?;

    let font_file = sheet;
    u16::try_from(font_file.len()).context("renderer font file exceeds one DOS read")?;

    Ok(RendererFontPlan {
        codebook,
        font_file,
        font: font_report()?,
        particle_selector,
    })
}

pub(crate) fn install_renderer_font(
    payload: &mut BTreeMap<String, Vec<u8>>,
    plan: &RendererFontPlan,
) -> Result<RendererFontReport> {
    install_renderer_font_with_bios_gaiji(payload, plan, &[], MAIN_COM_SHA256)
}

pub(crate) fn install_renderer_font_with_bios_gaiji(
    payload: &mut BTreeMap<String, Vec<u8>>,
    plan: &RendererFontPlan,
    bios_gaiji: &[BiosGaijiRecord],
    expected_main_com_sha256: &str,
) -> Result<RendererFontReport> {
    ensure!(
        !payload.contains_key(RENDERER_FONT_FILE),
        "installer payload already contains {RENDERER_FONT_FILE}"
    );
    let original_main_com = payload
        .get(MAIN_COM_NAME)
        .context("installer payload is missing MAIN.COM")?;
    ensure!(
        sha256_hex(original_main_com) == expected_main_com_sha256,
        "MAIN.COM differs from the supported renderer-font loader preimage"
    );
    let (updated_main_com, main_com_loader_logical_offset) =
        patch_main_com_font_loader(original_main_com, plan.font_file.len(), bios_gaiji)?;
    let report = RendererFontReport {
        glyph_count: plan.glyph_count(),
        glyph_capacity: RENDERER_GLYPH_CAPACITY,
        sheet_size: RENDERER_SHEET_BYTES,
        font_file: RENDERER_FONT_FILE.to_owned(),
        font_file_size: plan.font_file.len(),
        font_file_sha256: sha256_hex(&plan.font_file),
        reserved_font_paragraphs: RENDERER_SHEET_PARAGRAPHS,
        font_segment_delta_from_main_overlay: format!(
            "0x{MAIN_COM_WORKING_PARAGRAPHS_FROM_OVERLAY:04X}"
        ),
        main_com_loader_logical_offset: usize::from(main_com_loader_logical_offset),
        main_draw_hook_logical_offset: format!("0x{MAIN_DRAW_HOOK_LOGICAL_OFFSET:04X}"),
        original_main_com_sha256: sha256_hex(original_main_com),
        updated_main_com_sha256: sha256_hex(&updated_main_com),
        original_main_com_size: original_main_com.len(),
        updated_main_com_size: updated_main_com.len(),
        font: plan.font.clone(),
    };
    payload.insert(MAIN_COM_NAME.to_owned(), updated_main_com);
    payload.insert(RENDERER_FONT_FILE.to_owned(), plan.font_file.clone());
    Ok(report)
}

pub(crate) fn install_main_renderer_sheet_hook(
    decoded: &mut Vec<u8>,
    particle_selector: &RendererParticleSelector,
    patch_enemy_status_particles: bool,
) -> Result<()> {
    let expected_capture = assemble_main_jis_capture_precondition()?;
    let expected_draw = assemble_main_draw_precondition()?;
    let capture_end = MAIN_JIS_CAPTURE_DECODED_OFFSET + expected_capture.len();
    ensure!(
        decoded.get(MAIN_JIS_CAPTURE_DECODED_OFFSET..capture_end)
            == Some(expected_capture.as_slice()),
        "MAIN.OVL renderer JIS-capture bytes differ from the typed precondition"
    );
    let draw_end = MAIN_DRAW_HOOK_DECODED_OFFSET + expected_draw.len();
    ensure!(
        decoded.get(MAIN_DRAW_HOOK_DECODED_OFFSET..draw_end) == Some(expected_draw.as_slice()),
        "MAIN.OVL renderer draw bytes differ from the typed sheet-hook precondition"
    );
    if patch_enemy_status_particles {
        validate_enemy_status_particle_sites(decoded)?;
    }

    let capture_hook_logical_offset = u16::try_from(
        decoded
            .len()
            .checked_add(MAIN_OVERLAY_LOAD_OFFSET)
            .context("MAIN.OVL renderer hook address overflow")?,
    )
    .context("MAIN.OVL renderer hook exceeds the code segment")?;
    let provisional_capture_hook =
        assemble_renderer_jis_capture_hook(capture_hook_logical_offset, 0, 0, particle_selector)?;
    let draw_hook_logical_offset = capture_hook_logical_offset
        .checked_add(u16::try_from(provisional_capture_hook.len())?)
        .context("MAIN.OVL renderer draw-hook address overflow")?;
    let provisional_draw_hook = assemble_renderer_draw_hook(draw_hook_logical_offset, 0)?;
    let particle_hook_logical_offset = draw_hook_logical_offset
        .checked_add(u16::try_from(provisional_draw_hook.len())?)
        .context("MAIN.OVL particle hook address overflow")?;
    let provisional_particle_hook = assemble_enemy_status_particle_selector(
        particle_hook_logical_offset,
        0,
        particle_selector,
    )?;
    let saved_jis_logical_offset = particle_hook_logical_offset
        .checked_add(u16::try_from(provisional_particle_hook.len())?)
        .context("MAIN.OVL renderer saved-JIS address overflow")?;
    let last_hangul_has_batchim_logical_offset = saved_jis_logical_offset
        .checked_add(u16::try_from(std::mem::size_of::<u16>())?)
        .context("MAIN.OVL particle state address overflow")?;
    let capture_hook = assemble_renderer_jis_capture_hook(
        capture_hook_logical_offset,
        saved_jis_logical_offset,
        last_hangul_has_batchim_logical_offset,
        particle_selector,
    )?;
    let draw_hook =
        assemble_renderer_draw_hook(draw_hook_logical_offset, saved_jis_logical_offset)?;
    let particle_hook = assemble_enemy_status_particle_selector(
        particle_hook_logical_offset,
        last_hangul_has_batchim_logical_offset,
        particle_selector,
    )?;
    ensure!(
        capture_hook.len() == provisional_capture_hook.len()
            && draw_hook.len() == provisional_draw_hook.len()
            && particle_hook.len() == provisional_particle_hook.len(),
        "MAIN.OVL renderer or particle hook changed size after state placement"
    );
    ensure!(
        usize::from(last_hangul_has_batchim_logical_offset) + std::mem::size_of::<u8>() <= 0x1_0000,
        "MAIN.OVL renderer hook exceeds the code segment"
    );
    let capture_hijack = assemble_main_jis_capture_hijack(capture_hook_logical_offset)?;
    let draw_hijack = assemble_main_draw_hijack(draw_hook_logical_offset)?;
    ensure!(
        expected_capture.len() == 4 && capture_hijack.len() == expected_capture.len(),
        "MAIN.OVL renderer JIS-capture hook is not length preserving"
    );
    ensure!(
        expected_draw.len() == 6 && draw_hijack.len() == expected_draw.len(),
        "MAIN.OVL renderer hook is not length preserving"
    );

    let mut updated = decoded.clone();
    updated[MAIN_JIS_CAPTURE_DECODED_OFFSET..capture_end].copy_from_slice(&capture_hijack);
    updated[MAIN_DRAW_HOOK_DECODED_OFFSET..draw_end].copy_from_slice(&draw_hijack);
    if patch_enemy_status_particles {
        install_enemy_status_particle_calls(&mut updated, particle_hook_logical_offset)?;
    }
    let hook_decoded_offset = updated.len();
    updated.extend_from_slice(&capture_hook);
    updated.extend_from_slice(&draw_hook);
    updated.extend_from_slice(&particle_hook);
    updated.extend_from_slice(&0_u16.to_le_bytes());
    updated.push(0);
    ensure!(
        updated.get(MAIN_JIS_CAPTURE_DECODED_OFFSET..capture_end)
            == Some(capture_hijack.as_slice()),
        "MAIN.OVL renderer JIS-capture hook readback differs"
    );
    ensure!(
        updated.get(MAIN_DRAW_HOOK_DECODED_OFFSET..draw_end) == Some(draw_hijack.as_slice()),
        "MAIN.OVL renderer sheet hook readback differs"
    );
    ensure!(
        updated.get(hook_decoded_offset..)
            == Some(
                [
                    capture_hook.as_slice(),
                    draw_hook.as_slice(),
                    particle_hook.as_slice(),
                    &[0, 0, 0],
                ]
                .concat()
                .as_slice()
            ),
        "MAIN.OVL renderer hook body readback differs"
    );
    *decoded = updated;
    Ok(())
}

pub(crate) fn encode_renderer_text(
    text: &str,
    codebook: &BTreeMap<char, [u8; 2]>,
) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let remaining = &text[cursor..];
        if let Some(marker) = remaining.strip_prefix("{josa:") {
            let close = marker
                .find('}')
                .context("unterminated runtime particle marker")?;
            let form = &marker[..close];
            let particle = KoreanParticle::parse(form)
                .with_context(|| format!("unsupported runtime particle form {form:?}"))?;
            let marker = particle.marker();
            encoded.extend_from_slice(codebook.get(&marker).with_context(|| {
                format!(
                    "renderer font lacks runtime particle marker U+{:04X}",
                    marker as u32
                )
            })?);
            cursor += "{josa:".len() + close + 1;
            continue;
        }

        let character = remaining
            .chars()
            .next()
            .expect("nonempty UTF-8 suffix has one character");
        if is_modern_hangul(character) {
            encoded.extend_from_slice(
                codebook
                    .get(&character)
                    .with_context(|| format!("unassigned Hangul glyph {character:?}"))?,
            );
            cursor += character.len_utf8();
            continue;
        }
        let value = character.to_string();
        let (bytes, _, had_errors) = SHIFT_JIS.encode(&value);
        ensure!(
            !had_errors,
            "character {character:?} is neither assigned Hangul nor Shift-JIS"
        );
        ensure!(
            !contains_renderer_sheet_code(bytes.as_ref()),
            "character {character:?} occupies the reserved renderer font rows"
        );
        encoded.extend_from_slice(bytes.as_ref());
        cursor += character.len_utf8();
    }
    Ok(encoded)
}

pub(crate) fn contains_renderer_sheet_code(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|pair| {
        (0xeb..=0xef).contains(&pair[0])
            && ((0x40..=0x7e).contains(&pair[1]) || (0x80..=0xfc).contains(&pair[1]))
    })
}

pub(crate) fn jis_to_shift_jis(jis_code: u16) -> Result<[u8; 2]> {
    let [row, cell] = jis_code.to_be_bytes();
    ensure!(
        (0x21..=0x7e).contains(&row) && (0x21..=0x7e).contains(&cell),
        "JIS code 0x{jis_code:04X} is outside the row-cell range"
    );
    let mut lead = ((row - 0x21) >> 1) + 0x81;
    if lead > 0x9f {
        lead = lead
            .checked_add(0x40)
            .context("Shift-JIS lead byte overflow")?;
    }
    let trail = if row & 1 == 1 {
        let mut value = cell
            .checked_add(0x1f)
            .context("Shift-JIS trail byte overflow")?;
        if value >= 0x7f {
            value = value
                .checked_add(1)
                .context("Shift-JIS trail byte overflow")?;
        }
        value
    } else {
        cell.checked_add(0x7e)
            .context("Shift-JIS trail byte overflow")?
    };
    Ok([lead, trail])
}

fn assemble_main_jis_capture_precondition() -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Mov {
            dest: reg8(Register8::AL),
            src: imm8(0x0b),
        })
        .emit(Instruction::OutAl {
            port: v30::PortAddress::Imm8(0x68),
        });
    assemble_at(
        &assembler,
        MAIN_JIS_CAPTURE_LOGICAL_OFFSET,
        "MAIN.OVL renderer JIS-capture precondition",
    )
}

fn assemble_main_jis_capture_hijack(capture_hook_logical_offset: u16) -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler.emit(Instruction::Call {
        target: CallTarget::Rel16(near_displacement(
            MAIN_JIS_CAPTURE_LOGICAL_OFFSET,
            capture_hook_logical_offset,
        )),
    });
    for _ in 3..4 {
        assembler.emit(Instruction::Nop);
    }
    assemble_at(
        &assembler,
        MAIN_JIS_CAPTURE_LOGICAL_OFFSET,
        "MAIN.OVL renderer JIS-capture hijack",
    )
}

fn assemble_main_draw_precondition() -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Mov {
            dest: reg16(Register16::CX),
            src: imm16(0x10),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: imm16(0x2000),
        });
    assemble_at(
        &assembler,
        MAIN_DRAW_HOOK_LOGICAL_OFFSET,
        "MAIN.OVL renderer draw precondition",
    )
}

fn assemble_main_draw_hijack(hook_logical_offset: u16) -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler.emit(Instruction::Call {
        target: CallTarget::Rel16(near_displacement(
            MAIN_DRAW_HOOK_LOGICAL_OFFSET,
            hook_logical_offset,
        )),
    });
    for _ in 3..6 {
        assembler.emit(Instruction::Nop);
    }
    assemble_at(
        &assembler,
        MAIN_DRAW_HOOK_LOGICAL_OFFSET,
        "MAIN.OVL renderer sheet hijack",
    )
}

fn validate_enemy_status_particle_sites(decoded: &[u8]) -> Result<()> {
    for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
        ensure!(
            decoded.get(site..site + expected.len()) == Some(expected.as_slice()),
            "MAIN.OVL enemy-status particle renderer changed at {site:#06x}"
        );
    }
    Ok(())
}

fn install_enemy_status_particle_calls(
    decoded: &mut [u8],
    particle_hook_logical_offset: u16,
) -> Result<()> {
    for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
        ensure!(
            decoded.get(site..site + expected.len()) == Some(expected.as_slice()),
            "MAIN.OVL enemy-status particle renderer changed before write at {site:#06x}"
        );
        let logical_offset = u16::try_from(site + MAIN_OVERLAY_LOAD_OFFSET)
            .context("MAIN.OVL enemy-status particle call address overflow")?;
        let mut assembler = Assembler::new();
        assembler.emit(Instruction::Call {
            target: CallTarget::Rel16(near_displacement(
                logical_offset,
                particle_hook_logical_offset,
            )),
        });
        let call = assemble_at(
            &assembler,
            logical_offset,
            "MAIN.OVL enemy-status particle call",
        )?;
        ensure!(
            call.len() == 3,
            "MAIN.OVL enemy-status particle call is not length preserving"
        );
        decoded[site..site + call.len()].copy_from_slice(&call);
    }
    Ok(())
}

fn assemble_renderer_jis_capture_hook(
    origin: u16,
    saved_jis_logical_offset: u16,
    last_hangul_has_batchim_logical_offset: u16,
    particle_selector: &RendererParticleSelector,
) -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Pushf)
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.marker_jis[KoreanParticle::Object.index()]),
        })
        .emit_branch(Condition::E, "select_object_particle")
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.marker_jis[KoreanParticle::Subject.index()]),
        })
        .emit_branch(Condition::E, "select_subject_particle")
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.marker_jis[KoreanParticle::Topic.index()]),
        })
        .emit_branch(Condition::E, "select_topic_particle")
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.marker_jis[KoreanParticle::With.index()]),
        })
        .emit_branch(Condition::E, "select_with_particle")
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.first_hangul_jis),
        })
        .emit_branch(Condition::B, "save_jis")
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.marker_jis[0]),
        })
        .emit_branch(Condition::B, "classify_hangul")
        .emit_jump_short("save_jis")
        .label("classify_hangul")
        .emit(Instruction::Cmp {
            a: reg16(Register16::BX),
            b: imm16(particle_selector.first_with_batchim_jis),
        })
        .emit_branch(Condition::B, "remember_without_batchim")
        .emit(Instruction::Mov {
            dest: direct_memory(
                Some(SegmentRegister::CS),
                last_hangul_has_batchim_logical_offset,
                OperandSize::Byte,
            ),
            src: imm8(1),
        })
        .emit_jump_short("save_jis")
        .label("remember_without_batchim")
        .emit(Instruction::Mov {
            dest: direct_memory(
                Some(SegmentRegister::CS),
                last_hangul_has_batchim_logical_offset,
                OperandSize::Byte,
            ),
            src: imm8(0),
        })
        .emit_jump_short("save_jis")
        .label("select_object_particle");
    emit_particle_selection(
        &mut assembler,
        "object_with_batchim",
        last_hangul_has_batchim_logical_offset,
        particle_selector.forms(KoreanParticle::Object),
    );
    assembler.label("select_subject_particle");
    emit_particle_selection(
        &mut assembler,
        "subject_with_batchim",
        last_hangul_has_batchim_logical_offset,
        particle_selector.forms(KoreanParticle::Subject),
    );
    assembler.label("select_topic_particle");
    emit_particle_selection(
        &mut assembler,
        "topic_with_batchim",
        last_hangul_has_batchim_logical_offset,
        particle_selector.forms(KoreanParticle::Topic),
    );
    assembler.label("select_with_particle");
    emit_particle_selection(
        &mut assembler,
        "with_with_batchim",
        last_hangul_has_batchim_logical_offset,
        particle_selector.forms(KoreanParticle::With),
    );
    assembler
        .label("save_jis")
        .emit(Instruction::Mov {
            dest: direct_memory(
                Some(SegmentRegister::CS),
                saved_jis_logical_offset,
                OperandSize::Word,
            ),
            src: reg16(Register16::BX),
        })
        .emit(Instruction::Popf)
        .emit(Instruction::Mov {
            dest: reg8(Register8::AL),
            src: imm8(0x0b),
        })
        .emit(Instruction::OutAl {
            port: v30::PortAddress::Imm8(0x68),
        })
        .emit(Instruction::Ret { pop: 0 });
    assemble_at(&assembler, origin, "MAIN.OVL renderer JIS-capture hook")
}

fn emit_particle_selection(
    assembler: &mut Assembler,
    with_batchim_label: &'static str,
    last_hangul_has_batchim_logical_offset: u16,
    forms: [u16; 2],
) {
    assembler
        .emit(Instruction::Cmp {
            a: direct_memory(
                Some(SegmentRegister::CS),
                last_hangul_has_batchim_logical_offset,
                OperandSize::Byte,
            ),
            b: imm8(0),
        })
        .emit_branch(Condition::Ne, with_batchim_label)
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: imm16(forms[0]),
        })
        .emit_jump_short("save_jis")
        .label(with_batchim_label)
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: imm16(forms[1]),
        })
        .emit_jump_short("save_jis");
}

fn assemble_enemy_status_particle_selector(
    origin: u16,
    last_hangul_has_batchim_logical_offset: u16,
    particle_selector: &RendererParticleSelector,
) -> Result<Vec<u8>> {
    let topic_forms = particle_selector.forms(KoreanParticle::Topic);
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Pushf)
        .emit(Instruction::Cmp {
            a: direct_memory(
                Some(SegmentRegister::CS),
                last_hangul_has_batchim_logical_offset,
                OperandSize::Byte,
            ),
            b: imm8(0),
        })
        .emit_branch(Condition::Ne, "with_batchim")
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: imm16(topic_forms[0]),
        })
        .emit_jump_short("done")
        .label("with_batchim")
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: imm16(topic_forms[1]),
        })
        .label("done")
        .emit(Instruction::Popf)
        .emit(Instruction::Ret { pop: 0 });
    assemble_at(
        &assembler,
        origin,
        "MAIN.OVL enemy-status particle selector",
    )
}

fn assemble_renderer_draw_hook(origin: u16, saved_jis_logical_offset: u16) -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Push {
            src: reg16(Register16::BP),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::BP),
            src: reg16(Register16::SP),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: direct_memory(
                Some(SegmentRegister::CS),
                saved_jis_logical_offset,
                OperandSize::Word,
            ),
        })
        .emit(Instruction::Cmp {
            a: reg8(Register8::BH),
            b: imm8(RENDERER_SHEET_BASE_ROW),
        })
        .emit_branch(Condition::B, "original_font")
        .emit(Instruction::Cmp {
            a: reg8(Register8::BH),
            b: imm8(RENDERER_SHEET_BASE_ROW + RENDERER_SHEET_ROWS as u8 - 1),
        })
        .emit_branch(Condition::A, "original_font")
        .emit(Instruction::Cmp {
            a: reg8(Register8::BL),
            b: imm8(JIS_FIRST_CELL),
        })
        .emit_branch(Condition::B, "original_font")
        .emit(Instruction::Cmp {
            a: reg8(Register8::BL),
            b: imm8(JIS_LAST_CELL),
        })
        .emit_branch(Condition::A, "original_font")
        .emit(Instruction::Mov {
            dest: based_memory(
                Some(SegmentRegister::SS),
                EffectiveAddressBase::Bp,
                2,
                OperandSize::Word,
            ),
            src: imm16(MAIN_DRAW_FINISH_LOGICAL_OFFSET),
        })
        .emit(Instruction::Push {
            src: segment(SegmentRegister::ES),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: segment(SegmentRegister::CS),
        })
        .emit(Instruction::Add {
            dest: reg16(Register16::AX),
            src: imm16(MAIN_COM_WORKING_PARAGRAPHS_FROM_OVERLAY),
        })
        .emit(Instruction::Mov {
            dest: segment(SegmentRegister::ES),
            src: reg16(Register16::AX),
        })
        .emit(Instruction::Mov {
            dest: reg8(Register8::AL),
            src: reg8(Register8::BH),
        })
        .emit(Instruction::Sub {
            dest: reg8(Register8::AL),
            src: imm8(RENDERER_SHEET_BASE_ROW),
        })
        .emit(Instruction::Mov {
            dest: reg8(Register8::AH),
            src: imm8(CELLS_PER_ROW as u8),
        })
        .emit(Instruction::Mul {
            src: reg8(Register8::AH),
        })
        .emit(Instruction::Mov {
            dest: reg8(Register8::DL),
            src: reg8(Register8::BL),
        })
        .emit(Instruction::Sub {
            dest: reg8(Register8::DL),
            src: imm8(JIS_FIRST_CELL),
        })
        .emit(Instruction::Xor {
            dest: reg8(Register8::DH),
            src: reg8(Register8::DH),
        })
        .emit(Instruction::Add {
            dest: reg16(Register16::AX),
            src: reg16(Register16::DX),
        });
    for _ in 0..5 {
        assembler.emit(Instruction::Shl {
            dest: reg16(Register16::AX),
            count: ShiftCount::One,
        });
    }
    assembler
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: reg16(Register16::AX),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::CX),
            src: imm16(16),
        })
        .label("hangul_row")
        .emit(Instruction::Mov {
            dest: reg8(Register8::DL),
            src: based_memory(
                Some(SegmentRegister::ES),
                EffectiveAddressBase::Bx,
                0,
                OperandSize::Byte,
            ),
        })
        .emit(Instruction::Mov {
            dest: reg8(Register8::DH),
            src: based_memory(
                Some(SegmentRegister::ES),
                EffectiveAddressBase::Bx,
                1,
                OperandSize::Byte,
            ),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: reg16(Register16::DX),
        })
        .emit(Instruction::Shl {
            dest: reg8(Register8::AH),
            count: ShiftCount::One,
        })
        .emit(Instruction::Rcl {
            dest: reg8(Register8::AL),
            count: ShiftCount::One,
        })
        .emit(Instruction::Or {
            dest: reg16(Register16::AX),
            src: reg16(Register16::DX),
        })
        .emit(Instruction::Xor {
            dest: reg16(Register16::AX),
            src: reg16(Register16::SI),
        })
        .emit(Instruction::Mov {
            dest: based_memory(None, EffectiveAddressBase::Di, 0, OperandSize::Word),
            src: reg16(Register16::AX),
        })
        .emit(Instruction::Add {
            dest: reg16(Register16::DI),
            src: imm16(0x50),
        })
        .emit(Instruction::Add {
            dest: reg16(Register16::BX),
            src: imm16(2),
        })
        .emit_loop(LoopCondition::Always, "hangul_row")
        .emit(Instruction::Pop {
            dest: segment(SegmentRegister::ES),
        })
        .emit(Instruction::Pop {
            dest: reg16(Register16::BP),
        })
        .emit(Instruction::Ret { pop: 0 })
        .label("original_font")
        .emit(Instruction::Pop {
            dest: reg16(Register16::BP),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::CX),
            src: imm16(0x10),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: imm16(0x2000),
        })
        .emit(Instruction::Ret { pop: 0 });
    assemble_at(&assembler, origin, "MAIN.OVL renderer font hook")
}

fn patch_main_com_font_loader(
    main_com: &[u8],
    font_file_size: usize,
    bios_gaiji: &[BiosGaijiRecord],
) -> Result<(Vec<u8>, u16)> {
    let font_file_size =
        u16::try_from(font_file_size).context("renderer font file exceeds one DOS read")?;
    ensure!(
        usize::from(font_file_size) == RENDERER_SHEET_BYTES,
        "renderer font file differs from the reserved sheet size"
    );
    let original_entry = assemble_main_com_entry_prologue()?;
    ensure!(
        main_com.starts_with(&original_entry),
        "MAIN.COM entry bytes differ from the typed font-loader precondition"
    );
    let mut decoded =
        decode_main_com_image(main_com).context("decode MAIN.COM font-loader body")?;
    reserve_main_com_font_tail(&mut decoded)?;
    let repacked_body = encode_compile_lz(&decoded);
    let roundtrip = decode_exact_compile_lz(&repacked_body)
        .context("rebuilt MAIN.COM body is not one exact Compile-LZ stream")?;
    ensure!(
        roundtrip.streams == [decoded],
        "rebuilt MAIN.COM body does not decode to the reserved-memory plan"
    );

    let mut repacked = main_com[..MAIN_COM_PACKED_STREAM_OFFSET].to_vec();
    repacked.extend_from_slice(&repacked_body);
    let stub_file_offset = u16::try_from(repacked.len())
        .context("MAIN.COM is too large for an appended near loader stub")?;
    let stub_logical_offset = MAIN_COM_ENTRY_LOGICAL_OFFSET
        .checked_add(stub_file_offset)
        .context("MAIN.COM renderer-font loader address overflow")?;
    let placeholder_addresses = vec![0_u16; bios_gaiji.len()];
    let provisional = assemble_main_com_font_loader(
        stub_logical_offset,
        0,
        font_file_size,
        &placeholder_addresses,
        bios_gaiji,
    )?;
    let filename_logical_offset = stub_logical_offset
        .checked_add(u16::try_from(provisional.len())?)
        .context("MAIN.COM renderer-font filename address overflow")?;
    let gaiji_record_base = filename_logical_offset
        .checked_add(u16::try_from(RENDERER_FONT_FILE.len() + 1)?)
        .context("MAIN.COM BIOS gaiji record address overflow")?;
    let gaiji_record_size = BIOS_GAIJI_RECORD_PREFIX.len() + GLYPH_BYTES;
    let gaiji_record_addresses = (0..bios_gaiji.len())
        .map(|index| {
            let byte_offset = index
                .checked_mul(gaiji_record_size)
                .context("MAIN.COM BIOS gaiji record offset overflow")?;
            gaiji_record_base
                .checked_add(u16::try_from(byte_offset)?)
                .context("MAIN.COM BIOS gaiji records exceed the load segment")
        })
        .collect::<Result<Vec<_>>>()?;
    let loader = assemble_main_com_font_loader(
        stub_logical_offset,
        filename_logical_offset,
        font_file_size,
        &gaiji_record_addresses,
        bios_gaiji,
    )?;
    ensure!(
        loader.len() == provisional.len(),
        "MAIN.COM renderer-font loader changed size after filename placement"
    );
    let hijack =
        assemble_main_com_entry_hijack(u16::try_from(original_entry.len())?, stub_logical_offset)?;
    ensure!(
        hijack.len() == original_entry.len(),
        "MAIN.COM renderer-font entry hijack is not length preserving"
    );

    let mut updated = repacked.clone();
    updated[..hijack.len()].copy_from_slice(&hijack);
    updated.extend_from_slice(&loader);
    updated.extend_from_slice(RENDERER_FONT_FILE.as_bytes());
    updated.push(0);
    for glyph in bios_gaiji {
        updated.extend_from_slice(&BIOS_GAIJI_RECORD_PREFIX);
        updated.extend_from_slice(&glyph.bitmap);
    }
    ensure!(
        updated.get(repacked.len()..repacked.len() + loader.len()) == Some(loader.as_slice()),
        "MAIN.COM renderer-font loader readback differs"
    );
    let filename_offset = repacked.len() + loader.len();
    let gaiji_records_offset = filename_offset + RENDERER_FONT_FILE.len() + 1;
    ensure!(
        updated.get(filename_offset..gaiji_records_offset) == Some(b"KFONT.BIN\0"),
        "MAIN.COM renderer-font filename readback differs"
    );
    let mut cursor = gaiji_records_offset;
    for glyph in bios_gaiji {
        let end = cursor + gaiji_record_size;
        let record = updated
            .get(cursor..end)
            .context("MAIN.COM BIOS gaiji record readback lies outside output")?;
        ensure!(
            record[..BIOS_GAIJI_RECORD_PREFIX.len()] == BIOS_GAIJI_RECORD_PREFIX
                && record[BIOS_GAIJI_RECORD_PREFIX.len()..] == glyph.bitmap,
            "MAIN.COM BIOS gaiji record readback differs for JIS {:04X}",
            glyph.jis_code
        );
        cursor = end;
    }
    ensure!(
        cursor == updated.len(),
        "MAIN.COM renderer-font loader has an unknown tail"
    );
    ensure!(
        updated.get(hijack.len()..MAIN_COM_PACKED_STREAM_OFFSET)
            == main_com.get(hijack.len()..MAIN_COM_PACKED_STREAM_OFFSET),
        "MAIN.COM renderer-font loader changed the self-unpacker outside its entry"
    );
    Ok((updated, stub_logical_offset))
}

fn reserve_main_com_font_tail(decoded: &mut [u8]) -> Result<()> {
    let expected = assemble_main_com_memory_resize(MAIN_COM_ORIGINAL_WORKING_PARAGRAPHS)?;
    let updated = assemble_main_com_memory_resize(MAIN_COM_UPDATED_WORKING_PARAGRAPHS)?;
    ensure!(
        expected.len() == updated.len(),
        "MAIN.COM memory reservation is not length preserving"
    );
    let end = MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET
        .checked_add(expected.len())
        .context("MAIN.COM memory reservation offset overflow")?;
    ensure!(
        decoded.get(MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET..end) == Some(expected.as_slice()),
        "MAIN.COM memory resize differs from the typed font-tail precondition"
    );
    decoded[MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET..end].copy_from_slice(&updated);
    ensure!(
        decoded.get(MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET..end) == Some(updated.as_slice()),
        "MAIN.COM font-tail reservation readback differs"
    );
    Ok(())
}

fn assemble_main_com_memory_resize(working_paragraphs: u16) -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Add {
            dest: reg16(Register16::BX),
            src: imm16(working_paragraphs),
        })
        .emit(Instruction::Mov {
            dest: reg8(Register8::AH),
            src: imm8(0x4a),
        })
        .emit(Instruction::Int { vector: 0x21 });
    assemble_at(
        &assembler,
        MAIN_COM_MEMORY_RESIZE_LOGICAL_OFFSET,
        "MAIN.COM memory resize",
    )
}

fn assemble_main_com_entry_prologue() -> Result<Vec<u8>> {
    let mut assembler = Assembler::new();
    emit_main_com_entry_prologue(&mut assembler);
    assemble_at(
        &assembler,
        MAIN_COM_ENTRY_LOGICAL_OFFSET,
        "MAIN.COM entry precondition",
    )
}

fn emit_main_com_entry_prologue(assembler: &mut Assembler) {
    assembler
        .emit(Instruction::Cld)
        .emit(Instruction::Pusha)
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: segment(SegmentRegister::CS),
        })
        .emit(Instruction::Mov {
            dest: segment(SegmentRegister::DS),
            src: reg16(Register16::AX),
        });
}

fn assemble_main_com_entry_hijack(entry_len: u16, stub: u16) -> Result<Vec<u8>> {
    ensure!(entry_len >= 3, "MAIN.COM entry is shorter than a near jump");
    let mut assembler = Assembler::new();
    assembler.emit(Instruction::Jmp {
        target: JmpTarget::Rel16(near_displacement(MAIN_COM_ENTRY_LOGICAL_OFFSET, stub)),
    });
    for _ in 3..entry_len {
        assembler.emit(Instruction::Nop);
    }
    assemble_at(
        &assembler,
        MAIN_COM_ENTRY_LOGICAL_OFFSET,
        "MAIN.COM renderer-font entry hijack",
    )
}

fn assemble_main_com_font_loader(
    origin: u16,
    filename: u16,
    font_file_size: u16,
    gaiji_record_addresses: &[u16],
    bios_gaiji: &[BiosGaijiRecord],
) -> Result<Vec<u8>> {
    ensure!(
        gaiji_record_addresses.len() == bios_gaiji.len(),
        "MAIN.COM BIOS gaiji loader has mismatched records"
    );
    let mut assembler = Assembler::new();
    assembler
        .emit(Instruction::Push {
            src: segment(SegmentRegister::DS),
        })
        .emit(Instruction::Push {
            src: segment(SegmentRegister::CS),
        })
        .emit(Instruction::Pop {
            dest: segment(SegmentRegister::DS),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::DX),
            src: imm16(filename),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: imm16(0x3d00),
        })
        .emit(Instruction::Int { vector: 0x21 })
        .emit_branch(Condition::B, "font_error")
        .emit(Instruction::Mov {
            dest: reg16(Register16::BX),
            src: reg16(Register16::AX),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: segment(SegmentRegister::CS),
        })
        .emit(Instruction::Add {
            dest: reg16(Register16::AX),
            src: imm16(MAIN_COM_FONT_SEGMENT_PARAGRAPHS),
        })
        .emit(Instruction::Mov {
            dest: segment(SegmentRegister::DS),
            src: reg16(Register16::AX),
        })
        .emit(Instruction::Xor {
            dest: reg16(Register16::DX),
            src: reg16(Register16::DX),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::CX),
            src: imm16(font_file_size),
        })
        .emit(Instruction::Mov {
            dest: reg8(Register8::AH),
            src: imm8(0x3f),
        })
        .emit(Instruction::Int { vector: 0x21 })
        .emit_branch(Condition::B, "close_then_error")
        .emit(Instruction::Cmp {
            a: reg16(Register16::AX),
            b: imm16(font_file_size),
        })
        .emit_branch(Condition::Ne, "close_then_error")
        .emit(Instruction::Mov {
            dest: reg8(Register8::AH),
            src: imm8(0x3e),
        })
        .emit(Instruction::Int { vector: 0x21 })
        .emit(Instruction::Pop {
            dest: segment(SegmentRegister::DS),
        });
    if bios_gaiji.is_empty() {
        emit_main_com_loader_resume(&mut assembler);
        emit_main_com_font_error_handlers(&mut assembler);
    } else {
        assembler.emit_jump_near("font_ready");
        emit_main_com_font_error_handlers(&mut assembler);
        assembler.label("font_ready");
        assembler
            .emit(Instruction::Pushf)
            .emit(Instruction::Pusha)
            .emit(Instruction::Mov {
                dest: reg16(Register16::BX),
                src: segment(SegmentRegister::CS),
            });
        for (&record_address, glyph) in gaiji_record_addresses.iter().zip(bios_gaiji) {
            assembler
                .emit(Instruction::Mov {
                    dest: reg16(Register16::CX),
                    src: imm16(record_address),
                })
                .emit(Instruction::Mov {
                    dest: reg16(Register16::DX),
                    src: imm16(glyph.jis_code),
                })
                .emit(Instruction::Mov {
                    dest: reg8(Register8::AH),
                    src: imm8(0x1a),
                })
                .emit(Instruction::Int { vector: 0x18 });
        }
        assembler.emit(Instruction::Popa).emit(Instruction::Popf);
        emit_main_com_loader_resume(&mut assembler);
    }
    assemble_at(&assembler, origin, "MAIN.COM renderer-font loader")
}

fn emit_main_com_loader_resume(assembler: &mut Assembler) {
    emit_main_com_entry_prologue(assembler);
    assembler
        .emit(Instruction::Push {
            src: imm16(MAIN_COM_ENTRY_LOGICAL_OFFSET + 6),
        })
        .emit(Instruction::Ret { pop: 0 });
}

fn emit_main_com_font_error_handlers(assembler: &mut Assembler) {
    assembler
        .label("close_then_error")
        .emit(Instruction::Mov {
            dest: reg8(Register8::AH),
            src: imm8(0x3e),
        })
        .emit(Instruction::Int { vector: 0x21 })
        .label("font_error")
        .emit(Instruction::Pop {
            dest: segment(SegmentRegister::DS),
        })
        .emit(Instruction::Mov {
            dest: reg16(Register16::AX),
            src: imm16(0x4c01),
        })
        .emit(Instruction::Int { vector: 0x21 })
        .emit(Instruction::Hlt);
}

fn assemble_at(assembler: &Assembler, origin: u16, purpose: &str) -> Result<Vec<u8>> {
    assembler
        .assemble(CodeLocation {
            seg: 0,
            off: origin,
        })
        .with_context(|| format!("assemble typed V30 {purpose} at 0x{origin:04X}"))
        .map(|program| program.bytes().to_vec())
}

fn based_memory(
    segment: Option<SegmentRegister>,
    base: EffectiveAddressBase,
    displacement: i16,
    size: OperandSize,
) -> Operand {
    Operand::Mem(
        EffectiveAddress::new(
            segment,
            base,
            EffectiveAddressDisplacement::Signed(displacement),
            size,
        )
        .expect("renderer-font V30 address is representable"),
    )
}

fn direct_memory(segment: Option<SegmentRegister>, address: u16, size: OperandSize) -> Operand {
    Operand::Mem(
        EffectiveAddress::new(
            segment,
            EffectiveAddressBase::Direct,
            EffectiveAddressDisplacement::Absolute(address),
            size,
        )
        .expect("renderer-font V30 direct address is representable"),
    )
}

const fn reg8(register: Register8) -> Operand {
    Operand::Reg8(register)
}

const fn reg16(register: Register16) -> Operand {
    Operand::Reg16(register)
}

const fn segment(register: SegmentRegister) -> Operand {
    Operand::Sreg(register)
}

const fn imm8(value: u8) -> Operand {
    Operand::Imm8(value)
}

const fn imm16(value: u16) -> Operand {
    Operand::Imm16(value)
}

const fn near_displacement(origin: u16, target: u16) -> i16 {
    target.wrapping_sub(origin.wrapping_add(3)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_codes_cover_ten_rows_without_an_illegal_trail_byte() {
        assert_eq!(jis_to_shift_jis(0x7521).unwrap(), [0xeb, 0x40]);
        assert_eq!(jis_to_shift_jis(0x755f).unwrap(), [0xeb, 0x7e]);
        assert_eq!(jis_to_shift_jis(0x7560).unwrap(), [0xeb, 0x80]);
        assert_eq!(jis_to_shift_jis(0x7e7e).unwrap(), [0xef, 0xfc]);
    }

    #[test]
    fn renderer_sheet_code_detection_requires_a_valid_two_byte_code() {
        assert!(contains_renderer_sheet_code(&[0xeb, 0x40]));
        assert!(contains_renderer_sheet_code(&[b'A', 0xef, 0xfc, b'B']));
        assert!(!contains_renderer_sheet_code(&[0xea, 0xfc]));
        assert!(!contains_renderer_sheet_code(&[0xeb, 0x7f]));
        assert!(!contains_renderer_sheet_code(&[0xeb]));
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn renderer_font_groups_hangul_for_runtime_particle_selection() {
        let characters = ['가', '각', '나', '난']
            .into_iter()
            .collect::<BTreeSet<_>>();
        let plan = build_renderer_font_plan(&characters).unwrap();

        for character in characters
            .iter()
            .chain(crate::josa::PARTICLE_FORMS.iter())
            .chain(crate::josa::PARTICLE_MARKERS.iter())
        {
            assert!(plan.codebook().contains_key(character));
        }
        assert!(
            plan.particle_selector.first_hangul_jis < plan.particle_selector.first_with_batchim_jis
        );
        assert!(
            plan.particle_selector.first_with_batchim_jis < plan.particle_selector.marker_jis[0]
        );
        assert_eq!(plan.font_file.len(), RENDERER_SHEET_BYTES);
        assert_eq!(RENDERER_SHEET_BYTES, 30_080);
        assert_eq!(RENDERER_SHEET_PARAGRAPHS, 1_880);
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn renderer_text_encodes_a_particle_marker_as_one_reserved_glyph() {
        let characters = ['가', '각'].into_iter().collect::<BTreeSet<_>>();
        let plan = build_renderer_font_plan(&characters).unwrap();

        let encoded = encode_renderer_text("{josa:은}", plan.codebook()).unwrap();

        assert_eq!(
            encoded,
            plan.codebook()[&KoreanParticle::Topic.marker()].as_slice()
        );
        assert!(encode_renderer_text("{josa:으로}", plan.codebook()).is_err());
    }

    #[test]
    fn main_draw_hijack_is_a_length_preserving_near_call() {
        let expected = assemble_main_draw_precondition().unwrap();
        let hijack = assemble_main_draw_hijack(0xa000).unwrap();

        assert_eq!(expected, [0xb9, 0x10, 0x00, 0xbb, 0x00, 0x20]);
        assert_eq!(hijack, [0xe8, 0x67, 0x4f, 0x90, 0x90, 0x90]);
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn renderer_hook_preserves_jis_code_across_work_buffer_setup() {
        let plan = build_renderer_font_plan(&['가', '각'].into_iter().collect()).unwrap();
        let capture_expected = assemble_main_jis_capture_precondition().unwrap();
        let capture_hijack = assemble_main_jis_capture_hijack(0xa000).unwrap();
        let capture_hook =
            assemble_renderer_jis_capture_hook(0xa000, 0xa100, 0xa102, plan.particle_selector())
                .unwrap();
        let draw_hook = assemble_renderer_draw_hook(0xa020, 0xa100).unwrap();

        assert_eq!(capture_expected, [0xb0, 0x0b, 0xe6, 0x68]);
        assert_eq!(capture_hijack, [0xe8, 0xd8, 0x4f, 0x90]);
        assert!(
            capture_hook
                .windows(5)
                .any(|bytes| bytes == [0x2e, 0x89, 0x1e, 0x00, 0xa1])
        );
        assert!(
            draw_hook
                .windows(5)
                .any(|bytes| bytes == [0x2e, 0x8b, 0x1e, 0x00, 0xa1])
        );
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn renderer_hook_is_appended_inside_the_main_overlay() {
        let plan = build_renderer_font_plan(&['가', '각'].into_iter().collect()).unwrap();
        let mut decoded = vec![0_u8; 0x6000];
        let expected_capture = assemble_main_jis_capture_precondition().unwrap();
        decoded[MAIN_JIS_CAPTURE_DECODED_OFFSET
            ..MAIN_JIS_CAPTURE_DECODED_OFFSET + expected_capture.len()]
            .copy_from_slice(&expected_capture);
        let expected_draw = assemble_main_draw_precondition().unwrap();
        decoded[MAIN_DRAW_HOOK_DECODED_OFFSET..MAIN_DRAW_HOOK_DECODED_OFFSET + expected_draw.len()]
            .copy_from_slice(&expected_draw);
        let original_len = decoded.len();

        install_main_renderer_sheet_hook(&mut decoded, plan.particle_selector(), false).unwrap();

        assert!(decoded.len() > original_len);
        assert_eq!(decoded[MAIN_JIS_CAPTURE_DECODED_OFFSET], 0xe8);
        assert_eq!(decoded[MAIN_DRAW_HOOK_DECODED_OFFSET], 0xe8);
        assert_eq!(
            &decoded[MAIN_DRAW_HOOK_DECODED_OFFSET + 3..MAIN_DRAW_HOOK_DECODED_OFFSET + 6],
            [0x90, 0x90, 0x90]
        );
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn enemy_status_particle_calls_replace_only_the_direct_japanese_glyph_load() {
        let plan = build_renderer_font_plan(&['가', '각'].into_iter().collect()).unwrap();
        let mut decoded = vec![0_u8; 0x6000];
        let expected_capture = assemble_main_jis_capture_precondition().unwrap();
        decoded[MAIN_JIS_CAPTURE_DECODED_OFFSET
            ..MAIN_JIS_CAPTURE_DECODED_OFFSET + expected_capture.len()]
            .copy_from_slice(&expected_capture);
        let expected_draw = assemble_main_draw_precondition().unwrap();
        decoded[MAIN_DRAW_HOOK_DECODED_OFFSET..MAIN_DRAW_HOOK_DECODED_OFFSET + expected_draw.len()]
            .copy_from_slice(&expected_draw);
        for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
            decoded[site..site + expected.len()].copy_from_slice(&expected);
        }

        install_main_renderer_sheet_hook(&mut decoded, plan.particle_selector(), true).unwrap();

        for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
            assert_eq!(decoded[site], 0xe8);
            assert_eq!(&decoded[site + 3..site + 6], &expected[3..]);
        }
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn enemy_status_particle_patch_rejects_an_unknown_site_without_writing() {
        let plan = build_renderer_font_plan(&['가', '각'].into_iter().collect()).unwrap();
        let mut decoded = vec![0_u8; 0x6000];
        let expected_capture = assemble_main_jis_capture_precondition().unwrap();
        decoded[MAIN_JIS_CAPTURE_DECODED_OFFSET
            ..MAIN_JIS_CAPTURE_DECODED_OFFSET + expected_capture.len()]
            .copy_from_slice(&expected_capture);
        let expected_draw = assemble_main_draw_precondition().unwrap();
        decoded[MAIN_DRAW_HOOK_DECODED_OFFSET..MAIN_DRAW_HOOK_DECODED_OFFSET + expected_draw.len()]
            .copy_from_slice(&expected_draw);
        for (site, expected) in ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES {
            decoded[site..site + expected.len()].copy_from_slice(&expected);
        }
        decoded[ENEMY_STATUS_TOPIC_PARTICLE_RENDER_SITES[1].0] = 0xcc;
        let original = decoded.clone();

        let error = install_main_renderer_sheet_hook(&mut decoded, plan.particle_selector(), true)
            .unwrap_err();

        assert!(error.to_string().contains("particle renderer changed"));
        assert_eq!(decoded, original);
    }

    #[test]
    fn main_com_memory_resize_reserves_the_renderer_sheet_tail() {
        let original =
            assemble_main_com_memory_resize(MAIN_COM_ORIGINAL_WORKING_PARAGRAPHS).unwrap();
        assert_eq!(original, [0x81, 0xc3, 0x00, 0x6a, 0xb4, 0x4a, 0xcd, 0x21]);
        let mut decoded = vec![0_u8; MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET + original.len()];
        decoded[MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET..].copy_from_slice(&original);

        reserve_main_com_font_tail(&mut decoded).unwrap();

        assert_eq!(
            &decoded[MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET + 2
                ..MAIN_COM_MEMORY_RESIZE_DECODED_OFFSET + 4],
            &MAIN_COM_UPDATED_WORKING_PARAGRAPHS.to_le_bytes()
        );
        assert_eq!(
            MAIN_COM_OVERLAY_SEGMENT_PARAGRAPHS + MAIN_COM_UPDATED_WORKING_PARAGRAPHS,
            0x7427
        );
    }

    #[test]
    fn main_com_loader_reads_the_sheet_into_the_owned_tail() {
        let loader = assemble_main_com_font_loader(0x2000, 0x2100, 0x7580, &[], &[]).unwrap();

        assert!(loader.windows(6).any(|bytes| {
            bytes
                == [
                    0x8c,
                    0xc8,
                    0x05,
                    (MAIN_COM_FONT_SEGMENT_PARAGRAPHS & 0xff) as u8,
                    (MAIN_COM_FONT_SEGMENT_PARAGRAPHS >> 8) as u8,
                    0x8e,
                ]
        }));
        assert!(loader.windows(3).any(|bytes| bytes == [0xb9, 0x80, 0x75]));
    }

    #[test]
    fn font_loader_keeps_error_branches_in_range_with_many_bios_gaiji() {
        let glyphs = (0..32)
            .map(|_| BiosGaijiRecord {
                jis_code: 0x7621,
                bitmap: [0; GLYPH_BYTES],
            })
            .collect::<Vec<_>>();
        let record_addresses = (0..glyphs.len())
            .map(|index| 0x3000 + u16::try_from(index * (2 + GLYPH_BYTES)).unwrap())
            .collect::<Vec<_>>();

        let loader =
            assemble_main_com_font_loader(0x2000, 0x2100, 0x7580, &record_addresses, &glyphs)
                .unwrap();

        assert!(loader.windows(2).any(|bytes| bytes == [0xcd, 0x18]));
    }
}
