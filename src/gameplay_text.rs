use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};
use v30::{CallTarget, Instruction, Operand, Register16, decode_bytes};

use crate::catalog_bundle::{read_catalog_json, resolve_catalog_part_path};
use crate::compile_lz::{decode_exact_compile_lz, encode_compile_lz};
use crate::lha_sfx::extract_lha_sfx;
use crate::main_text::{ensure_message_avoids_renderer_sheet_codes, renderer_control_sequences};
use crate::renderer_font::{encode_renderer_text, is_modern_hangul};
use crate::source_disk::{load_verified_source, sha256_hex};

const CATALOG_SCHEMA: &str = "pc98_madou_docho.gameplay_text_catalog";
const CATALOG_BUNDLE_SCHEMA: &str = "pc98_madou_docho.gameplay_text_catalog_bundle";
const RESOURCE_FILE_SCHEMA: &str = "pc98_madou_docho.gameplay_text_resource";
const UNTRANSLATED_STATUS: &str = "untranslated";

struct ResourceProfile {
    name: &'static str,
    packed_sha256: &'static str,
    decoded_sha256: &'static str,
    decoded_size: usize,
}

const RESOURCE_PROFILES: [ResourceProfile; 16] = [
    ResourceProfile {
        name: "S01.DAT",
        packed_sha256: "6a263b50981fc79769322e000407602a98093ffb2cfb26dfa6c8d070e92f8711",
        decoded_sha256: "74e7ae96be2cbba6a16274b5395be07567a928cadc2e799b893f68a357a2cf28",
        decoded_size: 15_014,
    },
    ResourceProfile {
        name: "S02.DAT",
        packed_sha256: "d97af65118168e93085fc155963673c4fc3e2522f2a5d754943db918258a5c12",
        decoded_sha256: "fad5a5323e3e8a1244d9139c969c385a799b131abaa50f1233d2332e41a8bddd",
        decoded_size: 15_058,
    },
    ResourceProfile {
        name: "ENEMY01.DAT",
        packed_sha256: "604a0fbb16900612e8d17f3255412d73afae370dd4906ed5420767bb5ec7a446",
        decoded_sha256: "d0e2ed1ddf21474157c69b6c50ac66894206d975428194a4d7ff4b6d6d216f6c",
        decoded_size: 10_448,
    },
    ResourceProfile {
        name: "ENEMY02.DAT",
        packed_sha256: "dd68f50d9dbbbe9103bfc65e06bc272a9b87fb39d108e43be810b60bf62c5cf9",
        decoded_sha256: "f038c11bff90ddc52e9c3ae9ad981492d65157bbb2d0cef269c37e1f62ef6f6f",
        decoded_size: 21_711,
    },
    ResourceProfile {
        name: "ENEMY03.DAT",
        packed_sha256: "a6f76b9359c47cd43f85f9b3911f3c9c9e824ffb11b8c3cf12d39f3d03165c2b",
        decoded_sha256: "1024cc42ddb3c223a9409f065722f28d5963e98b97128ed6adfe2e2847bdfb35",
        decoded_size: 16_458,
    },
    ResourceProfile {
        name: "ENEMY04.DAT",
        packed_sha256: "cb20cac70e04ab84a3e1518fbe625cd54ce502440bb8f349561e7145dc2daf20",
        decoded_sha256: "677467fa86fd6a2b6ff151577c73898c357d4ad1a6f56552ceb211e935dd896d",
        decoded_size: 24_787,
    },
    ResourceProfile {
        name: "ENEMY05.DAT",
        packed_sha256: "b19e5621d025490dede0821e38ebc8226212237d49d0dd31a110c323bd7fad36",
        decoded_sha256: "9b08ca3e56bbc40dd30fd9fb31079bd194dc9c761afd4ce71f4580430c0b34c0",
        decoded_size: 25_410,
    },
    ResourceProfile {
        name: "ENEMY06.DAT",
        packed_sha256: "8693ac13d49773cfaaaf08109444d18db415c1fede05e08af34b172dfe11a19e",
        decoded_sha256: "88cb2aa9b7b1f5c08bc0bd947578c201784c52d6c0fcfe3fefc31b3a2644dccb",
        decoded_size: 22_032,
    },
    ResourceProfile {
        name: "ENEMY07.DAT",
        packed_sha256: "bf1fd9a4276ae1c677fa76cd5f8b5da466b2a8c2933f9c0b38a434d17517b06a",
        decoded_sha256: "07070820c1c3b0eb414cb100d1a71a60b5e8b77fec97d617d6fa3bb34da65d74",
        decoded_size: 22_329,
    },
    ResourceProfile {
        name: "ENEMY08.DAT",
        packed_sha256: "ed67d68f89235cc4f5658a868159865075c80158fdeaf50c0be2c7de6223aca9",
        decoded_sha256: "7f9d305a9af5800a07338fe8df33c5222bc2fffcd7c09037d7d7dc05635abdc0",
        decoded_size: 23_740,
    },
    ResourceProfile {
        name: "ENEMY09.DAT",
        packed_sha256: "e58da7dbeb3e31dda4f889857b364f068867afb5662587fd940c68ca80fca58c",
        decoded_sha256: "0982a671fa63196ee56d31ff48ad01d5d9e1ce5a5c2d59d5690503194bbdda92",
        decoded_size: 31_093,
    },
    ResourceProfile {
        name: "ENEMY0A.DAT",
        packed_sha256: "708a5ced0a340681736b3fd8d05ff6e99454a5bb75d8a972004b110bfe11162a",
        decoded_sha256: "6b2b26065ae3fe8b5d018d57b3de3b894793c888481e2ca60b7c496fc4d6766e",
        decoded_size: 19_174,
    },
    ResourceProfile {
        name: "ENEMY0B.DAT",
        packed_sha256: "136ac8d3c6999487d959f1442d59c80171e983a06f6cc7fba8fd9824d1dc1198",
        decoded_sha256: "95f538d9d487d376b97426cd639475b5c5059b0a6d313142e7b39ae4cec8cf40",
        decoded_size: 37_644,
    },
    ResourceProfile {
        name: "ENEMY0C.DAT",
        packed_sha256: "f80df86adbf38fd200cbebf207ff1a07ca97f3785d38502feea8f86560ed81bf",
        decoded_sha256: "f34e1a5619d46b04726ef9939472bf19657c793e9f1ef45073563e3e22820cd8",
        decoded_size: 24_288,
    },
    ResourceProfile {
        name: "ENEMY0D.DAT",
        packed_sha256: "d0030c5135eec5b207cd1c66aa13b0794cdb86a18250581c5b8cf011e9496bbf",
        decoded_sha256: "c66f1850d221f609ab03edc5622992ec6000e2d840a04f91859d6a8323797bae",
        decoded_size: 23_264,
    },
    ResourceProfile {
        name: "ENEMY0E.DAT",
        packed_sha256: "7a394600af3c3a76146b1d2f2ee09d310722403b7921f3b95c305688356a51c0",
        decoded_sha256: "6a6960407d25360ef73c457ce37a094554b2bb1eb45d77e5292096159fd83bba",
        decoded_size: 14_832,
    },
];

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameplayTextCatalog {
    pub schema: String,
    pub resource_count: usize,
    pub entry_count: usize,
    pub reference_count: usize,
    pub unresolved_entry_count: usize,
    pub resources: Vec<GameplayTextResource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GameplayTextCatalogBundle {
    schema: String,
    catalog: GameplayTextCatalog,
    resource_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GameplayTextResourceFile {
    schema: String,
    resource: GameplayTextResource,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameplayTextResource {
    pub name: String,
    pub source_packed_sha256: String,
    pub source_decoded_sha256: String,
    pub decoded_size: usize,
    pub text_pool_start: usize,
    pub text_pool_end: usize,
    pub entry_count: usize,
    pub reference_count: usize,
    pub unresolved_entry_count: usize,
    pub entries: Vec<GameplayTextEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameplayTextEntry {
    pub id: String,
    pub string_decoded_offset: usize,
    pub byte_budget: usize,
    pub raw_hex: String,
    pub source_text: String,
    pub references: Vec<GameplayTextReference>,
    pub ko: String,
    pub status: String,
    pub notes: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GameplayTextReference {
    MovSiThenNearCall {
        instruction_decoded_offset: usize,
        target_word_decoded_offset: usize,
        call_decoded_offset: usize,
        renderer_decoded_offset: usize,
    },
    ShopMessageTable {
        table_decoded_offset: usize,
        table_index: usize,
        target_word_decoded_offset: usize,
    },
    ContiguousTextPool {
        previous_string_decoded_offset: usize,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GameplayTextCatalogValidation {
    pub resource_count: usize,
    pub entry_count: usize,
    pub reference_count: usize,
    pub unresolved_entry_count: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GameplayTextRebuildReport {
    pub changed_resources: usize,
    pub changed_entries: usize,
    pub relocated_entries: usize,
    pub resources: Vec<GameplayTextResourceRebuildReport>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct GameplayTextResourceRebuildReport {
    pub name: String,
    pub changed_entries: usize,
    pub relocated_entries: usize,
    pub pool_capacity: usize,
    pub pool_used: usize,
    pub original_packed_sha256: String,
    pub updated_packed_sha256: String,
    pub original_decoded_sha256: String,
    pub updated_decoded_sha256: String,
    pub original_packed_size: usize,
    pub updated_packed_size: usize,
    pub decoded_size: usize,
    pub exact_decode_roundtrip: bool,
}

pub fn extract_gameplay_text_catalog(source_path: &Path) -> Result<GameplayTextCatalog> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    extract_gameplay_text_catalog_from_payload(&payload)
}

fn extract_gameplay_text_catalog_from_payload(
    payload: &BTreeMap<String, Vec<u8>>,
) -> Result<GameplayTextCatalog> {
    let resources = RESOURCE_PROFILES
        .iter()
        .map(|profile| extract_resource(payload, profile))
        .collect::<Result<Vec<_>>>()?;
    let entry_count = resources.iter().map(|resource| resource.entry_count).sum();
    let reference_count = resources
        .iter()
        .map(|resource| resource.reference_count)
        .sum();
    Ok(GameplayTextCatalog {
        schema: CATALOG_SCHEMA.to_owned(),
        resource_count: resources.len(),
        entry_count,
        reference_count,
        unresolved_entry_count: 0,
        resources,
    })
}

pub fn write_gameplay_text_catalog(
    source_path: &Path,
    output_path: &Path,
) -> Result<GameplayTextCatalog> {
    let catalog = extract_gameplay_text_catalog(source_path)?;
    let encoded = serde_json::to_vec_pretty(&catalog).context("serialize gameplay text catalog")?;
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create gameplay text catalog directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(output_path, encoded).with_context(|| {
        format!(
            "failed to write gameplay text catalog: {}",
            output_path.display()
        )
    })?;
    Ok(catalog)
}

pub fn validate_gameplay_text_catalog(
    source_path: &Path,
    catalog_path: &Path,
) -> Result<GameplayTextCatalogValidation> {
    let source = load_verified_source(source_path)?;
    let payload = extract_lha_sfx(&source.installer)?;
    let expected = extract_gameplay_text_catalog_from_payload(&payload)?;
    let supplied = read_gameplay_text_catalog(catalog_path)?;
    validate_protected_gameplay_catalog(&expected, &supplied)?;
    Ok(GameplayTextCatalogValidation {
        resource_count: expected.resource_count,
        entry_count: expected.entry_count,
        reference_count: expected.reference_count,
        unresolved_entry_count: expected.unresolved_entry_count,
    })
}

pub(crate) fn gameplay_text_hangul_characters(catalog_path: &Path) -> Result<BTreeSet<char>> {
    let catalog = read_gameplay_text_catalog(catalog_path)?;
    validate_translation_states(&catalog)?;
    Ok(catalog
        .resources
        .iter()
        .flat_map(|resource| &resource.entries)
        .flat_map(|entry| entry.ko.chars())
        .filter(|character| is_modern_hangul(*character))
        .collect())
}

pub(crate) fn apply_gameplay_text_drafts(
    payload: &mut BTreeMap<String, Vec<u8>>,
    catalog_path: &Path,
    codebook: &BTreeMap<char, [u8; 2]>,
) -> Result<GameplayTextRebuildReport> {
    let expected = extract_gameplay_text_catalog_from_payload(payload)?;
    let supplied = read_gameplay_text_catalog(catalog_path)?;
    validate_protected_gameplay_catalog(&expected, &supplied)?;

    let mut reports = Vec::with_capacity(supplied.resources.len());
    for resource in &supplied.resources {
        let original_packed = payload
            .get(&resource.name)
            .with_context(|| format!("installer payload is missing {}", resource.name))?
            .clone();
        let decoded = decode_exact_compile_lz(&original_packed)
            .with_context(|| format!("{} is not an exact Compile-LZ resource", resource.name))?;
        ensure!(
            decoded.streams.len() == 1,
            "{} must contain exactly one Compile-LZ stream",
            resource.name
        );
        let original_decoded = decoded.streams.into_iter().next().unwrap();
        let (updated_packed, report) = rebuild_gameplay_text_resource(
            resource,
            &original_packed,
            &original_decoded,
            codebook,
        )?;
        payload.insert(resource.name.clone(), updated_packed);
        reports.push(report);
    }

    Ok(GameplayTextRebuildReport {
        changed_resources: reports
            .iter()
            .filter(|report| report.changed_entries > 0)
            .count(),
        changed_entries: reports.iter().map(|report| report.changed_entries).sum(),
        relocated_entries: reports.iter().map(|report| report.relocated_entries).sum(),
        resources: reports,
    })
}

pub(crate) fn read_gameplay_text_catalog(path: &Path) -> Result<GameplayTextCatalog> {
    if !path.is_dir() {
        return read_catalog_json(path, "gameplay text catalog");
    }

    let bundle_path = path.join("catalog.json");
    let bundle: GameplayTextCatalogBundle =
        read_catalog_json(&bundle_path, "gameplay text catalog bundle")?;
    ensure!(
        bundle.schema == CATALOG_BUNDLE_SCHEMA,
        "unsupported gameplay text catalog bundle schema {:?}",
        bundle.schema
    );
    ensure!(
        bundle.catalog.resources.is_empty(),
        "gameplay catalog bundle metadata must not contain inline resources"
    );
    ensure!(
        !bundle.resource_files.is_empty(),
        "gameplay catalog bundle has no resource files"
    );

    let mut catalog = bundle.catalog;
    let mut listed_files = HashSet::new();
    let mut resource_names = HashSet::new();
    for relative_path in bundle.resource_files {
        ensure!(
            listed_files.insert(relative_path.clone()),
            "gameplay catalog bundle lists {relative_path:?} more than once"
        );
        let resource_path =
            resolve_catalog_part_path(path, &relative_path, "gameplay resource file")?;
        let resource_file: GameplayTextResourceFile =
            read_catalog_json(&resource_path, "gameplay resource file")?;
        ensure!(
            resource_file.schema == RESOURCE_FILE_SCHEMA,
            "unsupported gameplay resource schema {:?} in {}",
            resource_file.schema,
            resource_path.display()
        );
        ensure!(
            resource_names.insert(resource_file.resource.name.clone()),
            "gameplay resource {} appears more than once",
            resource_file.resource.name
        );
        ensure!(
            resource_file.resource.entries.len() == resource_file.resource.entry_count,
            "{} declares {} entries but contains {}",
            resource_file.resource.name,
            resource_file.resource.entry_count,
            resource_file.resource.entries.len()
        );
        catalog.resources.push(resource_file.resource);
    }

    ensure!(
        catalog.resources.len() == catalog.resource_count,
        "gameplay catalog bundle declares {} resources but contains {}",
        catalog.resource_count,
        catalog.resources.len()
    );
    let entry_count = catalog
        .resources
        .iter()
        .map(|resource| resource.entries.len())
        .sum::<usize>();
    ensure!(
        entry_count == catalog.entry_count,
        "gameplay catalog bundle declares {} entries but contains {entry_count}",
        catalog.entry_count
    );
    let reference_count = catalog
        .resources
        .iter()
        .map(|resource| resource.reference_count)
        .sum::<usize>();
    ensure!(
        reference_count == catalog.reference_count,
        "gameplay catalog bundle declares {} references but contains {reference_count}",
        catalog.reference_count
    );
    Ok(catalog)
}

fn validate_protected_gameplay_catalog(
    expected: &GameplayTextCatalog,
    supplied: &GameplayTextCatalog,
) -> Result<()> {
    validate_translation_states(supplied)?;
    let mut protected = supplied.clone();
    for resource in &mut protected.resources {
        for entry in &mut resource.entries {
            entry.ko.clear();
            entry.status = UNTRANSLATED_STATUS.to_owned();
            entry.notes.clear();
        }
    }
    ensure!(
        protected == *expected,
        "gameplay text catalog protected fields differ from the exact source extraction"
    );
    Ok(())
}

fn rebuild_gameplay_text_resource(
    resource: &GameplayTextResource,
    original_packed: &[u8],
    original_decoded: &[u8],
    codebook: &BTreeMap<char, [u8; 2]>,
) -> Result<(Vec<u8>, GameplayTextResourceRebuildReport)> {
    let pool_capacity = resource
        .text_pool_end
        .checked_sub(resource.text_pool_start)
        .context("gameplay text pool ends before it starts")?;
    let mut encoded_entries = Vec::with_capacity(resource.entries.len());
    let mut pool_used = 0usize;
    let mut source_cursor = resource.text_pool_start;
    let mut changed_entries = 0usize;
    for entry in &resource.entries {
        ensure!(
            entry.string_decoded_offset == source_cursor,
            "{} source text pool is not contiguous at 0x{source_cursor:04X}",
            entry.id
        );
        let source_end = entry
            .string_decoded_offset
            .checked_add(entry.byte_budget)
            .with_context(|| format!("{} source slot overflow", entry.id))?;
        let source_slot = original_decoded
            .get(entry.string_decoded_offset..source_end)
            .with_context(|| format!("{} source slot lies outside {}", entry.id, resource.name))?;
        ensure!(
            source_slot.last() == Some(&0) && !source_slot[..source_slot.len() - 1].contains(&0),
            "{} source slot does not have one trailing NUL",
            entry.id
        );
        let source_body = &source_slot[..source_slot.len() - 1];
        ensure_message_avoids_renderer_sheet_codes(source_body).with_context(|| {
            format!(
                "{} source text conflicts with the renderer font rows",
                entry.id
            )
        })?;
        let encoded = if entry.ko.is_empty() {
            source_body.to_vec()
        } else {
            changed_entries += 1;
            encode_renderer_text(&entry.ko, codebook)
                .with_context(|| format!("{} Korean text cannot be encoded", entry.id))?
        };
        ensure!(
            renderer_control_sequences(&encoded)? == renderer_control_sequences(source_body)?,
            "{} Korean text changed the ordered renderer control instructions",
            entry.id
        );
        pool_used = pool_used
            .checked_add(encoded.len() + 1)
            .with_context(|| format!("{} translated pool size overflow", resource.name))?;
        encoded_entries.push(encoded);
        source_cursor = source_end;
    }
    ensure!(
        source_cursor == resource.text_pool_end,
        "{} protected text pool end is not the end of its entries",
        resource.name
    );
    ensure!(
        pool_used <= pool_capacity,
        "{} translated text pool needs {pool_used} bytes but has {pool_capacity}",
        resource.name
    );

    let mut destinations = Vec::with_capacity(resource.entries.len());
    let mut destination_cursor = resource.text_pool_start;
    for (entry, encoded) in resource.entries.iter().zip(&encoded_entries) {
        u16::try_from(destination_cursor)
            .with_context(|| format!("{} translated pointer exceeds 16 bits", entry.id))?;
        destinations.push(destination_cursor);
        destination_cursor += encoded.len() + 1;
    }
    ensure!(
        destination_cursor == resource.text_pool_start + pool_used,
        "{} translated pool placement is inconsistent",
        resource.name
    );

    let mut seen_sites = HashSet::new();
    for (entry, destination) in resource.entries.iter().zip(&destinations) {
        for reference in &entry.references {
            let site = match reference {
                GameplayTextReference::MovSiThenNearCall {
                    target_word_decoded_offset,
                    ..
                }
                | GameplayTextReference::ShopMessageTable {
                    target_word_decoded_offset,
                    ..
                } => *target_word_decoded_offset,
                GameplayTextReference::ContiguousTextPool { .. } => continue,
            };
            ensure!(
                seen_sites.insert(site),
                "{} repeats gameplay pointer site 0x{site:04X}",
                resource.name
            );
            ensure!(
                site + 2 <= resource.text_pool_start || site >= resource.text_pool_end,
                "{} pointer site 0x{site:04X} overlaps its text pool",
                entry.id
            );
            let raw = original_decoded.get(site..site + 2).with_context(|| {
                format!("{} pointer site 0x{site:04X} is out of range", entry.id)
            })?;
            let actual = usize::from(u16::from_le_bytes([raw[0], raw[1]]));
            ensure!(
                actual == entry.string_decoded_offset,
                "{} pointer site 0x{site:04X} holds 0x{actual:04X}, expected 0x{:04X}",
                entry.id,
                entry.string_decoded_offset
            );
            u16::try_from(*destination)
                .with_context(|| format!("{} destination exceeds 16 bits", entry.id))?;
        }
    }

    let mut updated_decoded = original_decoded.to_vec();
    updated_decoded[resource.text_pool_start..resource.text_pool_end].fill(0);
    for ((entry, encoded), destination) in resource
        .entries
        .iter()
        .zip(&encoded_entries)
        .zip(&destinations)
    {
        let end = destination + encoded.len();
        updated_decoded[*destination..end].copy_from_slice(encoded);
        ensure!(
            updated_decoded.get(end) == Some(&0),
            "{} translated slot lost its NUL terminator",
            entry.id
        );
        let pointer = u16::try_from(*destination)?.to_le_bytes();
        for reference in &entry.references {
            let site = match reference {
                GameplayTextReference::MovSiThenNearCall {
                    target_word_decoded_offset,
                    ..
                }
                | GameplayTextReference::ShopMessageTable {
                    target_word_decoded_offset,
                    ..
                } => *target_word_decoded_offset,
                GameplayTextReference::ContiguousTextPool { .. } => continue,
            };
            updated_decoded[site..site + 2].copy_from_slice(&pointer);
        }
    }

    let relocated_entries = resource
        .entries
        .iter()
        .zip(&destinations)
        .filter(|(entry, destination)| entry.string_decoded_offset != **destination)
        .count();
    let updated_packed = if changed_entries == 0 {
        original_packed.to_vec()
    } else {
        encode_compile_lz(&updated_decoded)
    };
    let roundtrip = decode_exact_compile_lz(&updated_packed)
        .with_context(|| format!("rebuilt {} is not exact Compile-LZ", resource.name))?;
    ensure!(
        roundtrip.streams == [updated_decoded.clone()],
        "rebuilt {} does not decode to the planned bytes",
        resource.name
    );

    let report = GameplayTextResourceRebuildReport {
        name: resource.name.clone(),
        changed_entries,
        relocated_entries: if changed_entries == 0 {
            0
        } else {
            relocated_entries
        },
        pool_capacity,
        pool_used: if changed_entries == 0 {
            pool_capacity
        } else {
            pool_used
        },
        original_packed_sha256: sha256_hex(original_packed),
        updated_packed_sha256: sha256_hex(&updated_packed),
        original_decoded_sha256: sha256_hex(original_decoded),
        updated_decoded_sha256: sha256_hex(&updated_decoded),
        original_packed_size: original_packed.len(),
        updated_packed_size: updated_packed.len(),
        decoded_size: updated_decoded.len(),
        exact_decode_roundtrip: true,
    };
    Ok((updated_packed, report))
}

fn extract_resource(
    payload: &BTreeMap<String, Vec<u8>>,
    profile: &ResourceProfile,
) -> Result<GameplayTextResource> {
    let packed = payload
        .get(profile.name)
        .with_context(|| format!("MADOU.EXE payload is missing {}", profile.name))?;
    ensure!(
        sha256_hex(packed) == profile.packed_sha256,
        "{} packed bytes differ from the supported resource",
        profile.name
    );
    let decoded = decode_exact_compile_lz(packed)
        .with_context(|| format!("{} is not an exact Compile-LZ resource", profile.name))?;
    ensure!(
        decoded.streams.len() == 1,
        "{} must contain exactly one Compile-LZ stream",
        profile.name
    );
    let decoded = decoded.streams.into_iter().next().unwrap();
    ensure!(
        decoded.len() == profile.decoded_size && sha256_hex(&decoded) == profile.decoded_sha256,
        "{} decoded bytes differ from the supported resource",
        profile.name
    );

    let mut references = direct_text_references(&decoded);
    merge_references(
        &mut references,
        shop_message_table_references(profile.name, &decoded)?,
    )?;
    let first_anchor = references
        .keys()
        .next()
        .copied()
        .with_context(|| format!("{} has no consumer-linked text anchors", profile.name))?;
    for resource_references in references.values_mut() {
        resource_references.retain(|reference| match reference {
            GameplayTextReference::MovSiThenNearCall {
                instruction_decoded_offset,
                ..
            } => *instruction_decoded_offset < first_anchor,
            GameplayTextReference::ShopMessageTable { .. } => true,
            GameplayTextReference::ContiguousTextPool { .. } => false,
        });
    }
    references.retain(|_, resource_references| !resource_references.is_empty());
    let last_anchor = references
        .keys()
        .next_back()
        .copied()
        .with_context(|| format!("{} has no validated text anchors", profile.name))?;
    let (text_pool_end, entries) = collect_text_pool(
        profile.name,
        &decoded,
        first_anchor,
        last_anchor,
        references,
    )?;
    ensure!(
        entries.len() == expected_entry_count(profile.name),
        "{} text population changed: expected {}, found {}",
        profile.name,
        expected_entry_count(profile.name),
        entries.len()
    );
    let reference_count = entries.iter().map(|entry| entry.references.len()).sum();

    Ok(GameplayTextResource {
        name: profile.name.to_owned(),
        source_packed_sha256: sha256_hex(packed),
        source_decoded_sha256: sha256_hex(&decoded),
        decoded_size: decoded.len(),
        text_pool_start: first_anchor,
        text_pool_end,
        entry_count: entries.len(),
        reference_count,
        unresolved_entry_count: 0,
        entries,
    })
}

fn direct_text_references(decoded: &[u8]) -> BTreeMap<usize, Vec<GameplayTextReference>> {
    let mut references = BTreeMap::<usize, Vec<GameplayTextReference>>::new();
    for instruction_offset in 0..decoded.len() {
        let Ok(mov) = decode_bytes(&decoded[instruction_offset..]) else {
            continue;
        };
        let Instruction::Mov {
            dest: Operand::Reg16(Register16::SI),
            src: Operand::Imm16(target),
        } = mov.instruction
        else {
            continue;
        };
        if mov.byte_len != 3 || !mov.prefixes.is_empty() {
            continue;
        }
        let call_offset = instruction_offset + mov.byte_len;
        let Ok(call) = decode_bytes(&decoded[call_offset..]) else {
            continue;
        };
        let Instruction::Call {
            target: CallTarget::Rel16(relative),
        } = call.instruction
        else {
            continue;
        };
        if call.byte_len != 3 || !call.prefixes.is_empty() {
            continue;
        }
        let renderer = call_offset as isize + call.byte_len as isize + relative as isize;
        if renderer < 0 || renderer as usize >= decoded.len() {
            continue;
        }
        let target = usize::from(target);
        let Some(raw) = nul_payload(decoded, target) else {
            continue;
        };
        if !is_japanese_text(raw) {
            continue;
        }
        references
            .entry(target)
            .or_default()
            .push(GameplayTextReference::MovSiThenNearCall {
                instruction_decoded_offset: instruction_offset,
                target_word_decoded_offset: instruction_offset + 1,
                call_decoded_offset: call_offset,
                renderer_decoded_offset: renderer as usize,
            });
    }
    references
}

fn shop_message_table_references(
    name: &str,
    decoded: &[u8],
) -> Result<BTreeMap<usize, Vec<GameplayTextReference>>> {
    if !matches!(name, "S01.DAT" | "S02.DAT") {
        return Ok(BTreeMap::new());
    }
    const TABLE_WORDS: usize = 10;
    const TABLE_BYTES: usize = 2 + TABLE_WORDS * 2;
    const TOPIC_FRAGMENT: &[u8] = b"\x82\xCD\x0A";
    const COPULA_FRAGMENT: &[u8] = b"\x82\xC5\x82\xB7\x0A\x0A";
    const PRICE_FRAGMENT: &[u8] = b"\x82\xC9\x82\xC8\x82\xE8\x82\xDC\x82\xB7\x0A\x0A";

    let mut tables = Vec::new();
    for marker in 0..=decoded.len().saturating_sub(TABLE_BYTES) {
        if decoded.get(marker..marker + 2) != Some(&[0xff, 0xff]) {
            continue;
        }
        let mut targets = [0usize; TABLE_WORDS];
        for (index, target) in targets.iter_mut().enumerate() {
            let site = marker + 2 + index * 2;
            *target = usize::from(u16::from_le_bytes([decoded[site], decoded[site + 1]]));
        }
        if targets[6] != targets[8]
            || nul_payload(decoded, targets[6]) != Some(TOPIC_FRAGMENT)
            || nul_payload(decoded, targets[7]) != Some(COPULA_FRAGMENT)
            || nul_payload(decoded, targets[9]) != Some(PRICE_FRAGMENT)
            || targets
                .iter()
                .any(|target| !nul_payload(decoded, *target).is_some_and(is_japanese_text))
        {
            continue;
        }
        tables.push((marker, targets));
    }
    ensure!(
        tables.len() == 1,
        "{name} expected exactly one anchored ten-message shop table, found {}",
        tables.len()
    );
    let (marker, targets) = tables.pop().unwrap();
    let mut references = BTreeMap::<usize, Vec<GameplayTextReference>>::new();
    for (table_index, target) in targets.into_iter().enumerate() {
        references
            .entry(target)
            .or_default()
            .push(GameplayTextReference::ShopMessageTable {
                table_decoded_offset: marker,
                table_index,
                target_word_decoded_offset: marker + 2 + table_index * 2,
            });
    }
    Ok(references)
}

fn merge_references(
    destination: &mut BTreeMap<usize, Vec<GameplayTextReference>>,
    source: BTreeMap<usize, Vec<GameplayTextReference>>,
) -> Result<()> {
    for (target, source_references) in source {
        let target_references = destination.entry(target).or_default();
        for reference in source_references {
            ensure!(
                !target_references.contains(&reference),
                "duplicate gameplay text reference at decoded target 0x{target:04X}"
            );
            target_references.push(reference);
        }
    }
    Ok(())
}

fn collect_text_pool(
    name: &str,
    decoded: &[u8],
    first_anchor: usize,
    last_anchor: usize,
    mut references: BTreeMap<usize, Vec<GameplayTextReference>>,
) -> Result<(usize, Vec<GameplayTextEntry>)> {
    let mut entries = Vec::new();
    let mut cursor = first_anchor;
    let mut previous = None;
    loop {
        let raw = nul_payload(decoded, cursor)
            .with_context(|| format!("{name} text pool at 0x{cursor:04X} has no NUL terminator"))?;
        if !is_japanese_text(raw) {
            ensure!(
                cursor > last_anchor,
                "{name} text pool has a non-text gap at 0x{cursor:04X} before the last anchor"
            );
            break;
        }
        let mut entry_references = references.remove(&cursor).unwrap_or_default();
        if entry_references.is_empty() {
            let previous_string_decoded_offset = previous.with_context(|| {
                format!("{name} first text-pool slot at 0x{cursor:04X} lacks a consumer anchor")
            })?;
            entry_references.push(GameplayTextReference::ContiguousTextPool {
                previous_string_decoded_offset,
            });
        }
        entries.push(GameplayTextEntry {
            id: format!("{}_{cursor:04X}", name.trim_end_matches(".DAT")),
            string_decoded_offset: cursor,
            byte_budget: raw.len() + 1,
            raw_hex: encode_hex(raw),
            source_text: SHIFT_JIS.decode(raw).0.into_owned(),
            references: entry_references,
            ko: String::new(),
            status: UNTRANSLATED_STATUS.to_owned(),
            notes: String::new(),
        });
        previous = Some(cursor);
        cursor += raw.len() + 1;
    }
    ensure!(
        references.is_empty(),
        "{name} has consumer anchors outside its contiguous text pool: {:?}",
        references.keys().collect::<Vec<_>>()
    );
    Ok((cursor, entries))
}

fn expected_entry_count(name: &str) -> usize {
    match name {
        "S01.DAT" => 11,
        "S02.DAT" => 9,
        "ENEMY01.DAT" => 2,
        "ENEMY02.DAT" => 4,
        "ENEMY03.DAT" => 4,
        "ENEMY04.DAT" => 6,
        "ENEMY05.DAT" => 3,
        "ENEMY06.DAT" => 2,
        "ENEMY07.DAT" => 4,
        "ENEMY08.DAT" => 4,
        "ENEMY09.DAT" => 5,
        "ENEMY0A.DAT" => 3,
        "ENEMY0B.DAT" => 3,
        "ENEMY0C.DAT" => 3,
        "ENEMY0D.DAT" => 5,
        "ENEMY0E.DAT" => 2,
        _ => unreachable!("resource profile has an entry-count expectation"),
    }
}

fn validate_translation_states(catalog: &GameplayTextCatalog) -> Result<()> {
    for resource in &catalog.resources {
        for entry in &resource.entries {
            match entry.status.as_str() {
                "untranslated" => ensure!(
                    entry.ko.is_empty(),
                    "{}: untranslated entry must have empty Korean text",
                    entry.id
                ),
                "in_progress" | "needs_review" | "needs_human_review" | "distribution_eligible" => {
                    ensure!(
                        !entry.ko.is_empty(),
                        "{}: {} entry must have Korean text",
                        entry.id,
                        entry.status
                    )
                }
                other => bail!("{}: unsupported translation status {other:?}", entry.id),
            }
        }
    }
    Ok(())
}

fn nul_payload(decoded: &[u8], start: usize) -> Option<&[u8]> {
    let tail = decoded.get(start..)?;
    let end = tail.iter().position(|byte| *byte == 0)?;
    Some(&tail[..end])
}

fn is_japanese_text(raw: &[u8]) -> bool {
    if raw.is_empty() {
        return false;
    }
    let (text, _, had_errors) = SHIFT_JIS.decode(raw);
    !had_errors
        && text.chars().any(|character| {
            matches!(
                character,
                '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{ff61}'..='\u{ff9f}'
            )
        })
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

    #[test]
    fn contiguous_text_pool_keeps_an_unpointed_middle_slot() {
        let decoded = [
            0x82, 0xa0, 0x00, // あ
            0x82, 0xa2, 0x00, // い
            0x82, 0xa4, 0x00, // う
            0xff, 0x00,
        ];
        let mut references = BTreeMap::new();
        for (index, target) in [0usize, 6].into_iter().enumerate() {
            references.insert(
                target,
                vec![GameplayTextReference::ShopMessageTable {
                    table_decoded_offset: 0x20,
                    table_index: index,
                    target_word_decoded_offset: 0x22 + index * 2,
                }],
            );
        }

        let (pool_end, entries) =
            collect_text_pool("TEST.DAT", &decoded, 0, 6, references).unwrap();

        assert_eq!(pool_end, 9);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].source_text, "い");
        assert_eq!(
            entries[1].references,
            [GameplayTextReference::ContiguousTextPool {
                previous_string_decoded_offset: 0,
            }]
        );
    }

    #[test]
    fn typed_direct_scan_ignores_a_target_word_inside_another_instruction() {
        let mut decoded = vec![0_u8; 0x30];
        decoded[..3].copy_from_slice(&[0x03, 0x20, 0x00]);
        decoded[0x20..0x23].copy_from_slice(&[0x82, 0xa0, 0x00]);

        assert!(direct_text_references(&decoded).is_empty());
    }

    #[test]
    fn shop_table_keeps_duplicate_pointer_sites_as_separate_references() {
        let mut decoded = vec![0_u8; 0xc0];
        let marker = 0x10;
        decoded[marker..marker + 2].copy_from_slice(&[0xff, 0xff]);
        let targets = [
            0x70u16, 0x70, 0x70, 0x70, 0x70, 0x70, 0x80, 0x90, 0x80, 0xa0,
        ];
        for (index, target) in targets.into_iter().enumerate() {
            let site = marker + 2 + index * 2;
            decoded[site..site + 2].copy_from_slice(&target.to_le_bytes());
        }
        decoded[0x70..0x73].copy_from_slice(&[0x82, 0xa0, 0]);
        decoded[0x80..0x84].copy_from_slice(b"\x82\xCD\x0A\0");
        decoded[0x90..0x97].copy_from_slice(b"\x82\xC5\x82\xB7\x0A\x0A\0");
        decoded[0xa0..0xad].copy_from_slice(b"\x82\xC9\x82\xC8\x82\xE8\x82\xDC\x82\xB7\x0A\x0A\0");

        let references = shop_message_table_references("S01.DAT", &decoded).unwrap();

        assert_eq!(references.values().map(Vec::len).sum::<usize>(), 10);
        assert_eq!(references[&0x80].len(), 2);
    }

    #[test]
    fn rebuild_repacks_a_complete_pool_and_rewrites_moved_entries() {
        let mut decoded = vec![0_u8; 0x40];
        decoded[0x04..0x06].copy_from_slice(&0x20_u16.to_le_bytes());
        decoded[0x06..0x08].copy_from_slice(&0x25_u16.to_le_bytes());
        decoded[0x20..0x25].copy_from_slice(b"AAA\n\0");
        decoded[0x25..0x2a].copy_from_slice(b"BBBB\0");
        let packed = encode_compile_lz(&decoded);
        let resource = GameplayTextResource {
            name: "TEST.DAT".to_owned(),
            source_packed_sha256: sha256_hex(&packed),
            source_decoded_sha256: sha256_hex(&decoded),
            decoded_size: decoded.len(),
            text_pool_start: 0x20,
            text_pool_end: 0x2a,
            entry_count: 2,
            reference_count: 2,
            unresolved_entry_count: 0,
            entries: vec![
                GameplayTextEntry {
                    id: "TEST_0020".to_owned(),
                    string_decoded_offset: 0x20,
                    byte_budget: 5,
                    raw_hex: "4141410a".to_owned(),
                    source_text: "AAA\n".to_owned(),
                    references: vec![GameplayTextReference::MovSiThenNearCall {
                        instruction_decoded_offset: 3,
                        target_word_decoded_offset: 4,
                        call_decoded_offset: 6,
                        renderer_decoded_offset: 8,
                    }],
                    ko: "가\n".to_owned(),
                    status: "needs_human_review".to_owned(),
                    notes: "pool repack test".to_owned(),
                },
                GameplayTextEntry {
                    id: "TEST_0025".to_owned(),
                    string_decoded_offset: 0x25,
                    byte_budget: 5,
                    raw_hex: "42424242".to_owned(),
                    source_text: "BBBB".to_owned(),
                    references: vec![GameplayTextReference::ShopMessageTable {
                        table_decoded_offset: 0,
                        table_index: 0,
                        target_word_decoded_offset: 6,
                    }],
                    ko: String::new(),
                    status: "untranslated".to_owned(),
                    notes: String::new(),
                },
            ],
        };
        let codebook = [('가', [0xeb, 0x40])]
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        let (updated, report) =
            rebuild_gameplay_text_resource(&resource, &packed, &decoded, &codebook).unwrap();
        let rebuilt = decode_exact_compile_lz(&updated).unwrap().streams.remove(0);

        assert_eq!(report.changed_entries, 1);
        assert_eq!(report.relocated_entries, 1);
        assert_eq!(&rebuilt[0x20..0x2a], b"\xeb\x40\n\0BBBB\0\0");
        assert_eq!(&rebuilt[0x04..0x06], &0x20_u16.to_le_bytes());
        assert_eq!(&rebuilt[0x06..0x08], &0x24_u16.to_le_bytes());
    }
}
