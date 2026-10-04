use std::fs;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::bps;
use crate::official_patch::OfficialFreezeFixReport;
use crate::source_disk::{DISK_SIZE, sha256_hex, validate_pc98_fat12_geometry};
use crate::standalone::{build_bundled_release_image, transplant_localized_files};
use crate::translation_test::ReleaseInputReport;

const RELEASE_VERSION: &str = env!("CARGO_PKG_VERSION");
const SOURCE_PROFILE: &str = "docho-ibun-standalone-fdi";
const SOURCE_FDI_SHA256: &str = "cb6647a726861d7ba2d1cdd414cf21f6f35f892fb1f4065a8116f4367656d6b4";
const FDI_HEADER_SIZE: usize = 4_096;
const FDI_SIZE: usize = FDI_HEADER_SIZE + DISK_SIZE;

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct FdiBpsReleaseReport {
    pub version: String,
    pub source_profile: String,
    pub source_sha256: String,
    pub source_size: usize,
    pub localized_hdm_sha256: String,
    pub target_sha256: String,
    pub target_size: usize,
    pub patch_sha256: String,
    pub patch_size: usize,
    pub fdi_header_preserved: bool,
    pub verified_file_count: usize,
    pub official_freeze_fix: OfficialFreezeFixReport,
    pub readiness: ReleaseInputReport,
}

pub fn build_fdi_release_patch(
    disc_station_source: &Path,
    source_fdi: &Path,
    patch_output: &Path,
    target_output: Option<&Path>,
) -> Result<FdiBpsReleaseReport> {
    ensure_distinct(source_fdi, patch_output)?;
    ensure_distinct(disc_station_source, patch_output)?;
    if let Some(target_output) = target_output {
        ensure_distinct(source_fdi, target_output)?;
        ensure_distinct(disc_station_source, target_output)?;
        ensure_distinct(patch_output, target_output)?;
    }

    let source = fs::read(source_fdi)
        .with_context(|| format!("failed to read source FDI: {}", source_fdi.display()))?;
    validate_source_fdi(&source)?;
    let source_disk = &source[FDI_HEADER_SIZE..];

    let temporary = tempfile::tempdir()
        .context("failed to create temporary directory for the release build")?;
    let localized_hdm_path = temporary.path().join("localized-release.hdm");
    let release = build_bundled_release_image(disc_station_source, &localized_hdm_path)?;
    let localized_hdm = fs::read(&localized_hdm_path)
        .context("failed to read verified localized HDM from the release build")?;
    let localized_hdm_sha256 = sha256_hex(&localized_hdm);
    let (localized_disk, verified_file_count) =
        transplant_localized_files(source_disk, &localized_hdm)?;

    let mut target = source[..FDI_HEADER_SIZE].to_vec();
    target.extend_from_slice(&localized_disk);
    ensure!(target.len() == FDI_SIZE, "localized FDI size changed");
    ensure!(
        target[..FDI_HEADER_SIZE] == source[..FDI_HEADER_SIZE],
        "localized FDI changed its original header"
    );
    validate_fdi_header(&target)?;

    let patch = bps::create(&source, &target)?;
    ensure!(
        bps::apply(&source, &patch)? == target,
        "BPS readback differs from the localized FDI"
    );

    write_output(patch_output, &patch, "BPS patch")?;
    if let Some(target_output) = target_output {
        write_output(target_output, &target, "localized FDI")?;
    }

    Ok(FdiBpsReleaseReport {
        version: RELEASE_VERSION.to_owned(),
        source_profile: SOURCE_PROFILE.to_owned(),
        source_sha256: sha256_hex(&source),
        source_size: source.len(),
        localized_hdm_sha256,
        target_sha256: sha256_hex(&target),
        target_size: target.len(),
        patch_sha256: sha256_hex(&patch),
        patch_size: patch.len(),
        fdi_header_preserved: true,
        verified_file_count,
        official_freeze_fix: release.localization.standalone.official_freeze_fix,
        readiness: release.readiness,
    })
}

fn validate_source_fdi(source: &[u8]) -> Result<()> {
    ensure!(
        source.len() == FDI_SIZE,
        "unsupported source FDI size: expected {FDI_SIZE}, got {}",
        source.len()
    );
    let sha256 = sha256_hex(source);
    ensure!(
        sha256 == SOURCE_FDI_SHA256,
        "unsupported source FDI SHA-256: expected {SOURCE_FDI_SHA256}, got {sha256}"
    );
    validate_fdi_header(source)?;
    validate_pc98_fat12_geometry(&source[FDI_HEADER_SIZE..])
}

fn validate_fdi_header(fdi: &[u8]) -> Result<()> {
    let u32_at = |offset: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            fdi.get(offset..offset + 4)
                .context("truncated FDI header")?
                .try_into()?,
        ))
    };
    let observed = (
        u32_at(0x08)?,
        u32_at(0x0c)?,
        u32_at(0x10)?,
        u32_at(0x14)?,
        u32_at(0x18)?,
        u32_at(0x1c)?,
    );
    let expected = (FDI_HEADER_SIZE as u32, DISK_SIZE as u32, 1024, 8, 2, 77);
    ensure!(
        observed == expected,
        "unsupported FDI geometry: expected {expected:?}, got {observed:?}"
    );
    Ok(())
}

fn ensure_distinct(left: &Path, right: &Path) -> Result<()> {
    ensure!(left != right, "input and output paths must differ");
    if left.try_exists()? && right.try_exists()? {
        ensure!(
            !same_file::is_same_file(left, right)?,
            "input and output paths alias the same file: {}",
            left.display()
        );
    }
    Ok(())
}

fn write_output(path: &Path, bytes: &[u8], role: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {role} directory: {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("failed to write {role}: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_fdi_geometry_is_recognized() {
        let mut fdi = vec![0; FDI_SIZE];
        for (offset, value) in [
            (0x08, FDI_HEADER_SIZE as u32),
            (0x0c, DISK_SIZE as u32),
            (0x10, 1024),
            (0x14, 8),
            (0x18, 2),
            (0x1c, 77),
        ] {
            fdi[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        validate_fdi_header(&fdi).unwrap();
        fdi[0x14] = 9;
        assert!(validate_fdi_header(&fdi).is_err());
    }
}
