use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::compile_lz::{decode_exact_compile_lz, encode_compile_lz};
use crate::expected_write::{FixedRangeExpectedWrite, apply_fixed_range_expected_writes};
use crate::font::{
    FontReport, GLYPH_BYTES, GLYPH_HEIGHT, GLYPH_WIDTH, font_report, rasterize_character,
};
use crate::game_data::decode_main_com_image;
use crate::lha_sfx::extract_lha_sfx;
use crate::local_input;
use crate::source_disk::{load_verified_source, sha256_hex};

const SCREEN_WIDTH: usize = 640;
const SCREEN_HEIGHT: usize = 400;
const PLANE_BYTES: usize = SCREEN_WIDTH * SCREEN_HEIGHT / 8;
const TILE_WIDTH: usize = 16;
const TILE_HEIGHT: usize = 16;
const TILE_PLANE_BYTES: usize = TILE_WIDTH * TILE_HEIGHT / 8;
const TILE_BYTES: usize = TILE_PLANE_BYTES * 4;
const EVENT_DECODED_BYTES: usize = 0x2c00;
const EVENT_TILE_END: usize = 0x1100;
const EVENT_SMALL_END: usize = 0x1600;
const EVENT_MEDIUM_END: usize = 0x1e00;
const BATTLE_HEADER_BYTES: usize = 0x0480;
const BATTLE_MAP_ENTRY_COUNT: usize = 14;
const DUNGEON_HEADER_BYTES: usize = 0x0c00;
const DUNGEON_MAP_ENTRY_COUNT: usize = 124;
const FACE_TILE_BANK_OFFSET: usize = 0x0170;
const FACE_MAP_OFFSET: usize = 0x0080;
const FACE_TILE_REGION_END: usize = 0x42f0;
const FACE_SPRITE_DESCRIPTOR_OFFSET: usize = 0x0008;
const FACE_SPRITE_DESCRIPTOR_COUNT: usize = 20;
const CATALOG_SCHEMA: &str = "pc98_madou_docho.graphic_text_catalog";
const UNTRANSLATED_STATUS: &str = "untranslated";
const EXCLUDED_STATUS: &str = "excluded";
const TITLE_ART_SCHEMA: &str = "pc98_madou_docho.title_art";
const TITLE_ART_STATUS: &str = "needs_human_review";
const TITLE_ART_RELEASE_STATUS: &str = "distribution_eligible";
const TITLE_ART_PATH: &str = "graphics/title-art.json";
const TITLE_COMPOSITION_PATH: &str = "graphics/title-composition.png";

struct GraphicProfile {
    name: &'static str,
    packed_sha256: &'static str,
    stream_sha256: &'static [&'static str],
    stream_sizes: &'static [usize],
    layout: GraphicLayout,
    consumer_program: &'static str,
    consumer_filename_decoded_offset: usize,
}

#[derive(Clone, Copy)]
enum GraphicLayout {
    PlanarBrgi,
    ColumnMajorBrgi,
    MixedBrgiSpriteAtlas,
    IndexedPlaneMajorBrgiTileAtlas {
        tile_bank_offset: usize,
        map_entry_count: usize,
    },
    MappedTilesAndDescriptorSprites,
    Monochrome,
}

