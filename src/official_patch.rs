use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::expected_write::{FixedRangeExpectedWrite, apply_fixed_range_expected_writes};
use crate::source_disk::sha256_hex;

pub(crate) const ORIGINAL_BSAMP_SHA256: &str =
    "f0644a2b56eee4aa31fed4d8e6f517cf28acd18120ceacc17b6b270f5923be91";
pub(crate) const UPDATED_BSAMP_SHA256: &str =
    "c8f1f5bea4f41d968f3790b1ceefcdf204a8e011015489346b402f4450c9cb8e";
const BSAMP_SIZE: usize = 5_921;

const OFFICIAL_BYTE_ROTATIONS: [ByteRotation; 2] = [
    ByteRotation {
        offset: 0x03e2,
        length: 35,
        rotate_left: 25,
    },
    ByteRotation {
        offset: 0x06f2,
        length: 35,
        rotate_left: 25,
    },
];

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
pub struct OfficialFreezeFixRange {
    pub offset: usize,
    pub length: usize,
    pub rotate_left: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct OfficialFreezeFixReport {
    pub original_bsamp_sha256: String,
    pub updated_bsamp_sha256: String,
    pub bsamp_size: usize,
    pub changed_byte_count: usize,
    pub ranges: Vec<OfficialFreezeFixRange>,
}

pub(crate) struct AppliedOfficialFreezeFix {
    pub report: OfficialFreezeFixReport,
    pub updated_bsamp: Vec<u8>,
}

#[derive(Clone, Copy)]
struct ByteRotation {
    offset: usize,
    length: usize,
    rotate_left: usize,
}

pub(crate) fn apply_official_freeze_fix(original: &[u8]) -> Result<AppliedOfficialFreezeFix> {
    ensure!(
        original.len() == BSAMP_SIZE,
        "original BSAMP.COM has size {}, expected {BSAMP_SIZE}",
        original.len()
    );
    let original_bsamp_sha256 = sha256_hex(original);
    ensure!(
        original_bsamp_sha256 == ORIGINAL_BSAMP_SHA256,
        "original BSAMP.COM failed the official freeze-fix precondition: expected {ORIGINAL_BSAMP_SHA256}, got {original_bsamp_sha256}"
    );
    let writes = OFFICIAL_BYTE_ROTATIONS
        .into_iter()
        .map(|rotation| {
            let end = rotation
                .offset
                .checked_add(rotation.length)
                .context("official freeze-fix range overflow")?;
            let expected_source = original
                .get(rotation.offset..end)
                .map(<[u8]>::to_vec)
                .with_context(|| {
                    format!(
                        "official freeze-fix range {:#x}..{end:#x} is outside BSAMP.COM",
                        rotation.offset
                    )
                })?;
            ensure!(
                rotation.rotate_left < expected_source.len(),
                "official freeze-fix rotation is not smaller than its range"
            );
            let mut replacement = expected_source.clone();
            replacement.rotate_left(rotation.rotate_left);
            Ok(FixedRangeExpectedWrite {
                writer: "official_freeze_fix",
                purpose: "derive the Disc Station Vol. 04 BSAMP.COM update",
                offset: rotation.offset,
                expected_source,
                replacement,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let updated_bsamp = apply_fixed_range_expected_writes(original, &writes)?;

    let updated_bsamp_sha256 = sha256_hex(&updated_bsamp);
    ensure!(
        updated_bsamp_sha256 == UPDATED_BSAMP_SHA256,
        "derived BSAMP.COM does not match the official freeze fix: expected {UPDATED_BSAMP_SHA256}, got {updated_bsamp_sha256}"
    );
    let changed_byte_count = original
        .iter()
        .zip(&updated_bsamp)
        .filter(|(before, after)| before != after)
        .count();
    Ok(AppliedOfficialFreezeFix {
        report: OfficialFreezeFixReport {
            original_bsamp_sha256,
            updated_bsamp_sha256,
            bsamp_size: BSAMP_SIZE,
            changed_byte_count,
            ranges: OFFICIAL_BYTE_ROTATIONS
                .into_iter()
                .map(|rotation| OfficialFreezeFixRange {
                    offset: rotation.offset,
                    length: rotation.length,
                    rotate_left: rotation.rotate_left,
                })
                .collect(),
        },
        updated_bsamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_freeze_fix_rejects_wrong_sized_bsamp() {
        let error = apply_official_freeze_fix(&vec![0; BSAMP_SIZE - 1])
            .err()
            .expect("wrong-sized BSAMP.COM must fail");
        assert!(error.to_string().contains("has size"));
    }
}