const GRAPHIC_PROFILES: [GraphicProfile; 7] = [
    GraphicProfile {
        name: "TITLE.CNS",
        packed_sha256: "51c157647e61a689e860a32de0e4381af0f5d045fe9b8d3018057c7b3f4ccaba",
        stream_sha256: &[
            "7b07b39581ba55c24b0e1757be4535a0e5021cdf36fbfc41ef48dc5571020b70",
            "8f7321aa6c0c91c9b25e0b31e3d0ff91a3df03432692bbcb29a8a82793cdc6d1",
            "8559a0313a597d8b16db46a646f52fa5cb6b1d03d6d2bce88241d3ee5ae92d10",
            "472fe707c85412c3504d608e6ec51d4f37d60ad0b1fc8318a61e1374e89ec309",
        ],
        stream_sizes: &[PLANE_BYTES; 4],
        layout: GraphicLayout::PlanarBrgi,
        consumer_program: "OPEN.OVL",
        consumer_filename_decoded_offset: 304,
    },
    GraphicProfile {
        name: "ED7.CNS",
        packed_sha256: "1b985738271f8f7df3617c7a7b7f46d855392743aea2f6f97ea033bc2e4c815d",
        stream_sha256: &["574b41103933a366297b4174e84f2a70d4d220d55091ccfee88c90a3f209ff29"],
        stream_sizes: &[PLANE_BYTES],
        layout: GraphicLayout::Monochrome,
        consumer_program: "END.OVL",
        consumer_filename_decoded_offset: 299,
    },
    GraphicProfile {
        name: "FRAME.CS",
        packed_sha256: "ae24e1734fe5baa79a0a2932db84a7afd89057d98b4480447a8717231fa8a686",
        stream_sha256: &["bf503eb8c7e7ab87efe77bed1bc75d37166d407bebfd62e41057a6ec628521e0"],
        stream_sizes: &[4 * PLANE_BYTES],
        layout: GraphicLayout::ColumnMajorBrgi,
        consumer_program: "MAIN.OVL",
        consumer_filename_decoded_offset: 0x62e4,
    },
    GraphicProfile {
        name: "EVENT.CNS",
        packed_sha256: "dbd13840481d4b5012c7592944baace1792b1f639380bc50af3597dd2cd968e2",
        stream_sha256: &["400f694292e7e9c24a1423c5366e94e65c078f97299d4433cc7f2aa6288483bf"],
        stream_sizes: &[EVENT_DECODED_BYTES],
        layout: GraphicLayout::MixedBrgiSpriteAtlas,
        consumer_program: "MAIN.OVL",
        consumer_filename_decoded_offset: 0x62ef,
    },
    GraphicProfile {
        name: "BATTLE.CNS",
        packed_sha256: "f264a7eabfe4f11591ae7e8e81072de8e442820240273979757de9f657dce50e",
        stream_sha256: &["90f1310bacc2970d962e6ed0da2d2e16f59c19b47c904b5968ef7bb387a2f872"],
        stream_sizes: &[42_880],
        layout: GraphicLayout::IndexedPlaneMajorBrgiTileAtlas {
            tile_bank_offset: BATTLE_HEADER_BYTES,
            map_entry_count: BATTLE_MAP_ENTRY_COUNT,
        },
        consumer_program: "MAIN.OVL",
        consumer_filename_decoded_offset: 0x6306,
    },
    GraphicProfile {
        name: "DUNGEON.CNS",
        packed_sha256: "ac6b28474ef399f3fc1514b60c591e326cfe717348fec6365423690cf01a9806",
        stream_sha256: &["a2498fcf6150f042429ff5ab91023d8def48ba44459ea84aa776ed733d127cc6"],
        stream_sizes: &[109_696],
        layout: GraphicLayout::IndexedPlaneMajorBrgiTileAtlas {
            tile_bank_offset: DUNGEON_HEADER_BYTES,
            map_entry_count: DUNGEON_MAP_ENTRY_COUNT,
        },
        consumer_program: "MAIN.OVL",
        consumer_filename_decoded_offset: 0x631e,
    },
    GraphicProfile {
        name: "FACE.CNS",
        packed_sha256: "ecc77c4f5d9205177749aee0068c5636ab3d8391ee9d9443da6eebb8fdd214f6",
        stream_sha256: &["bec9769df20be5dd7882f10fe5ad59150887bfae8641fd25e9bd446446705253"],
        stream_sizes: &[35_824],
        layout: GraphicLayout::MappedTilesAndDescriptorSprites,
        consumer_program: "MAIN.OVL",
        consumer_filename_decoded_offset: 0x62fb,
    },
];

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphicTextReviewReport {
    pub resources: Vec<GraphicTextReviewResource>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphicTextReviewResource {
    pub name: String,
    pub packed_sha256: String,
    pub decoded_stream_sha256: Vec<String>,
    pub width: usize,
    pub height: usize,
    pub pixel_layout: String,
    pub consumer_program: String,
    pub consumer_filename_decoded_offsets: Vec<usize>,
    pub review_bitmap: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicTextCatalog {
    pub schema: String,
    pub resource_count: usize,
    pub surface_count: usize,
    pub target_count: usize,
    pub excluded_count: usize,
    pub resources: Vec<GraphicTextResource>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicTextResource {
    pub name: String,
    pub source_packed_sha256: String,
    pub source_decoded_stream_sha256: Vec<String>,
    pub width: usize,
    pub height: usize,
    pub pixel_layout: String,
    pub consumer_program: String,
    pub consumer_filename_decoded_offsets: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_composition_bounds: Option<[usize; 4]>,
    pub surfaces: Vec<GraphicTextSurface>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicTextSurface {
    pub id: String,
    pub source_transcription: String,
    pub source_transcription_status: String,
    pub review_bounds: [usize; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_bounds: Option<[usize; 4]>,
    pub disposition: GraphicTextDisposition,
    pub ko: String,
    pub status: String,
    pub notes: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicTextDisposition {
    Target,
    Excluded,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphicTextCatalogValidation {
    pub resource_count: usize,
    pub surface_count: usize,
    pub target_count: usize,
    pub excluded_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphicTextRebuildReport {
    pub resources: Vec<GraphicTextResourceRebuildReport>,
    pub font: FontReport,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GraphicTextResourceRebuildReport {
    pub resource: String,
    pub changed_surface_ids: Vec<String>,
    pub original_packed_sha256: String,
    pub original_decoded_sha256: Vec<String>,
    pub updated_packed_sha256: String,
    pub updated_decoded_sha256: Vec<String>,
    pub original_packed_size: usize,
    pub updated_packed_size: usize,
    pub preserved_protected_pixels: bool,
    pub roundtrip_decoded_pixels: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TitleArtManifest {
    schema: String,
    status: String,
    composition_bounds: [usize; 4],
    composition: TitleComposition,
    labels: Vec<TitleLabel>,
    generation: TitleArtGeneration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TitleComposition {
    file: String,
    sha256: String,
    width: usize,
    height: usize,
    destination: [usize; 4],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TitleLabel {
    surface_id: String,
    catalog_text: String,
    rendered_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TitleArtGeneration {
    tool: String,
    use_case: String,
    source_resource: String,
    source_packed_sha256: String,
    source_render_sha256: String,
    source_render_dimensions: [usize; 2],
    source_output_sha256: String,
    source_dimensions: [usize; 2],
    prompt: String,
    normalization: String,
}

pub fn extract_graphic_text_catalog(source_path: &Path) -> Result<GraphicTextCatalog> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    catalog_from_payload(&payload)
}

fn catalog_from_payload(payload: &BTreeMap<String, Vec<u8>>) -> Result<GraphicTextCatalog> {
    validate_main_graphic_consumers(payload)?;
    let resources = GRAPHIC_PROFILES
        .iter()
        .map(|profile| catalog_resource(payload, profile))
        .collect::<Result<Vec<_>>>()?;
    let surface_count = resources
        .iter()
        .map(|resource| resource.surfaces.len())
        .sum();
    let target_count = resources
        .iter()
        .flat_map(|resource| &resource.surfaces)
        .filter(|surface| surface.disposition == GraphicTextDisposition::Target)
        .count();
    let excluded_count = surface_count - target_count;
    Ok(GraphicTextCatalog {
        schema: CATALOG_SCHEMA.to_owned(),
        resource_count: resources.len(),
        surface_count,
        target_count,
        excluded_count,
        resources,
    })
}

pub fn write_graphic_text_catalog(
    source_path: &Path,
    output_path: &Path,
) -> Result<GraphicTextCatalog> {
    let catalog = extract_graphic_text_catalog(source_path)?;
    let encoded = serde_json::to_vec_pretty(&catalog).context("serialize graphic text catalog")?;
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create graphic text catalog directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(output_path, encoded).with_context(|| {
        format!(
            "failed to write graphic text catalog: {}",
            output_path.display()
        )
    })?;
    Ok(catalog)
}

pub fn validate_graphic_text_catalog(
    source_path: &Path,
    catalog_path: &Path,
) -> Result<GraphicTextCatalogValidation> {
    let expected = extract_graphic_text_catalog(source_path)?;
    let supplied = read_graphic_text_catalog(catalog_path)?;
    validate_catalog_against_expected(&supplied, &expected)?;
    Ok(GraphicTextCatalogValidation {
        resource_count: expected.resource_count,
        surface_count: expected.surface_count,
        target_count: expected.target_count,
        excluded_count: expected.excluded_count,
    })
}

fn read_graphic_text_catalog(catalog_path: &Path) -> Result<GraphicTextCatalog> {
    let bytes = fs::read(catalog_path).with_context(|| {
        format!(
            "failed to read graphic text catalog: {}",
            catalog_path.display()
        )
    })?;
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "failed to parse graphic text catalog: {}",
            catalog_path.display()
        )
    })
}

fn validate_catalog_against_expected(
    supplied: &GraphicTextCatalog,
    expected: &GraphicTextCatalog,
) -> Result<()> {
    validate_translation_states(supplied)?;
    let mut protected = supplied.clone();
    for resource in &mut protected.resources {
        for surface in &mut resource.surfaces {
            surface.ko.clear();
            surface.notes.clear();
            if surface.disposition == GraphicTextDisposition::Target {
                surface.status = UNTRANSLATED_STATUS.to_owned();
            }
        }
    }
    ensure!(
        protected == *expected,
        "graphic text catalog protected fields differ from the exact source review catalog"
    );
    Ok(())
}

pub fn render_graphic_text_sources(
    source_path: &Path,
    output_dir: &Path,
) -> Result<GraphicTextReviewReport> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    validate_main_graphic_consumers(&payload)?;
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create graphic text review directory: {}",
            output_dir.display()
        )
    })?;

    let resources = GRAPHIC_PROFILES
        .iter()
        .map(|profile| render_resource(&payload, profile, output_dir))
        .collect::<Result<Vec<_>>>()?;
    Ok(GraphicTextReviewReport { resources })
}

fn render_resource(
    payload: &BTreeMap<String, Vec<u8>>,
    profile: &GraphicProfile,
    output_dir: &Path,
) -> Result<GraphicTextReviewResource> {
    let packed = payload
        .get(profile.name)
        .with_context(|| format!("MADOU.EXE payload is missing {}", profile.name))?;
    ensure!(
        sha256_hex(packed) == profile.packed_sha256,
        "{} packed bytes differ from the supported graphic resource",
        profile.name
    );
    let decoded = decode_exact_compile_lz(packed)
        .with_context(|| format!("{} is not an exact Compile-LZ resource", profile.name))?;
    validate_decoded_streams(profile, &decoded.streams)?;

    let (rgb, pixel_layout) = match profile.layout {
        GraphicLayout::PlanarBrgi => (
            render_planar_brgi(&decoded.streams)?,
            "640x400 row-major B/R/G/I planes",
        ),
        GraphicLayout::ColumnMajorBrgi => {
            let planes = decoded.streams[0]
                .chunks_exact(PLANE_BYTES)
                .map(transpose_column_major_plane)
                .collect::<Vec<_>>();
            (
                render_planar_brgi(&planes)?,
                "640x400 column-major B/R/G/I planes",
            )
        }
        GraphicLayout::MixedBrgiSpriteAtlas => (
            render_event_sprite_atlas(&decoded.streams[0])?,
            "640x400 atlas of mixed tile and row-interleaved B/R/G/I sprites",
        ),
        GraphicLayout::IndexedPlaneMajorBrgiTileAtlas {
            tile_bank_offset,
            map_entry_count,
        } => (
            render_indexed_plane_major_brgi_tile_atlas(
                &decoded.streams[0],
                tile_bank_offset,
                map_entry_count,
            )?,
            indexed_tile_atlas_layout(tile_bank_offset),
        ),
        GraphicLayout::MappedTilesAndDescriptorSprites => (
            render_mapped_tiles_and_descriptor_sprites(&decoded.streams[0])?,
            "640x400 atlas of 130 mapped 16x16 plane-major B/R/G/I tiles and 20 descriptor-driven row-interleaved sprites",
        ),
        GraphicLayout::Monochrome => (
            render_monochrome(&decoded.streams[0])?,
            "640x400 row-major 1bpp",
        ),
    };
    let review_bitmap = output_dir.join(format!("{}.bmp", profile.name));
    fs::write(&review_bitmap, encode_rgb_bmp(&rgb)?).with_context(|| {
        format!(
            "failed to write graphic text review bitmap: {}",
            review_bitmap.display()
        )
    })?;

    Ok(GraphicTextReviewResource {
        name: profile.name.to_owned(),
        packed_sha256: sha256_hex(packed),
        decoded_stream_sha256: decoded
            .streams
            .iter()
            .map(|stream| sha256_hex(stream))
            .collect(),
        width: SCREEN_WIDTH,
        height: SCREEN_HEIGHT,
        pixel_layout: pixel_layout.to_owned(),
        consumer_program: profile.consumer_program.to_owned(),
        consumer_filename_decoded_offsets: vec![profile.consumer_filename_decoded_offset],
        review_bitmap,
    })
}

fn catalog_resource(
    payload: &BTreeMap<String, Vec<u8>>,
    profile: &GraphicProfile,
) -> Result<GraphicTextResource> {
    let packed = payload
        .get(profile.name)
        .with_context(|| format!("MADOU.EXE payload is missing {}", profile.name))?;
    ensure!(
        sha256_hex(packed) == profile.packed_sha256,
        "{} packed bytes differ from the supported graphic resource",
        profile.name
    );
    let decoded = decode_exact_compile_lz(packed)
        .with_context(|| format!("{} is not an exact Compile-LZ resource", profile.name))?;
    validate_decoded_streams(profile, &decoded.streams)?;

    let (pixel_layout, surfaces) = match profile.name {
        "TITLE.CNS" => ("640x400 row-major B/R/G/I planes", title_surfaces()),
        "ED7.CNS" => ("640x400 row-major 1bpp", ending_surfaces()),
        "FRAME.CS" => ("640x400 column-major B/R/G/I planes", Vec::new()),
        "EVENT.CNS" => (
            "640x400 atlas of mixed tile and row-interleaved B/R/G/I sprites",
            Vec::new(),
        ),
        "BATTLE.CNS" => (
            "640x400 atlas of mapped 16x16 plane-major B/R/G/I source tiles after a 0x480-byte header",
            Vec::new(),
        ),
        "DUNGEON.CNS" => (
            "640x400 atlas of mapped 16x16 plane-major B/R/G/I source tiles after a 0xc00-byte header",
            Vec::new(),
        ),
        "FACE.CNS" => (
            "640x400 atlas of 130 mapped 16x16 plane-major B/R/G/I tiles and 20 descriptor-driven row-interleaved sprites",
            Vec::new(),
        ),
        _ => unreachable!("graphic profile has a catalog layout"),
    };
    let owned_composition_bounds = if profile.name == "TITLE.CNS" {
        Some(load_title_art_manifest()?.composition_bounds)
    } else {
        None
    };
    validate_resource_bounds(profile.name, owned_composition_bounds, &surfaces)?;
    if profile.name == "ED7.CNS" {
        validate_monochrome_text_only_source(&decoded.streams[0], &surfaces)?;
    }
    Ok(GraphicTextResource {
        name: profile.name.to_owned(),
        source_packed_sha256: sha256_hex(packed),
        source_decoded_stream_sha256: decoded
            .streams
            .iter()
            .map(|stream| sha256_hex(stream))
            .collect(),
        width: SCREEN_WIDTH,
        height: SCREEN_HEIGHT,
        pixel_layout: pixel_layout.to_owned(),
        consumer_program: profile.consumer_program.to_owned(),
        consumer_filename_decoded_offsets: vec![profile.consumer_filename_decoded_offset],
        owned_composition_bounds,
        surfaces,
    })
}

fn validate_main_graphic_consumers(payload: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let packed = payload
        .get("MAIN.OVL")
        .context("MADOU.EXE payload is missing MAIN.OVL")?;
    ensure!(
        sha256_hex(packed) == "419178fd0a795f629a9d67f68d58e1e6cb7539d149cfeed435a5d2c21bda924a",
        "MAIN.OVL differs from the supported FRAME.CS and EVENT.CNS consumer"
    );
    let decoded = decode_exact_compile_lz(packed).context("decode FRAME.CS consumer MAIN.OVL")?;
    let [decoded] = decoded.streams.as_slice() else {
        bail!("FRAME.CS consumer MAIN.OVL must contain one decoded stream");
    };
    ensure!(
        sha256_hex(decoded) == "6886bdfcba6bfdc462be854322d6b67fd854ef4008efd03a831831c8b3f0f736",
        "decoded MAIN.OVL differs from the supported FRAME.CS and EVENT.CNS consumer"
    );
    ensure!(
        decoded.get(0x011b..0x013e)
            == Some(
                [
                    0x8b, 0x16, 0xd4, 0x57, 0x8b, 0x3e, 0x38, 0x9f, 0xe8, 0xea, 0x43, 0x8e, 0x1e,
                    0x38, 0x9f, 0x33, 0xf6, 0xb9, 0x00, 0xa8, 0x8e, 0xc1, 0xbb, 0x00, 0xb0, 0xb9,
                    0x00, 0xb8, 0xba, 0x00, 0xe0, 0xb4, 0x05, 0xcd, 0x7c,
                ]
                .as_slice()
            ),
        "MAIN.OVL FRAME.CS load and AH=5 consumer changed"
    );
    let logical = u16::from_le_bytes(
        decoded[0x56d4..0x56d6]
            .try_into()
            .expect("a two-byte range converts to an array"),
    );
    ensure!(
        logical == 0x63e2 && decoded.get(0x62e2..0x62ed) == Some(b"0:FRAME.CS\0"),
        "MAIN.OVL FRAME.CS filename binding changed"
    );

    ensure!(
        decoded.get(0x00f8..0x0115)
            == Some(
                [
                    0x8b, 0x16, 0xd6, 0x57, 0x8b, 0x3e, 0x38, 0x9f, 0xe8, 0x0d, 0x44, 0x1e, 0x8e,
                    0x06, 0x3e, 0x9f, 0x8e, 0x1e, 0x38, 0x9f, 0x33, 0xf6, 0x8b, 0xfe, 0xb4, 0x03,
                    0xcd, 0x7c, 0x1f,
                ]
                .as_slice()
            ),
        "MAIN.OVL EVENT.CNS load and decode path changed"
    );
    ensure!(
        decoded.get(0x0083..0x008d)
            == Some([0x2e, 0xa1, 0x3e, 0x9f, 0x05, 0x10, 0x01, 0xe8, 0x6e, 0x4f].as_slice()),
        "MAIN.OVL EVENT.CNS 0x1100 animation-region selector changed"
    );
    ensure!(
        decoded.get(0x2371..0x238f)
            == Some(
                [
                    0x2e, 0xa1, 0x3e, 0x9f, 0x05, 0x60, 0x01, 0x8e, 0xd8, 0xbf, 0xa6, 0x3e, 0xbd,
                    0xde, 0x9e, 0xc7, 0x46, 0x00, 0x04, 0x00, 0xc7, 0x46, 0x02, 0x20, 0x00, 0x33,
                    0xc0, 0xcd, 0x79, 0xc3,
                ]
                .as_slice()
            ),
        "MAIN.OVL EVENT.CNS 0x1600 32x32 direct-blit selector changed"
    );
    ensure!(
        decoded.get(0x23e5..0x241d)
            == Some(
                [
                    0xb1, 0x48, 0xba, 0xb1, 0x50, 0xba, 0xb1, 0x58, 0xb5, 0x01, 0x50, 0x98, 0x8b,
                    0xd8, 0x2e, 0x8b, 0xbf, 0x56, 0x60, 0x2e, 0x03, 0x0e, 0x3e, 0x9f, 0x8e, 0xd9,
                    0x33, 0xf6, 0x68, 0x00, 0xa8, 0x68, 0x00, 0xb0, 0x68, 0x00, 0xb8, 0x68, 0x00,
                    0xe0, 0x6a, 0x10, 0x6a, 0x02, 0x8b, 0xec, 0xb8, 0x80, 0x00, 0xcd, 0x79, 0x83,
                    0xc4, 0x0c, 0x58, 0xc3,
                ]
                .as_slice()
            ),
        "MAIN.OVL EVENT.CNS 0x1480/0x1500/0x1580 masked selectors changed"
    );
    ensure!(
        decoded.get(0x1a3f..0x1a90)
            == Some(
                [
                    0x2e, 0xa1, 0x3e, 0x9f, 0x05, 0xe0, 0x01, 0x50, 0x68, 0x00, 0x0e, 0x6a, 0x00,
                    0xb9, 0x1c, 0x00, 0x51, 0xb4, 0x07, 0xcd, 0x7d, 0xa8, 0x10, 0x75, 0x0a, 0xb4,
                    0x02, 0xcd, 0x7a, 0xe8, 0x8f, 0x38, 0xe8, 0x3e, 0x3a, 0x8b, 0xec, 0x8b, 0x46,
                    0x02, 0x05, 0x04, 0x00, 0x89, 0x46, 0x02, 0xc5, 0x76, 0x04, 0x81, 0xee, 0x80,
                    0x00, 0x89, 0x76, 0x04, 0xbf, 0x24, 0x50, 0xbd, 0xde, 0x9e, 0xc7, 0x46, 0x00,
                    0x08, 0x00, 0x89, 0x46, 0x02, 0x33, 0xc0, 0xcd, 0x79, 0x59, 0xe2, 0xc3, 0x83,
                    0xc4, 0x06, 0xe9,
                ]
                .as_slice()
            ),
        "MAIN.OVL EVENT.CNS 0x1e00 64x112 transition selector changed"
    );
    ensure!(
        decoded.get(0x36a3..0x36aa) == Some([0x2e, 0xa1, 0x3e, 0x9f, 0x89, 0x46, 0x10].as_slice())
            && decoded.get(0x36f4..0x36fd)
                == Some([0xb9, 0x08, 0x00, 0x8b, 0xd1, 0xb4, 0x02, 0xcd, 0x79].as_slice()),
        "MAIN.OVL EVENT.CNS initial tile-region AH=2 binding changed"
    );
    let event_logical = u16::from_le_bytes(
        decoded[0x56d6..0x56d8]
            .try_into()
            .expect("a two-byte range converts to an array"),
    );
    ensure!(
        event_logical == 0x63ed && decoded.get(0x62ed..0x62f9) == Some(b"0:EVENT.CNS\0"),
        "MAIN.OVL EVENT.CNS filename binding changed"
    );

    ensure!(
        decoded.get(0x027e..0x028d)
            == Some(
                [
                    0xb4, 0x02, 0xba, 0x1c, 0x00, 0xcd, 0x7b, 0x8b, 0xd0, 0xbf, 0x00, 0xe0, 0xe8,
                    0x83, 0x42,
                ]
                .as_slice()
            ),
        "MAIN.OVL BATTLE.CNS state-table load into E000h changed"
    );
    ensure!(
        decoded.get(0x29cb..0x29d5)
            == Some([0xb0, 0x01, 0xe8, 0x0b, 0xd9, 0xb0, 0x01, 0xe8, 0xb3, 0xda].as_slice()),
        "MAIN.OVL BATTLE.CNS selector-to-map-consumer binding changed"
    );
    ensure!(
        decoded.get(0x04fd..0x050d)
            == Some(
                [
                    0x2e, 0x8e, 0x1e, 0x38, 0x9f, 0xd1, 0xe3, 0xd1, 0xe3, 0x8b, 0x77, 0x06, 0x8b,
                    0x47, 0x04, 0x86,
                ]
                .as_slice()
            )
            && decoded.get(0x052c..0x0556)
                == Some(
                    [
                        0xad, 0x8b, 0xd8, 0x2a, 0xd4, 0xba, 0x06, 0x00, 0xb0, 0x0e, 0x2a, 0xc4,
                        0x98, 0xd1, 0xe0, 0xd1, 0xe0, 0xd1, 0xe0, 0x8a, 0xcf, 0xa5, 0x03, 0xfa,
                        0xe2, 0xfb, 0x03, 0xf8, 0xfe, 0xcb, 0x75, 0xf3, 0xb9, 0x0e, 0x00, 0xba,
                        0x0d, 0x00, 0xb4, 0x02, 0xcd, 0x79,
                    ]
                    .as_slice()
                ),
        "MAIN.OVL BATTLE.CNS indexed map and 14x13 AH=2 consumer changed"
    );
    let battle_logical = u16::from_le_bytes(
        decoded[0x56dc..0x56de]
            .try_into()
            .expect("a two-byte range converts to an array"),
    );
    ensure!(
        battle_logical == 0x6404 && decoded.get(0x6304..0x6311) == Some(b"0:BATTLE.CNS\0"),
        "MAIN.OVL BATTLE.CNS state-table filename binding changed"
    );

    ensure!(
        decoded.get(0x0204..0x022a)
            == Some(
                [
                    0x8c, 0xc8, 0x8e, 0xd8, 0xb4, 0x02, 0xba, 0x18, 0x00, 0xcd, 0x7b, 0x8b, 0xd0,
                    0x8b, 0x3e, 0x38, 0x9f, 0xe8, 0xf8, 0x42, 0x1e, 0x8e, 0x06, 0x3a, 0x9f, 0x8e,
                    0x1e, 0x38, 0x9f, 0x33, 0xf6, 0x8b, 0xfe, 0xb4, 0x03, 0xcd, 0x7c, 0x1f,
                ]
                .as_slice()
            ),
        "MAIN.OVL DUNGEON.CNS state-table load and decode path changed"
    );
    ensure!(
        decoded.get(0x022a..0x024f)
            == Some(
                [
                    0xa1, 0x3a, 0x9f, 0x8e, 0xc0, 0x26, 0x8b, 0x16, 0x00, 0x00, 0xc1, 0xea, 0x04,
                    0x03, 0xc2, 0xa3, 0x02, 0x9f, 0x80, 0xc4, 0x08, 0xa3, 0x04, 0x9f, 0x80, 0xc4,
                    0x08, 0xa3, 0x06, 0x9f, 0x80, 0xc4, 0x08, 0xa3, 0x08, 0x9f, 0xfc,
                ]
                .as_slice()
            ),
        "MAIN.OVL DUNGEON.CNS tile-bank segment selection changed"
    );
    ensure!(
        decoded.get(0x227d..0x2291)
            == Some(
                [
                    0x1e, 0x56, 0x57, 0x52, 0x2e, 0x8e, 0x1e, 0x3a, 0x9f, 0xbe, 0x04, 0x00, 0xe8,
                    0x3b, 0x00, 0x5a, 0x5f, 0x5e, 0x1f, 0xc3,
                ]
                .as_slice()
            ),
        "MAIN.OVL DUNGEON.CNS indexed map-table binding changed"
    );
    ensure!(
        decoded.get(0x22c7..0x2309)
            == Some(
                [
                    0x33, 0xdb, 0xfc, 0xd1, 0xe0, 0xd1, 0xe0, 0x03, 0xf0, 0xad, 0x02, 0xc3, 0x02,
                    0xe7, 0x2e, 0xc6, 0x06, 0x34, 0x9f, 0x01, 0x8c, 0xd2, 0x8e, 0xc2, 0x2e, 0x8b,
                    0x3e, 0xf8, 0x9e, 0x2e, 0x8a, 0x16, 0x32, 0x9f, 0x32, 0xf6, 0x83, 0xe1, 0x03,
                    0xd1, 0xe1, 0x03, 0xf9, 0x8a, 0xcc, 0xf6, 0xe2, 0x03, 0xc1, 0xd1, 0xe0, 0xd1,
                    0xe0, 0xd1, 0xe0, 0x03, 0xf8, 0xad, 0x8b, 0xf0, 0xad, 0x0b, 0xc0, 0x75, 0x01,
                    0xc3,
                ]
                .as_slice()
            )
            && decoded.get(0x2309..0x2333)
                == Some(
                    [
                        0x8b, 0xd8, 0x2a, 0xd4, 0xd1, 0xe2, 0xd1, 0xe2, 0xd1, 0xe2, 0x8a, 0xcf,
                        0xad, 0x0b, 0xc0, 0x74, 0x0d, 0xab, 0x83, 0xc7, 0x06, 0xe2, 0xf5, 0x03,
                        0xfa, 0xfe, 0xcb, 0x75, 0xed, 0xc3, 0x83, 0xc7, 0x08, 0xe2, 0xe9, 0x03,
                        0xfa, 0xfe, 0xcb, 0x75, 0xe1, 0xc3,
                    ]
                    .as_slice()
                ),
        "MAIN.OVL DUNGEON.CNS map dimensions and tile-word consumer changed"
    );
    let dungeon_logical = u16::from_le_bytes(
        decoded[0x56d8..0x56da]
            .try_into()
            .expect("a two-byte range converts to an array"),
    );
    ensure!(
        dungeon_logical == 0x641c && decoded.get(0x631c..0x632a) == Some(b"0:DUNGEON.CNS\0"),
        "MAIN.OVL DUNGEON.CNS state-table filename binding changed"
    );

    ensure!(
        decoded.get(0x0261..0x027e)
            == Some(
                [
                    0xb4, 0x05, 0xcd, 0x7a, 0xb8, 0x01, 0x00, 0xe8, 0x36, 0x53, 0x8c, 0xc8, 0x8e,
                    0xd8, 0xb4, 0x02, 0xba, 0x1a, 0x00, 0xcd, 0x7b, 0x8b, 0xd0, 0xbf, 0x00, 0xb8,
                    0xe8, 0x92, 0x42,
                ]
                .as_slice()
            ),
        "MAIN.OVL FACE.CNS state-table load into B800h changed"
    );
    ensure!(
        decoded.get(0x02db..0x030e)
            == Some(
                [
                    0x2e, 0x8a, 0x26, 0x42, 0x9f, 0x25, 0x01, 0xfe, 0x0a, 0xc4, 0x2e, 0xa2, 0x42,
                    0x9f, 0xb8, 0x00, 0xb8, 0x74, 0x03, 0xb8, 0x00, 0xe0, 0x8e, 0xd8, 0xb4, 0x05,
                    0xcd, 0x7a, 0xb8, 0x01, 0x00, 0xe8, 0xa4, 0x52, 0x2e, 0x8e, 0x06, 0x38, 0x9f,
                    0x33, 0xf6, 0x8b, 0xfe, 0xb4, 0x03, 0xcd, 0x7c, 0xb4, 0x04, 0xcd, 0x7a,
                ]
                .as_slice()
            ),
        "MAIN.OVL FACE/BATTLE selector and scratch decode path changed"
    );
    ensure!(
        decoded.get(0x044a..0x0480)
            == Some(
                [
                    0x2e, 0x8b, 0x0e, 0x38, 0x9f, 0x8e, 0xd9, 0xa1, 0x00, 0x00, 0xc1, 0xe8, 0x04,
                    0x03, 0xc1, 0x89, 0x46, 0x10, 0x80, 0xc4, 0x08, 0x89, 0x46, 0x12, 0x8b, 0x36,
                    0x06, 0x00, 0x8b, 0x7e, 0x06, 0x83, 0xc7, 0x06, 0xad, 0xb9, 0xb6, 0x00, 0xa5,
                    0x83, 0xc7, 0x06, 0xe2, 0xfa, 0xb9, 0x0e, 0x00, 0xba, 0x0d, 0x00, 0xb4, 0x02,
                    0xcd, 0x79,
                ]
                .as_slice()
            ),
        "MAIN.OVL FACE.CNS 14x13 mapped-tile consumer changed"
    );
    ensure!(
        decoded.get(0x5340..0x538d)
            == Some(
                [
                    0x8b, 0xd8, 0xa3, 0x68, 0x55, 0xd1, 0xe0, 0xd1, 0xe0, 0x03, 0xc3, 0x03, 0xc3,
                    0x05, 0x6a, 0x55, 0x8b, 0xf8, 0x8b, 0x05, 0xa3, 0x64, 0x55, 0xc7, 0x06, 0x66,
                    0x55, 0x00, 0x00, 0x8b, 0x45, 0x04, 0x50, 0x8b, 0x45, 0x02, 0x50, 0x8a, 0xd8,
                    0xb7, 0x00, 0x1e, 0xe8, 0xcb, 0x00, 0x1f, 0x58, 0x8a, 0xdc, 0xb7, 0x00, 0x1e,
                    0xe8, 0xc1, 0x00, 0x1f, 0x58, 0x50, 0x8a, 0xd8, 0xb7, 0x00, 0x1e, 0xe8, 0xb6,
                    0x00, 0x1f, 0x58, 0x8a, 0xdc, 0xb7, 0x00, 0x1e, 0xe8, 0xac, 0x00, 0x1f,
                ]
                .as_slice()
            ),
        "MAIN.OVL FACE.CNS descriptor-combination table consumer changed"
    );
    ensure!(
        decoded.get(0x5438..0x5464)
            == Some(
                [
                    0xf6, 0x06, 0x44, 0x9f, 0x01, 0x75, 0x24, 0xbd, 0xde, 0x9e, 0x8e, 0x1e, 0x38,
                    0x9f, 0x8b, 0x07, 0x8a, 0xcc, 0x32, 0xe4, 0x89, 0x46, 0x00, 0x8a, 0xc1, 0x89,
                    0x46, 0x02, 0x8b, 0x7f, 0x02, 0x81, 0xc7, 0x2c, 0x0a, 0x8b, 0x77, 0x04, 0xb8,
                    0x00, 0x00, 0xcd, 0x79, 0xc3,
                ]
                .as_slice()
            ),
        "MAIN.OVL FACE.CNS six-byte descriptor and AH=0 consumer changed"
    );
    let animated_descriptor_offsets = [
        (0x53ab, 0x0e_u8),
        (0x53bd, 0x14),
        (0x53cf, 0x0e),
        (0x53e8, 0x08),
        (0x53f9, 0x0e),
        (0x540a, 0x14),
        (0x541b, 0x0e),
        (0x5433, 0x1a),
    ];
    for (offset, expected_descriptor) in animated_descriptor_offsets {
        ensure!(
            decoded.get(offset..offset + 3) == Some([0xbb, expected_descriptor, 0x00].as_slice()),
            "MAIN.OVL FACE.CNS animated descriptor selection changed at {offset:#06x}"
        );
    }
    let combined_descriptors = decoded
        .get(0x546a..0x54a0)
        .context("MAIN.OVL FACE.CNS descriptor-combination table is truncated")?;
    let mut combined_descriptor_offsets = BTreeSet::new();
    for entry in combined_descriptors.chunks_exact(6) {
        let consumer = u16::from_le_bytes([entry[0], entry[1]]);
        ensure!(
            consumer == 0x54ed || consumer == 0x549f,
            "MAIN.OVL FACE.CNS descriptor-combination consumer changed"
        );
        combined_descriptor_offsets.extend(entry[2..].iter().copied());
    }
    ensure!(
        combined_descriptor_offsets
            == BTreeSet::from([
                0x20, 0x26, 0x2c, 0x32, 0x38, 0x3e, 0x44, 0x4a, 0x50, 0x56, 0x5c, 0x62, 0x68, 0x6e,
                0x74, 0x7a,
            ]),
        "MAIN.OVL FACE.CNS descriptor-combination coverage changed"
    );
    let face_logical = u16::from_le_bytes(
        decoded[0x56da..0x56dc]
            .try_into()
            .expect("a two-byte range converts to an array"),
    );
    ensure!(
        face_logical == 0x63f9 && decoded.get(0x62f9..0x6304) == Some(b"0:FACE.CNS\0"),
        "MAIN.OVL FACE.CNS state-table filename binding changed"
    );

    let main_com = payload
        .get("MAIN.COM")
        .context("MADOU.EXE payload is missing MAIN.COM")?;
    ensure!(
        sha256_hex(main_com) == "50954a2dce3f4d424558718b753029ae27b0a33273563cb2b3792318da1e6957",
        "MAIN.COM differs from the supported graphics consumer"
    );
    let main_com = decode_main_com_image(main_com).context("decode graphics consumer MAIN.COM")?;
    ensure!(
        main_com.len() == 10_992
            && sha256_hex(&main_com)
                == "0302fc6f45a02528de222f9483564c807be8038a4b48224b2e3ce4ac7669ee30",
        "self-unpacked MAIN.COM differs from the supported graphics consumer"
    );
    ensure!(
        main_com.get(0x06b4..0x06bc)
            == Some([0x80, 0x0c, 0x21, 0x0f, 0x70, 0x0f, 0x19, 0x1f].as_slice()),
        "MAIN.COM INT 79h graphics dispatch table changed"
    );
    ensure!(
        main_com.get(0x0ba0..0x0bd6)
            == Some(
                [
                    0xfc, 0x8b, 0xd7, 0x8b, 0x5e, 0x00, 0xd1, 0xeb, 0x8b, 0x46, 0x02, 0x8b, 0xfa,
                    0x8e, 0x46, 0x0a, 0x8b, 0xcb, 0xf3, 0xa5, 0x8b, 0xfa, 0x8e, 0x46, 0x08, 0x8b,
                    0xcb, 0xf3, 0xa5, 0x8b, 0xfa, 0x8e, 0x46, 0x06, 0x8b, 0xcb, 0xf3, 0xa5, 0x8b,
                    0xfa, 0x8e, 0x46, 0x04, 0x8b, 0xcb, 0xf3, 0xa5, 0x83, 0xc2, 0x50, 0x48, 0x75,
                    0xd6, 0xc3,
                ]
                .as_slice()
            ),
        "MAIN.COM row-interleaved B/R/G/I direct copier changed"
    );
    ensure!(
        main_com.get(0x0cdb..0x0d22)
            == Some(
                [
                    0xfc, 0x8b, 0xd7, 0x8b, 0x5e, 0x00, 0x2b, 0xe3, 0xd1, 0xeb, 0x89, 0x5e, 0x00,
                    0x8c, 0xd0, 0x8e, 0xc0, 0x8b, 0xfc, 0x56, 0x8b, 0x4e, 0x00, 0x8b, 0xd9, 0x4b,
                    0xd1, 0xe3, 0xad, 0x0b, 0x00, 0xab, 0xe2, 0xfa, 0x8b, 0x4e, 0x00, 0x8b, 0xfc,
                    0x47, 0x47, 0x8d, 0x70, 0x02, 0xad, 0x26, 0x0b, 0x05, 0xab, 0xe2, 0xf9, 0x8b,
                    0x4e, 0x00, 0x8b, 0xfc, 0x47, 0x47, 0xad, 0x26, 0x0b, 0x05, 0xf7, 0xd0, 0xab,
                    0xe2, 0xf7, 0x5e, 0x8e, 0x46, 0x0a,
                ]
                .as_slice()
            ),
        "MAIN.COM row-interleaved transparency-mask consumer changed"
    );
    ensure!(
        main_com.get(0x0e70..0x0e89)
            == Some(
                [
                    0xfc, 0x8c, 0xd0, 0x8e, 0xd8, 0x8b, 0x76, 0x06, 0x8b, 0x5e, 0x04, 0x8b, 0x7e,
                    0x02, 0x52, 0x51, 0x52, 0x51, 0x51, 0x56, 0x57, 0x53, 0xe8, 0x34, 0x00,
                ]
                .as_slice()
            ),
        "MAIN.COM INT 79h AH=2 tile consumer changed"
    );
    Ok(())
}

fn validate_decoded_streams(profile: &GraphicProfile, streams: &[Vec<u8>]) -> Result<()> {
    ensure!(
        profile.stream_sha256.len() == profile.stream_sizes.len(),
        "{} graphic profile stream contract is inconsistent",
        profile.name
    );
    ensure!(
        streams.len() == profile.stream_sha256.len(),
        "{} decoded stream count changed",
        profile.name
    );
    for (index, ((stream, expected_sha256), expected_size)) in streams
        .iter()
        .zip(profile.stream_sha256)
        .zip(profile.stream_sizes)
        .enumerate()
    {
        ensure!(
            stream.len() == *expected_size && sha256_hex(stream) == *expected_sha256,
            "{} decoded stream {index} differs from the supported graphic resource",
            profile.name
        );
    }
    Ok(())
}

fn title_surfaces() -> Vec<GraphicTextSurface> {
    vec![
        target_surface("TITLE_MAIN_LOGO", "魔導物語", [64, 48, 512, 112]),
        target_surface("TITLE_SUBTITLE", "道草異聞", [168, 152, 304, 88]),
        GraphicTextSurface {
            id: "TITLE_COPYRIGHT".to_owned(),
            source_transcription: "©1994 COMPILE".to_owned(),
            source_transcription_status: "needs_human_review".to_owned(),
            review_bounds: [248, 384, 144, 12],
            owned_bounds: None,
            disposition: GraphicTextDisposition::Excluded,
            ko: String::new(),
            status: EXCLUDED_STATUS.to_owned(),
            notes: String::new(),
        },
    ]
}

fn ending_surfaces() -> Vec<GraphicTextSurface> {
    vec![
        owned_target_surface(
            "ENDING_LEFT_TEXT",
            concat!(
                "ずいぶん古い魔導書だな・・・\n",
                "あっ　見て見て!\n",
                "ぐ?\n",
                "ぼくの知らない呪文だよ!\n",
                "えーと　「オワニモ」の呪文か・・・\n",
                "「色同じく四つの魔物あらわるとき\n",
                "　魔力を解放せよ\n",
                "　されば　時の女神が「門」を開き\n",
                "　魔物を異界へと誘わん\n",
                "ぐー?\n",
                "つまり　同じ色したモンスターが\n",
                "四匹そろったら\n",
                "消しちゃう　呪文なんだって\n",
                "ぐぐ?\n",
                "ぼくも　そんなモンスターなんて\n",
                "知らないよ\n",
                "これじゃ　役に立ちそーもないな・・・\n",
                "今回のアルルとカーバンクルの冒険は\n",
                "とりあえず　ここで終ります\n",
                "しかし　運命とは不思議なものです\n",
                "アルルが「オワニモ」と\n",
                "出会ったことのように\n",
                "闇の世界もまた"
            ),
            [0, 0, 288, 368],
        ),
        owned_target_surface(
            "ENDING_RIGHT_TEXT",
            concat!(
                "そのモンスターを生み出していました\n",
                "「ぷよぷよ」\n",
                "それが　新たな五色の体を授けられた\n",
                "モンスターの名前でした・・・"
            ),
            [288, 0, 288, 64],
        ),
        owned_target_surface(
            "ENDING_CONTINUATION_TAGLINE",
            "- TO BE CONTINUED PUYOPUYO -",
            [288, 64, 200, 16],
        ),
    ]
}

fn target_surface(
    id: &str,
    source_transcription: &str,
    review_bounds: [usize; 4],
) -> GraphicTextSurface {
    GraphicTextSurface {
        id: id.to_owned(),
        source_transcription: source_transcription.to_owned(),
        source_transcription_status: "needs_human_review".to_owned(),
        review_bounds,
        owned_bounds: None,
        disposition: GraphicTextDisposition::Target,
        ko: String::new(),
        status: UNTRANSLATED_STATUS.to_owned(),
        notes: String::new(),
    }
}

fn owned_target_surface(
    id: &str,
    source_transcription: &str,
    owned_bounds: [usize; 4],
) -> GraphicTextSurface {
    let mut surface = target_surface(id, source_transcription, owned_bounds);
    surface.owned_bounds = Some(owned_bounds);
    surface
}

fn validate_resource_bounds(
    name: &str,
    owned_composition_bounds: Option<[usize; 4]>,
    surfaces: &[GraphicTextSurface],
) -> Result<()> {
    if let Some([x, y, width, height]) = owned_composition_bounds {
        ensure!(
            width > 0
                && height > 0
                && x.is_multiple_of(8)
                && width.is_multiple_of(8)
                && x.checked_add(width)
                    .is_some_and(|right| right <= SCREEN_WIDTH)
                && y.checked_add(height)
                    .is_some_and(|bottom| bottom <= SCREEN_HEIGHT),
            "{name} has invalid byte-aligned composition ownership"
        );
    }
    for surface in surfaces {
        let [x, y, width, height] = surface.review_bounds;
        ensure!(
            width > 0
                && height > 0
                && x.checked_add(width)
                    .is_some_and(|right| right <= SCREEN_WIDTH)
                && y.checked_add(height)
                    .is_some_and(|bottom| bottom <= SCREEN_HEIGHT),
            "{} surface {} has out-of-bounds review coordinates",
            name,
            surface.id
        );
        if let Some([owned_x, owned_y, owned_width, owned_height]) = surface.owned_bounds {
            ensure!(
                surface.disposition == GraphicTextDisposition::Target,
                "{} surface {} gives an excluded surface pixel ownership",
                name,
                surface.id
            );
            ensure!(
                owned_width > 0
                    && owned_height > 0
                    && owned_x.is_multiple_of(8)
                    && owned_width.is_multiple_of(8)
                    && owned_x >= x
                    && owned_y >= y
                    && owned_x
                        .checked_add(owned_width)
                        .is_some_and(|right| right <= x + width)
                    && owned_y
                        .checked_add(owned_height)
                        .is_some_and(|bottom| bottom <= y + height),
                "{} surface {} has invalid byte-aligned pixel ownership",
                name,
                surface.id
            );
        }
        if surface.disposition == GraphicTextDisposition::Excluded
            && let Some(composition_bounds) = owned_composition_bounds
        {
            ensure!(
                !rectangles_overlap(surface.review_bounds, composition_bounds),
                "{} excluded surface {} overlaps composition ownership",
                name,
                surface.id
            );
        }
    }
    for (index, surface) in surfaces.iter().enumerate() {
        let Some(bounds) = surface.owned_bounds else {
            continue;
        };
        for other in &surfaces[index + 1..] {
            let Some(other_bounds) = other.owned_bounds else {
                continue;
            };
            ensure!(
                !rectangles_overlap(bounds, other_bounds),
                "{} surfaces {} and {} have overlapping pixel ownership",
                name,
                surface.id,
                other.id
            );
        }
    }
    Ok(())
}

fn rectangles_overlap(left: [usize; 4], right: [usize; 4]) -> bool {
    let [left_x, left_y, left_width, left_height] = left;
    let [right_x, right_y, right_width, right_height] = right;
    left_x < right_x + right_width
        && right_x < left_x + left_width
        && left_y < right_y + right_height
        && right_y < left_y + left_height
}

fn validate_translation_states(catalog: &GraphicTextCatalog) -> Result<()> {
    for resource in &catalog.resources {
        for surface in &resource.surfaces {
            match surface.disposition {
                GraphicTextDisposition::Target => match surface.status.as_str() {
                    "untranslated" => ensure!(
                        surface.ko.is_empty(),
                        "{}: untranslated target must have empty Korean text",
                        surface.id
                    ),
                    "in_progress"
                    | "needs_review"
                    | "needs_human_review"
                    | "distribution_eligible" => ensure!(
                        !surface.ko.is_empty(),
                        "{}: {} target must have Korean text",
                        surface.id,
                        surface.status
                    ),
                    other => bail!("{}: unsupported translation status {other:?}", surface.id),
                },
                GraphicTextDisposition::Excluded => ensure!(
                    surface.status == EXCLUDED_STATUS && surface.ko.is_empty(),
                    "{}: excluded surface must stay excluded with empty Korean text",
                    surface.id
                ),
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_graphic_text_drafts(
    payload: &mut BTreeMap<String, Vec<u8>>,
    catalog_path: &Path,
) -> Result<GraphicTextRebuildReport> {
    let expected_catalog = catalog_from_payload(payload)?;
    let supplied_catalog = read_graphic_text_catalog(catalog_path)?;
    validate_catalog_against_expected(&supplied_catalog, &expected_catalog)?;

    let title = supplied_catalog
        .resources
        .iter()
        .find(|resource| resource.name == "TITLE.CNS")
        .context("graphic text catalog is missing TITLE.CNS")?;
    let ending = supplied_catalog
        .resources
        .iter()
        .find(|resource| resource.name == "ED7.CNS")
        .context("graphic text catalog is missing ED7.CNS")?;
    let resources = vec![
        rebuild_title_graphic(payload, title)?,
        rebuild_ending_graphic(payload, ending)?,
    ];
    Ok(GraphicTextRebuildReport {
        resources,
        font: font_report()?,
    })
}

fn translated_target_surfaces(resource: &GraphicTextResource) -> Result<Vec<&GraphicTextSurface>> {
    let changed_surfaces = resource
        .surfaces
        .iter()
        .filter(|surface| surface.disposition == GraphicTextDisposition::Target)
        .map(|surface| {
            ensure!(
                surface.status != UNTRANSLATED_STATUS && !surface.ko.is_empty(),
                "{} must have a Korean draft before rebuilding {}",
                surface.id,
                resource.name
            );
            Ok(surface)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !changed_surfaces.is_empty(),
        "{} has no translated surfaces",
        resource.name
    );
    Ok(changed_surfaces)
}

fn rebuild_title_graphic(
    payload: &mut BTreeMap<String, Vec<u8>>,
    title: &GraphicTextResource,
) -> Result<GraphicTextResourceRebuildReport> {
    let changed_surfaces = translated_target_surfaces(title)?;
    let manifest = load_title_art_manifest()?;
    ensure!(
        title.owned_composition_bounds == Some(manifest.composition_bounds),
        "TITLE.CNS catalog and title-art manifest disagree on composition ownership"
    );

    let original_packed = payload
        .get("TITLE.CNS")
        .context("installer payload is missing TITLE.CNS")?;
    let profile = GRAPHIC_PROFILES
        .iter()
        .find(|profile| profile.name == "TITLE.CNS")
        .expect("TITLE.CNS has a graphic profile");
    ensure!(
        sha256_hex(original_packed) == profile.packed_sha256,
        "TITLE.CNS does not match the supported title graphic resource"
    );
    let decoded = decode_exact_compile_lz(original_packed)
        .context("TITLE.CNS is not an exact Compile-LZ resource")?;
    validate_decoded_streams(profile, &decoded.streams)?;

    let updated_streams =
        rebuild_title_composition(&decoded.streams, &changed_surfaces, &manifest)?;
    ensure!(
        updated_streams != decoded.streams,
        "graphic text drafts did not change TITLE.CNS"
    );
    verify_protected_planar_pixels(
        &decoded.streams,
        &updated_streams,
        manifest.composition_bounds,
    )?;

    let mut updated_packed = Vec::new();
    for stream in &updated_streams {
        updated_packed.extend_from_slice(&encode_compile_lz(stream));
    }
    let roundtrip = decode_exact_compile_lz(&updated_packed)
        .context("repacked TITLE.CNS did not decode as exact Compile-LZ")?;
    ensure!(
        roundtrip.streams == updated_streams,
        "repacked TITLE.CNS did not preserve the updated pixels"
    );
    let report = GraphicTextResourceRebuildReport {
        resource: "TITLE.CNS".to_owned(),
        changed_surface_ids: changed_surfaces
            .iter()
            .map(|surface| surface.id.clone())
            .collect(),
        original_packed_sha256: sha256_hex(original_packed),
        original_decoded_sha256: decoded
            .streams
            .iter()
            .map(|stream| sha256_hex(stream))
            .collect(),
        updated_packed_sha256: sha256_hex(&updated_packed),
        updated_decoded_sha256: updated_streams
            .iter()
            .map(|stream| sha256_hex(stream))
            .collect(),
        original_packed_size: original_packed.len(),
        updated_packed_size: updated_packed.len(),
        preserved_protected_pixels: true,
        roundtrip_decoded_pixels: true,
    };
    payload.insert("TITLE.CNS".to_owned(), updated_packed);
    Ok(report)
}

fn load_title_art_manifest() -> Result<TitleArtManifest> {
    let manifest: TitleArtManifest =
        serde_json::from_str(&local_input::read_to_string(TITLE_ART_PATH)?)
            .context("parse title-art manifest")?;
    ensure!(
        manifest.schema == TITLE_ART_SCHEMA,
        "unsupported title-art schema {:?}",
        manifest.schema
    );
    ensure!(
        matches!(
            manifest.status.as_str(),
            TITLE_ART_STATUS | TITLE_ART_RELEASE_STATUS
        ),
        "title art must have status {TITLE_ART_STATUS:?} or {TITLE_ART_RELEASE_STATUS:?}"
    );
    validate_byte_aligned_screen_bounds(manifest.composition_bounds)
        .context("invalid title composition bounds")?;
    ensure!(
        manifest.composition.file == "title-composition.png"
            && manifest.composition.sha256
                == sha256_hex(&local_input::read(TITLE_COMPOSITION_PATH)?),
        "title composition does not match its manifest"
    );
    ensure!(
        manifest.composition.width > 0
            && manifest.composition.height > 0
            && manifest.composition.destination[2] == manifest.composition.width
            && manifest.composition.destination[3] == manifest.composition.height
            && manifest.composition.destination == manifest.composition_bounds,
        "title composition dimensions or destination are invalid"
    );

    let expected_ids = BTreeSet::from(["TITLE_MAIN_LOGO", "TITLE_SUBTITLE"]);
    let actual_ids = manifest
        .labels
        .iter()
        .map(|label| label.surface_id.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        actual_ids == expected_ids && manifest.labels.len() == expected_ids.len(),
        "title-art manifest must define one label for each title target"
    );
    for label in &manifest.labels {
        ensure!(
            !label.catalog_text.is_empty() && !label.rendered_text.is_empty(),
            "title label {} is incomplete",
            label.surface_id
        );
    }
    ensure!(
        manifest.generation.tool == "OpenAI built-in image_gen"
            && manifest.generation.use_case == "source-guided-edit"
            && manifest.generation.source_resource == "TITLE.CNS"
            && manifest.generation.source_packed_sha256 == GRAPHIC_PROFILES[0].packed_sha256
            && manifest.generation.source_render_sha256.len() == 64
            && manifest
                .generation
                .source_render_dimensions
                .iter()
                .all(|value| *value > 0)
            && manifest.generation.source_output_sha256.len() == 64
            && manifest
                .generation
                .source_dimensions
                .iter()
                .all(|value| *value > 0)
            && !manifest.generation.prompt.is_empty()
            && !manifest.generation.normalization.is_empty(),
        "title-art generation provenance is incomplete"
    );
    Ok(manifest)
}

fn validate_byte_aligned_screen_bounds(bounds: [usize; 4]) -> Result<()> {
    let [x, y, width, height] = bounds;
    ensure!(
        width > 0
            && height > 0
            && x.is_multiple_of(8)
            && width.is_multiple_of(8)
            && x.checked_add(width)
                .is_some_and(|right| right <= SCREEN_WIDTH)
            && y.checked_add(height)
                .is_some_and(|bottom| bottom <= SCREEN_HEIGHT),
        "bounds are not a byte-aligned screen rectangle"
    );
    Ok(())
}

fn rebuild_title_composition(
    source_planes: &[Vec<u8>],
    surfaces: &[&GraphicTextSurface],
    manifest: &TitleArtManifest,
) -> Result<Vec<Vec<u8>>> {
    for label in &manifest.labels {
        let surface = surfaces
            .iter()
            .find(|surface| surface.id == label.surface_id)
            .with_context(|| format!("missing translated title surface {}", label.surface_id))?;
        ensure!(
            surface.ko == label.catalog_text,
            "title surface {} text does not match the adopted title composition",
            label.surface_id
        );
    }

    let mut indices = planar_brgi_indices(source_planes)?;
    let composition = decode_title_composition(manifest)?;
    let [destination_x, destination_y, width, height] = manifest.composition.destination;
    for y in 0..height {
        for x in 0..width {
            let Some(index) = composition[y * width + x] else {
                continue;
            };
            indices[(destination_y + y) * SCREEN_WIDTH + destination_x + x] = index;
        }
    }
    let replacement_planes = planar_brgi_planes(&indices)?;

    let [x, y, width, height] = manifest.composition_bounds;
    let byte_x = x / 8;
    let byte_width = width / 8;
    let row_bytes = SCREEN_WIDTH / 8;
    source_planes
        .iter()
        .zip(replacement_planes)
        .map(|(source, replacement)| {
            let writes = (y..y + height)
                .map(|row| {
                    let offset = row * row_bytes + byte_x;
                    FixedRangeExpectedWrite {
                        writer: "title composition renderer",
                        purpose: "owned TITLE.CNS composition row",
                        offset,
                        expected_source: source[offset..offset + byte_width].to_vec(),
                        replacement: replacement[offset..offset + byte_width].to_vec(),
                    }
                })
                .collect::<Vec<_>>();
            apply_fixed_range_expected_writes(source, &writes)
        })
        .collect()
}

fn planar_brgi_indices(planes: &[Vec<u8>]) -> Result<Vec<u8>> {
    ensure!(
        planes.len() == 4 && planes.iter().all(|plane| plane.len() == PLANE_BYTES),
        "B/R/G/I title source must contain four 640x400 planes"
    );
    let mut indices = vec![0_u8; SCREEN_WIDTH * SCREEN_HEIGHT];
    for y in 0..SCREEN_HEIGHT {
        for x in 0..SCREEN_WIDTH {
            let offset = y * (SCREEN_WIDTH / 8) + x / 8;
            let mask = 0x80 >> (x % 8);
            indices[y * SCREEN_WIDTH + x] = u8::from(planes[0][offset] & mask != 0)
                | (u8::from(planes[1][offset] & mask != 0) << 1)
                | (u8::from(planes[2][offset] & mask != 0) << 2)
                | (u8::from(planes[3][offset] & mask != 0) << 3);
        }
    }
    Ok(indices)
}

fn planar_brgi_planes(indices: &[u8]) -> Result<Vec<Vec<u8>>> {
    ensure!(
        indices.len() == SCREEN_WIDTH * SCREEN_HEIGHT && indices.iter().all(|index| *index < 16),
        "title composition contains invalid palette indices"
    );
    let mut planes = vec![vec![0_u8; PLANE_BYTES]; 4];
    for y in 0..SCREEN_HEIGHT {
        for x in 0..SCREEN_WIDTH {
            let index = indices[y * SCREEN_WIDTH + x];
            let offset = y * (SCREEN_WIDTH / 8) + x / 8;
            let mask = 0x80 >> (x % 8);
            for (plane_index, plane) in planes.iter_mut().enumerate() {
                if index & (1 << plane_index) != 0 {
                    plane[offset] |= mask;
                }
            }
        }
    }
    Ok(planes)
}

fn decode_title_composition(manifest: &TitleArtManifest) -> Result<Vec<Option<u8>>> {
    let decoder = png::Decoder::new(Cursor::new(local_input::read(TITLE_COMPOSITION_PATH)?));
    let mut reader = decoder
        .read_info()
        .context("decode title composition PNG header")?;
    let output_size = reader
        .output_buffer_size()
        .context("title composition PNG output size overflow")?;
    let mut bytes = vec![0_u8; output_size];
    let info = reader
        .next_frame(&mut bytes)
        .context("decode title composition PNG pixels")?;
    ensure!(
        info.width as usize == manifest.composition.width
            && info.height as usize == manifest.composition.height
            && info.color_type == png::ColorType::Rgba
            && info.bit_depth == png::BitDepth::Eight,
        "title composition PNG format differs from its manifest"
    );
    bytes[..info.buffer_size()]
        .chunks_exact(4)
        .map(|pixel| match pixel[3] {
            0 => Ok(None),
            255 => {
                let rgb = [pixel[0], pixel[1], pixel[2]];
                (0_u8..16)
                    .find(|index| pc98_review_color(*index) == rgb)
                    .context("title composition uses a color outside the PC-98 16-color palette")
                    .map(Some)
            }
            alpha => bail!("title composition uses unsupported alpha value {alpha}"),
        })
        .collect()
}

fn verify_protected_planar_pixels(
    original: &[Vec<u8>],
    updated: &[Vec<u8>],
    owned_bounds: [usize; 4],
) -> Result<()> {
    ensure!(
        original.len() == 4
            && updated.len() == 4
            && original.iter().all(|plane| plane.len() == PLANE_BYTES)
            && updated.iter().all(|plane| plane.len() == PLANE_BYTES),
        "title planar stream shape changed"
    );
    let [x, y, width, height] = owned_bounds;
    let byte_x = x / 8;
    let byte_width = width / 8;
    let row_bytes = SCREEN_WIDTH / 8;
    for (plane_index, (original_plane, updated_plane)) in original.iter().zip(updated).enumerate() {
        for row in 0..SCREEN_HEIGHT {
            for column in 0..row_bytes {
                if y <= row && row < y + height && byte_x <= column && column < byte_x + byte_width
                {
                    continue;
                }
                ensure!(
                    original_plane[row * row_bytes + column]
                        == updated_plane[row * row_bytes + column],
                    "title writer changed protected plane {plane_index} byte ({column}, {row})"
                );
            }
        }
    }
    Ok(())
}

fn rebuild_ending_graphic(
    payload: &mut BTreeMap<String, Vec<u8>>,
    ending: &GraphicTextResource,
) -> Result<GraphicTextResourceRebuildReport> {
    let changed_surfaces = translated_target_surfaces(ending)?;

    let original_packed = payload
        .get("ED7.CNS")
        .context("installer payload is missing ED7.CNS")?;
    let profile = GRAPHIC_PROFILES
        .iter()
        .find(|profile| profile.name == "ED7.CNS")
        .expect("ED7.CNS has a graphic profile");
    ensure!(
        sha256_hex(original_packed) == profile.packed_sha256,
        "ED7.CNS does not match the supported ending graphic resource"
    );
    let decoded = decode_exact_compile_lz(original_packed)
        .context("ED7.CNS is not an exact Compile-LZ resource")?;
    validate_decoded_streams(profile, &decoded.streams)?;
    let [original_decoded] = decoded.streams.as_slice() else {
        bail!("ED7.CNS must contain one decoded stream");
    };
    validate_monochrome_text_only_source(original_decoded, &ending.surfaces)?;

    let updated_decoded = rebuild_monochrome_text(original_decoded, &changed_surfaces)?;
    ensure!(
        updated_decoded != *original_decoded,
        "graphic text drafts did not change ED7.CNS"
    );
    verify_protected_monochrome_pixels(original_decoded, &updated_decoded, &changed_surfaces)?;

    let updated_packed = encode_compile_lz(&updated_decoded);
    let roundtrip = decode_exact_compile_lz(&updated_packed)
        .context("repacked ED7.CNS did not decode as exact Compile-LZ")?;
    ensure!(
        roundtrip.streams == [updated_decoded.clone()],
        "repacked ED7.CNS did not preserve the updated pixels"
    );
    let report = GraphicTextResourceRebuildReport {
        resource: "ED7.CNS".to_owned(),
        changed_surface_ids: changed_surfaces
            .iter()
            .map(|surface| surface.id.clone())
            .collect(),
        original_packed_sha256: sha256_hex(original_packed),
        original_decoded_sha256: vec![sha256_hex(original_decoded)],
        updated_packed_sha256: sha256_hex(&updated_packed),
        updated_decoded_sha256: vec![sha256_hex(&updated_decoded)],
        original_packed_size: original_packed.len(),
        updated_packed_size: updated_packed.len(),
        preserved_protected_pixels: true,
        roundtrip_decoded_pixels: true,
    };
    payload.insert("ED7.CNS".to_owned(), updated_packed);
    Ok(report)
}

fn validate_monochrome_text_only_source(
    screen: &[u8],
    surfaces: &[GraphicTextSurface],
) -> Result<()> {
    ensure!(
        screen.len() == PLANE_BYTES,
        "monochrome graphic must be one 640x400 plane"
    );
    let owned_bounds = surfaces
        .iter()
        .filter_map(|surface| surface.owned_bounds)
        .collect::<Vec<_>>();
    ensure!(
        !owned_bounds.is_empty(),
        "monochrome text resource has no declared pixel ownership"
    );
    let row_bytes = SCREEN_WIDTH / 8;
    for (offset, byte) in screen.iter().copied().enumerate() {
        if byte == 0 {
            continue;
        }
        let y = offset / row_bytes;
        let x = (offset % row_bytes) * 8;
        ensure!(
            owned_bounds
                .iter()
                .any(|bounds| byte_lies_in_rectangle(x, y, *bounds)),
            "monochrome source has a lit protected byte at ({x}, {y})"
        );
    }
    Ok(())
}

fn rebuild_monochrome_text(source: &[u8], surfaces: &[&GraphicTextSurface]) -> Result<Vec<u8>> {
    ensure!(
        source.len() == PLANE_BYTES,
        "monochrome graphic must be one 640x400 plane"
    );
    let mut replacement = source.to_vec();
    for surface in surfaces {
        let bounds = surface
            .owned_bounds
            .with_context(|| format!("{} has no pixel ownership", surface.id))?;
        clear_monochrome_rectangle(&mut replacement, bounds)?;
        draw_monochrome_text(&mut replacement, bounds, &surface.ko)
            .with_context(|| format!("failed to draw {}", surface.id))?;
    }

    let row_bytes = SCREEN_WIDTH / 8;
    let mut writes = Vec::new();
    for surface in surfaces {
        let [x, y, width, height] = surface
            .owned_bounds
            .expect("translated surface ownership was checked above");
        let byte_x = x / 8;
        let byte_width = width / 8;
        for row in y..y + height {
            let offset = row * row_bytes + byte_x;
            writes.push(FixedRangeExpectedWrite {
                writer: "graphic text renderer",
                purpose: "owned ED7.CNS text row",
                offset,
                expected_source: source[offset..offset + byte_width].to_vec(),
                replacement: replacement[offset..offset + byte_width].to_vec(),
            });
        }
    }
    apply_fixed_range_expected_writes(source, &writes)
}

fn clear_monochrome_rectangle(screen: &mut [u8], bounds: [usize; 4]) -> Result<()> {
    let [x, y, width, height] = bounds;
    ensure!(
        x.is_multiple_of(8)
            && width.is_multiple_of(8)
            && x + width <= SCREEN_WIDTH
            && y + height <= SCREEN_HEIGHT,
        "monochrome owned rectangle is not byte-aligned or lies outside the screen"
    );
    let row_bytes = SCREEN_WIDTH / 8;
    let byte_x = x / 8;
    let byte_width = width / 8;
    for row in y..y + height {
        screen[row * row_bytes + byte_x..row * row_bytes + byte_x + byte_width].fill(0);
    }
    Ok(())
}

fn draw_monochrome_text(screen: &mut [u8], bounds: [usize; 4], text: &str) -> Result<()> {
    let [x, y, width, height] = bounds;
    ensure!(!text.is_empty(), "monochrome graphic text is empty");
    ensure!(
        !text.ends_with('\n'),
        "monochrome graphic text has a trailing blank line"
    );
    let lines = text.lines().collect::<Vec<_>>();
    ensure!(
        lines.len() * GLYPH_HEIGHT <= height,
        "monochrome graphic text has too many lines for its owned rectangle"
    );
    for (line_index, line) in lines.iter().enumerate() {
        ensure!(
            line.chars().count() * GLYPH_WIDTH <= width,
            "monochrome graphic line does not fit its owned rectangle: {line:?}"
        );
        for (column, character) in line.chars().enumerate() {
            let glyph = rasterize_character(character)
                .with_context(|| format!("failed to rasterize graphic character {character:?}"))?;
            draw_monochrome_glyph(
                screen,
                x + column * GLYPH_WIDTH,
                y + line_index * GLYPH_HEIGHT,
                &glyph,
            );
        }
    }
    Ok(())
}

fn draw_monochrome_glyph(
    screen: &mut [u8],
    origin_x: usize,
    origin_y: usize,
    glyph: &[u8; GLYPH_BYTES],
) {
    let row_bytes = SCREEN_WIDTH / 8;
    for y in 0..GLYPH_HEIGHT {
        for byte_x in 0..GLYPH_WIDTH / 8 {
            screen[(origin_y + y) * row_bytes + origin_x / 8 + byte_x] |=
                glyph[y * (GLYPH_WIDTH / 8) + byte_x];
        }
    }
}

fn verify_protected_monochrome_pixels(
    original: &[u8],
    updated: &[u8],
    surfaces: &[&GraphicTextSurface],
) -> Result<()> {
    ensure!(
        original.len() == PLANE_BYTES && updated.len() == PLANE_BYTES,
        "monochrome graphic size changed"
    );
    let owned_bounds = surfaces
        .iter()
        .filter_map(|surface| surface.owned_bounds)
        .collect::<Vec<_>>();
    let row_bytes = SCREEN_WIDTH / 8;
    for offset in 0..PLANE_BYTES {
        let y = offset / row_bytes;
        let x = (offset % row_bytes) * 8;
        if owned_bounds
            .iter()
            .any(|bounds| byte_lies_in_rectangle(x, y, *bounds))
        {
            continue;
        }
        ensure!(
            original[offset] == updated[offset],
            "graphic text writer changed a protected byte at ({x}, {y})"
        );
    }
    Ok(())
}

fn byte_lies_in_rectangle(x: usize, y: usize, bounds: [usize; 4]) -> bool {
    let [left, top, width, height] = bounds;
    left <= x && x < left + width && top <= y && y < top + height
}

fn render_planar_brgi(planes: &[Vec<u8>]) -> Result<Vec<u8>> {
    ensure!(planes.len() == 4, "B/R/G/I rendering requires four planes");
    ensure!(
        planes.iter().all(|plane| plane.len() == PLANE_BYTES),
        "B/R/G/I plane size changed"
    );
    let mut rgb = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 3);
    for (((blue, red), green), intensity) in planes[0]
        .iter()
        .zip(&planes[1])
        .zip(&planes[2])
        .zip(&planes[3])
    {
        for mask in [0x80_u8, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
            let index = u8::from(blue & mask != 0)
                | (u8::from(red & mask != 0) << 1)
                | (u8::from(green & mask != 0) << 2)
                | (u8::from(intensity & mask != 0) << 3);
            rgb.extend_from_slice(&pc98_review_color(index));
        }
    }
    Ok(rgb)
}

fn render_event_sprite_atlas(decoded: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        decoded.len() == EVENT_DECODED_BYTES,
        "EVENT.CNS decoded size changed"
    );
    let mut rgb = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 3];

    for tile_index in 0..EVENT_TILE_END / TILE_BYTES {
        let start = tile_index * TILE_BYTES;
        render_plane_major_brgi_tile(&decoded[start..start + TILE_BYTES], tile_index, &mut rgb);
    }
    for frame in 0..(EVENT_SMALL_END - EVENT_TILE_END) / TILE_BYTES {
        let start = EVENT_TILE_END + frame * TILE_BYTES;
        render_row_interleaved_brgi(
            &decoded[start..start + TILE_BYTES],
            TILE_WIDTH,
            TILE_HEIGHT,
            frame * TILE_WIDTH,
            24,
            &mut rgb,
        )?;
    }
    for frame in 0..(EVENT_MEDIUM_END - EVENT_SMALL_END) / 0x200 {
        let start = EVENT_SMALL_END + frame * 0x200;
        render_row_interleaved_brgi(
            &decoded[start..start + 0x200],
            32,
            32,
            frame * 40,
            48,
            &mut rgb,
        )?;
    }
    render_row_interleaved_brgi(
        &decoded[EVENT_MEDIUM_END..EVENT_DECODED_BYTES],
        64,
        112,
        176,
        48,
        &mut rgb,
    )?;
    Ok(rgb)
}

fn indexed_tile_atlas_layout(tile_bank_offset: usize) -> &'static str {
    match tile_bank_offset {
        BATTLE_HEADER_BYTES => {
            "640x400 atlas of mapped 16x16 plane-major B/R/G/I source tiles after a 0x480-byte header"
        }
        DUNGEON_HEADER_BYTES => {
            "640x400 atlas of mapped 16x16 plane-major B/R/G/I source tiles after a 0xc00-byte header"
        }
        _ => unreachable!("indexed graphic profiles declare a known tile-bank offset"),
    }
}

fn render_mapped_tiles_and_descriptor_sprites(decoded: &[u8]) -> Result<Vec<u8>> {
    let tile_bank_word = decoded
        .get(..2)
        .context("mixed tile and sprite atlas is missing its tile-bank offset")?;
    ensure!(
        usize::from(u16::from_le_bytes([tile_bank_word[0], tile_bank_word[1]]))
            == FACE_TILE_BANK_OFFSET,
        "mixed tile and sprite atlas has a changed tile-bank offset"
    );
    ensure!(
        FACE_SPRITE_DESCRIPTOR_OFFSET + FACE_SPRITE_DESCRIPTOR_COUNT * 6 == FACE_MAP_OFFSET,
        "mixed tile and sprite atlas descriptor table does not end at its map"
    );
    let map_pointer = decoded
        .get(6..8)
        .context("mixed tile and sprite atlas is missing its map pointer")?;
    ensure!(
        usize::from(u16::from_le_bytes([map_pointer[0], map_pointer[1]])) == FACE_MAP_OFFSET,
        "mixed tile and sprite atlas has a changed map pointer"
    );

    let tiles = decoded
        .get(FACE_TILE_BANK_OFFSET..FACE_TILE_REGION_END)
        .context("mixed tile and sprite atlas has a truncated tile region")?;
    ensure!(
        tiles.len().is_multiple_of(TILE_BYTES),
        "mixed tile and sprite atlas ends its tile region with a partial tile"
    );
    let tile_count = tiles.len() / TILE_BYTES;
    ensure!(
        tile_count <= SCREEN_WIDTH / TILE_WIDTH * 4,
        "mixed tile and sprite atlas exceeds its four diagnostic tile rows"
    );

    let dimensions = decoded
        .get(FACE_MAP_OFFSET..FACE_MAP_OFFSET + 2)
        .context("mixed tile and sprite atlas has truncated map dimensions")?;
    let map_height = usize::from(dimensions[0]);
    let map_width = usize::from(dimensions[1]);
    ensure!(
        map_width > 0 && map_height > 0,
        "mixed tile and sprite atlas has an empty tile map"
    );
    let map_bytes = map_width
        .checked_mul(map_height)
        .and_then(|cells| cells.checked_mul(2))
        .context("mixed tile and sprite atlas map size overflow")?;
    let map_end = FACE_MAP_OFFSET
        .checked_add(2)
        .and_then(|offset| offset.checked_add(map_bytes))
        .context("mixed tile and sprite atlas map end overflow")?;
    ensure!(
        map_end <= FACE_TILE_BANK_OFFSET + TILE_BYTES,
        "mixed tile and sprite atlas map exceeds the reserved first tile"
    );
    let tile_words = decoded
        .get(FACE_MAP_OFFSET + 2..map_end)
        .context("mixed tile and sprite atlas has a truncated tile map")?;
    let mut selected_tiles = vec![false; tile_count];
    for word in tile_words.chunks_exact(2) {
        let tile_word = u16::from_le_bytes([word[0], word[1]]);
        ensure!(
            tile_word & 0x7800 == 0,
            "mixed tile and sprite atlas uses an unsupported tile flag"
        );
        let tile_index =
            usize::from(tile_word & 0x00ff) + usize::from((tile_word >> 8) & 0x0007) * 256;
        let selected = selected_tiles
            .get_mut(tile_index)
            .with_context(|| format!("mixed atlas map selects missing tile {tile_index}"))?;
        *selected = true;
    }
    ensure!(
        !selected_tiles[0] && selected_tiles[1..].iter().all(|selected| *selected),
        "mixed tile and sprite atlas map does not select every non-reserved source tile"
    );

    let mut rgb = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 3];
    for tile_index in 1..tile_count {
        let start = tile_index * TILE_BYTES;
        render_plane_major_brgi_tile(&tiles[start..start + TILE_BYTES], tile_index - 1, &mut rgb);
    }

    let mut expected_source_offset = FACE_TILE_REGION_END;
    let mut destination_x = 0_usize;
    let mut destination_y = 80_usize;
    let mut previous_dimensions = None;
    for descriptor_index in 0..FACE_SPRITE_DESCRIPTOR_COUNT {
        let descriptor_offset = FACE_SPRITE_DESCRIPTOR_OFFSET + descriptor_index * 6;
        let descriptor = decoded
            .get(descriptor_offset..descriptor_offset + 6)
            .context("mixed tile and sprite atlas has a truncated sprite descriptor")?;
        let width_bytes = usize::from(descriptor[0]);
        let width = width_bytes
            .checked_mul(8)
            .context("mixed tile and sprite atlas width overflow")?;
        let height = usize::from(descriptor[1]);
        ensure!(
            width > 0 && height > 0,
            "mixed tile and sprite atlas descriptor {descriptor_index} has an empty dimension"
        );
        let source_offset = usize::from(u16::from_le_bytes([descriptor[4], descriptor[5]]));
        ensure!(
            source_offset == expected_source_offset,
            "mixed tile and sprite atlas descriptor {descriptor_index} leaves a source gap"
        );
        let source_bytes = width_bytes
            .checked_mul(height)
            .and_then(|bytes| bytes.checked_mul(4))
            .context("mixed tile and sprite atlas sprite size overflow")?;
        let source_end = source_offset
            .checked_add(source_bytes)
            .context("mixed tile and sprite atlas sprite end overflow")?;
        let source = decoded
            .get(source_offset..source_end)
            .context("mixed tile and sprite atlas has a truncated sprite")?;

        if let Some((previous_width, previous_height)) = previous_dimensions
            && (width, height) != (previous_width, previous_height)
        {
            destination_x = 0;
            destination_y = destination_y
                .checked_add(
                    previous_height
                        .checked_add(8)
                        .context("mixed tile and sprite atlas diagnostic row gap overflow")?,
                )
                .context("mixed tile and sprite atlas diagnostic position overflow")?;
        }
        let destination_right = destination_x
            .checked_add(width)
            .context("mixed tile and sprite atlas diagnostic width overflow")?;
        let destination_bottom = destination_y
            .checked_add(height)
            .context("mixed tile and sprite atlas diagnostic height overflow")?;
        ensure!(
            destination_right <= SCREEN_WIDTH && destination_bottom <= SCREEN_HEIGHT,
            "mixed tile and sprite atlas exceeds the diagnostic sheet"
        );
        render_row_interleaved_brgi(
            source,
            width,
            height,
            destination_x,
            destination_y,
            &mut rgb,
        )?;
        destination_x = destination_right
            .checked_add(8)
            .context("mixed tile and sprite atlas diagnostic column gap overflow")?;
        previous_dimensions = Some((width, height));
        expected_source_offset = source_end;
    }
    ensure!(
        expected_source_offset == decoded.len(),
        "mixed tile and sprite atlas has unconsumed bytes after its descriptors"
    );
    Ok(rgb)
}

fn render_indexed_plane_major_brgi_tile_atlas(
    decoded: &[u8],
    tile_bank_offset: usize,
    map_entry_count: usize,
) -> Result<Vec<u8>> {
    let header_word = decoded
        .get(..2)
        .context("mapped tile atlas is missing its tile-bank offset")?;
    ensure!(
        usize::from(u16::from_le_bytes([header_word[0], header_word[1]])) == tile_bank_offset,
        "mapped tile-atlas header size differs from its consumer-selected tile-bank offset"
    );
    let tiles = decoded
        .get(tile_bank_offset..)
        .context("mapped tile-atlas header exceeds the decoded resource")?;
    ensure!(
        tiles.len().is_multiple_of(TILE_BYTES),
        "mapped tile-atlas bank ends with a partial 16x16 B/R/G/I tile"
    );
    let tile_count = tiles.len() / TILE_BYTES;
    ensure!(
        tile_count <= SCREEN_WIDTH / TILE_WIDTH * (SCREEN_HEIGHT / TILE_HEIGHT),
        "mapped tile-atlas bank exceeds the 640x400 diagnostic sheet"
    );

    let map_table_end = 4_usize
        .checked_add(
            map_entry_count
                .checked_mul(4)
                .context("map-table size overflow")?,
        )
        .context("map-table offset overflow")?;
    let mut map_offsets = BTreeSet::new();
    for map_entry_index in 0..map_entry_count {
        let pointer_offset = 6 + map_entry_index * 4;
        let pointer = decoded
            .get(pointer_offset..pointer_offset + 2)
            .context("mapped tile atlas has a truncated map pointer")?;
        let map_offset = usize::from(u16::from_le_bytes([pointer[0], pointer[1]]));
        ensure!(
            map_offset >= map_table_end,
            "mapped tile-atlas entry {map_entry_index} points inside its map table"
        );
        map_offsets.insert(map_offset);
    }

    let mut previous_map_end = map_table_end;
    let mut selected_tiles = vec![false; tile_count];
    for (map_index, map_offset) in map_offsets.into_iter().enumerate() {
        ensure!(
            map_offset == previous_map_end,
            "mapped tile-atlas unique maps are not contiguous at map {map_index}"
        );
        let dimensions = decoded
            .get(map_offset..map_offset + 2)
            .context("mapped tile atlas has truncated map dimensions")?;
        let height = usize::from(dimensions[0]);
        let width = usize::from(dimensions[1]);
        ensure!(
            (width == 0) == (height == 0),
            "mapped tile-atlas map {map_index} has only one empty dimension"
        );
        let map_bytes = width
            .checked_mul(height)
            .and_then(|cells| cells.checked_mul(2))
            .context("mapped tile-atlas map size overflow")?;
        let map_end = map_offset
            .checked_add(2)
            .and_then(|offset| offset.checked_add(map_bytes))
            .context("mapped tile-atlas map end overflow")?;
        let tile_words = decoded
            .get(map_offset + 2..map_end)
            .context("mapped tile atlas has a truncated tile map")?;
        for word in tile_words.chunks_exact(2) {
            let tile_word = u16::from_le_bytes([word[0], word[1]]);
            ensure!(
                tile_word & 0x7800 == 0,
                "mapped tile-atlas map {map_index} uses an unsupported tile flag"
            );
            let tile_index =
                usize::from(tile_word & 0x00ff) + usize::from((tile_word >> 8) & 0x0007) * 256;
            let selected = selected_tiles
                .get_mut(tile_index)
                .with_context(|| format!("map {map_index} selects missing tile {tile_index}"))?;
            *selected = true;
        }
        previous_map_end = map_end;
    }
    ensure!(
        previous_map_end <= tile_bank_offset + TILE_BYTES,
        "mapped tile-atlas maps exceed the header and reserved first tile"
    );
    ensure!(
        selected_tiles.iter().all(|selected| *selected),
        "mapped tile-atlas maps do not cover every source tile"
    );

    let mut rgb = vec![0; SCREEN_WIDTH * SCREEN_HEIGHT * 3];
    for (tile_index, tile) in tiles.chunks_exact(TILE_BYTES).enumerate() {
        render_plane_major_brgi_tile(tile, tile_index, &mut rgb);
    }
    Ok(rgb)
}

fn render_plane_major_brgi_tile(tile: &[u8], tile_index: usize, rgb: &mut [u8]) {
    let tiles_per_row = SCREEN_WIDTH / TILE_WIDTH;
    let destination_x = tile_index % tiles_per_row * TILE_WIDTH;
    let destination_y = tile_index / tiles_per_row * TILE_HEIGHT;
    for row in 0..TILE_HEIGHT {
        for column in 0..TILE_WIDTH {
            let byte_in_row = column / 8;
            let mask = 0x80 >> (column % 8);
            let mut color = 0_u8;
            for plane in 0..4 {
                let offset = plane * TILE_PLANE_BYTES + row * 2 + byte_in_row;
                color |= u8::from(tile[offset] & mask != 0) << plane;
            }
            let pixel = (destination_y + row) * SCREEN_WIDTH + destination_x + column;
            rgb[pixel * 3..pixel * 3 + 3].copy_from_slice(&pc98_review_color(color));
        }
    }
}

fn render_row_interleaved_brgi(
    source: &[u8],
    width: usize,
    height: usize,
    destination_x: usize,
    destination_y: usize,
    rgb: &mut [u8],
) -> Result<()> {
    ensure!(
        width > 0 && width.is_multiple_of(8) && height > 0,
        "row-interleaved sprite dimensions must be positive and byte aligned"
    );
    let plane_row_bytes = width / 8;
    let source_row_bytes = plane_row_bytes * 4;
    ensure!(
        source.len() == source_row_bytes * height,
        "row-interleaved sprite byte count changed"
    );
    ensure!(
        destination_x + width <= SCREEN_WIDTH && destination_y + height <= SCREEN_HEIGHT,
        "row-interleaved sprite exceeds the diagnostic atlas"
    );

    for row in 0..height {
        for column in 0..width {
            let byte_in_row = column / 8;
            let mask = 0x80 >> (column % 8);
            let mut color = 0_u8;
            for plane in 0..4 {
                let offset = row * source_row_bytes + plane * plane_row_bytes + byte_in_row;
                color |= u8::from(source[offset] & mask != 0) << plane;
            }
            let pixel = (destination_y + row) * SCREEN_WIDTH + destination_x + column;
            rgb[pixel * 3..pixel * 3 + 3].copy_from_slice(&pc98_review_color(color));
        }
    }
    Ok(())
}

fn transpose_column_major_plane(column_major: &[u8]) -> Vec<u8> {
    let mut row_major = vec![0; PLANE_BYTES];
    for column in 0..SCREEN_WIDTH / 8 {
        for row in 0..SCREEN_HEIGHT {
            row_major[row * (SCREEN_WIDTH / 8) + column] =
                column_major[column * SCREEN_HEIGHT + row];
        }
    }
    row_major
}

fn render_monochrome(plane: &[u8]) -> Result<Vec<u8>> {
    ensure!(plane.len() == PLANE_BYTES, "monochrome plane size changed");
    let mut rgb = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 3);
    for byte in plane {
        for mask in [0x80_u8, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01] {
            let value = if byte & mask == 0 { 0 } else { 255 };
            rgb.extend_from_slice(&[value, value, value]);
        }
    }
    Ok(rgb)
}

fn pc98_review_color(index: u8) -> [u8; 3] {
    let intensity = if index & 0x08 == 0 { 0 } else { 0x55 };
    let channel = |mask| {
        if index & mask == 0 {
            intensity
        } else {
            0xaa + intensity
        }
    };
    [channel(0x02), channel(0x04), channel(0x01)]
}

fn encode_rgb_bmp(rgb: &[u8]) -> Result<Vec<u8>> {
    let row_bytes = SCREEN_WIDTH * 3;
    ensure!(
        rgb.len() == row_bytes * SCREEN_HEIGHT,
        "RGB review image size changed"
    );
    let pixel_bytes = row_bytes * SCREEN_HEIGHT;
    let file_bytes = 54usize
        .checked_add(pixel_bytes)
        .context("BMP file size overflow")?;
    let mut bmp = Vec::with_capacity(file_bytes);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&u32::try_from(file_bytes)?.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54_u32.to_le_bytes());
    bmp.extend_from_slice(&40_u32.to_le_bytes());
    bmp.extend_from_slice(&u32::try_from(SCREEN_WIDTH)?.to_le_bytes());
    bmp.extend_from_slice(&u32::try_from(SCREEN_HEIGHT)?.to_le_bytes());
    bmp.extend_from_slice(&1_u16.to_le_bytes());
    bmp.extend_from_slice(&24_u16.to_le_bytes());
    bmp.extend_from_slice(&0_u32.to_le_bytes());
    bmp.extend_from_slice(&u32::try_from(pixel_bytes)?.to_le_bytes());
    bmp.extend_from_slice(&[0; 16]);

    for row in rgb.chunks_exact(row_bytes).rev() {
        for pixel in row.chunks_exact(3) {
            bmp.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    ensure!(bmp.len() == file_bytes, "BMP encoder size mismatch");
    Ok(bmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translated_surface(id: &str, owned_bounds: [usize; 4], ko: &str) -> GraphicTextSurface {
        GraphicTextSurface {
            id: id.to_owned(),
            source_transcription: "fixture".to_owned(),
            source_transcription_status: "needs_human_review".to_owned(),
            review_bounds: owned_bounds,
            owned_bounds: Some(owned_bounds),
            disposition: GraphicTextDisposition::Target,
            ko: ko.to_owned(),
            status: "needs_human_review".to_owned(),
            notes: String::new(),
        }
    }

    #[test]
    fn planar_renderer_preserves_brgi_bit_roles() {
        let mut planes = vec![vec![0; PLANE_BYTES]; 4];
        planes[0][0] = 0x80;
        planes[1][0] = 0x40;
        planes[2][0] = 0x20;
        planes[3][0] = 0x10;

        let rgb = render_planar_brgi(&planes).unwrap();

        assert_eq!(&rgb[0..3], &[0x00, 0x00, 0xaa]);
        assert_eq!(&rgb[3..6], &[0xaa, 0x00, 0x00]);
        assert_eq!(&rgb[6..9], &[0x00, 0xaa, 0x00]);
        assert_eq!(&rgb[9..12], &[0x55, 0x55, 0x55]);
    }

    #[test]
    fn planar_index_conversion_roundtrips_synthetic_palette_roles() {
        let mut indices = vec![0_u8; SCREEN_WIDTH * SCREEN_HEIGHT];
        indices[..6].copy_from_slice(&[1, 2, 4, 8, 15, 0]);

        let planes = planar_brgi_planes(&indices).unwrap();
        let roundtrip = planar_brgi_indices(&planes).unwrap();

        assert_eq!(roundtrip, indices);
    }

    #[test]
    fn planar_protection_rejects_a_change_outside_composition_ownership() {
        let original = vec![vec![0_u8; PLANE_BYTES]; 4];
        let mut owned_change = original.clone();
        owned_change[0][0] = 0x80;
        verify_protected_planar_pixels(&original, &owned_change, [0, 0, 8, 1]).unwrap();

        let mut protected_change = owned_change;
        protected_change[0][1] = 0x80;
        let error =
            verify_protected_planar_pixels(&original, &protected_change, [0, 0, 8, 1]).unwrap_err();

        assert!(error.to_string().contains("protected plane"));
    }

    #[test]
    fn column_major_plane_transposes_columns_into_scanlines() {
        let mut column_major = vec![0; PLANE_BYTES];
        column_major[SCREEN_HEIGHT + 2] = 0xaa;

        let row_major = transpose_column_major_plane(&column_major);

        assert_eq!(row_major[2 * (SCREEN_WIDTH / 8) + 1], 0xaa);
        assert_eq!(row_major.iter().filter(|byte| **byte != 0).count(), 1);
    }

    #[test]
    fn event_renderer_preserves_all_consumer_storage_orders() {
        let mut decoded = vec![0; EVENT_DECODED_BYTES];
        decoded[0] = 0x80;
        decoded[EVENT_TILE_END] = 0x80;
        decoded[EVENT_SMALL_END + 4] = 0x80;
        decoded[EVENT_MEDIUM_END + 16] = 0x80;

        let rgb = render_event_sprite_atlas(&decoded).unwrap();

        assert_eq!(&rgb[0..3], &[0x00, 0x00, 0xaa]);
        let small = 24 * SCREEN_WIDTH * 3;
        assert_eq!(&rgb[small..small + 3], &[0x00, 0x00, 0xaa]);
        let medium = 48 * SCREEN_WIDTH * 3;
        assert_eq!(&rgb[medium..medium + 3], &[0xaa, 0x00, 0x00]);
        let large = (48 * SCREEN_WIDTH + 176) * 3;
        assert_eq!(&rgb[large..large + 3], &[0x00, 0xaa, 0x00]);
    }

    #[test]
    fn mapped_tile_atlas_requires_every_source_tile_to_be_selected() {
        let header_bytes = 0x10;
        let mut decoded = vec![0; header_bytes + TILE_BYTES];
        decoded[..2].copy_from_slice(&(header_bytes as u16).to_le_bytes());
        decoded[6..8].copy_from_slice(&8_u16.to_le_bytes());
        decoded[8..12].copy_from_slice(&[1, 1, 0, 0]);
        decoded[header_bytes] = 0x80;
        decoded[header_bytes + TILE_PLANE_BYTES] = 0x40;
        decoded[header_bytes + TILE_PLANE_BYTES * 2] = 0x20;
        decoded[header_bytes + TILE_PLANE_BYTES * 3] = 0x10;

        let rgb = render_indexed_plane_major_brgi_tile_atlas(&decoded, header_bytes, 1).unwrap();

        assert_eq!(&rgb[0..3], &[0x00, 0x00, 0xaa]);
        assert_eq!(&rgb[3..6], &[0xaa, 0x00, 0x00]);
        assert_eq!(&rgb[6..9], &[0x00, 0xaa, 0x00]);
        assert_eq!(&rgb[9..12], &[0x55, 0x55, 0x55]);
    }

    #[test]
    fn indexed_tile_atlas_accepts_reused_and_empty_map_entries() {
        let tile_bank_offset = 0x20;
        let mut decoded = vec![0; tile_bank_offset + TILE_BYTES];
        decoded[..2].copy_from_slice(&(tile_bank_offset as u16).to_le_bytes());
        decoded[6..8].copy_from_slice(&16_u16.to_le_bytes());
        decoded[10..12].copy_from_slice(&20_u16.to_le_bytes());
        decoded[14..16].copy_from_slice(&16_u16.to_le_bytes());
        decoded[16..20].copy_from_slice(&[1, 1, 0, 0]);

        render_indexed_plane_major_brgi_tile_atlas(&decoded, tile_bank_offset, 3).unwrap();
    }

    #[test]
    fn descriptor_sprite_atlas_requires_contiguous_source_ranges() {
        let mut decoded = vec![0; FACE_TILE_REGION_END + FACE_SPRITE_DESCRIPTOR_COUNT * 4];
        decoded[..2].copy_from_slice(&(FACE_TILE_BANK_OFFSET as u16).to_le_bytes());
        decoded[6..8].copy_from_slice(&(FACE_MAP_OFFSET as u16).to_le_bytes());
        decoded[FACE_MAP_OFFSET..FACE_MAP_OFFSET + 2].copy_from_slice(&[13, 14]);
        for cell in 0..13 * 14 {
            let tile_index = cell % 130 + 1;
            let tile_word = 0x8000 | u16::try_from(tile_index).unwrap();
            let offset = FACE_MAP_OFFSET + 2 + cell * 2;
            decoded[offset..offset + 2].copy_from_slice(&tile_word.to_le_bytes());
        }
        for descriptor_index in 0..FACE_SPRITE_DESCRIPTOR_COUNT {
            let descriptor_offset = FACE_SPRITE_DESCRIPTOR_OFFSET + descriptor_index * 6;
            decoded[descriptor_offset..descriptor_offset + 2].copy_from_slice(&[1, 1]);
            let source_offset = FACE_TILE_REGION_END + descriptor_index * 4;
            decoded[descriptor_offset + 4..descriptor_offset + 6]
                .copy_from_slice(&(source_offset as u16).to_le_bytes());
        }

        render_mapped_tiles_and_descriptor_sprites(&decoded).unwrap();

        decoded[18..20].copy_from_slice(&((FACE_TILE_REGION_END + 5) as u16).to_le_bytes());
        assert!(render_mapped_tiles_and_descriptor_sprites(&decoded).is_err());
    }

    #[test]
    fn bmp_encoder_writes_a_bottom_up_24_bit_image() {
        let rgb = vec![0_u8; SCREEN_WIDTH * SCREEN_HEIGHT * 3];

        let bmp = encode_rgb_bmp(&rgb).unwrap();

        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[18..22].try_into().unwrap()), 640);
        assert_eq!(u32::from_le_bytes(bmp[22..26].try_into().unwrap()), 400);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 24);
        assert_eq!(bmp.len(), 54 + SCREEN_WIDTH * SCREEN_HEIGHT * 3);
    }

    #[test]
    #[ignore = "requires assets/fonts/Galmuri14.ttf"]
    fn monochrome_text_rebuild_changes_only_the_owned_rectangle() {
        let mut source = vec![0_u8; PLANE_BYTES];
        source[0] = 0xff;
        let protected_offset = (SCREEN_HEIGHT - 1) * (SCREEN_WIDTH / 8);
        source[protected_offset] = 0x80;
        let surface = translated_surface("fixture text", [0, 0, 16, 16], "가");

        let updated = rebuild_monochrome_text(&source, &[&surface]).unwrap();

        assert_ne!(&updated[..2], &source[..2]);
        assert_eq!(updated[protected_offset], source[protected_offset]);
        verify_protected_monochrome_pixels(&source, &updated, &[&surface]).unwrap();
    }

    #[test]
    fn monochrome_text_rebuild_rejects_a_line_outside_its_owned_rectangle() {
        let source = vec![0_u8; PLANE_BYTES];
        let surface = translated_surface("fixture text", [0, 0, 16, 16], "가나");

        let error = rebuild_monochrome_text(&source, &[&surface]).unwrap_err();

        assert!(format!("{error:#}").contains("does not fit"));
    }

    #[test]
    fn text_only_source_contract_rejects_lit_protected_pixels() {
        let mut source = vec![0_u8; PLANE_BYTES];
        source[2] = 0x80;
        let surface = translated_surface("fixture text", [0, 0, 16, 16], "가");

        let error = validate_monochrome_text_only_source(&source, &[surface]).unwrap_err();

        assert!(error.to_string().contains("lit protected byte"));
    }
}
