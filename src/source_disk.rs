use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use fatfs::{FatType, FileSystem, FsOptions};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) const DISK_SIZE: usize = 1_261_568;
pub(crate) const BOOT_SECTOR_SIZE: usize = 1_024;
const FATFS_HIDDEN_SECTORS_OFFSET: usize = 28;
const FATFS_TOTAL_SECTORS_32_OFFSET: usize = 32;
const FATFS_SIGNATURE_OFFSET: usize = 510;

pub(crate) const SYSTEM_FILES: [&str; 2] = ["IO98.SYS", "MEGDOS.SYS"];
pub(crate) const TOOL_FILES: [&str; 4] = ["DSH.COM", "FPLAY.COM", "BPLAY.COM", "CONFIG.SYS"];
pub(crate) const INSTALLER_NAME: &str = "MADOU.EXE";
pub(crate) const INSTALLER_SHA256: &str =
    "44d30282114a38c22df6b7eddb624650bcff53a0813f328388410b1c740e98e0";

const SOURCE_PROFILES: [SourceProfile; 3] = [
    SourceProfile {
        id: "disc-station-vol03-disk1",
        sha256: "2ba5ada68e76a74a2659484174b79ebf9a2c8285fab650e11a5340698d78b22e",
    },
    SourceProfile {
        id: "disc-station-vol03-disk1-set1",
        sha256: "bcc7ca6f35fe057e8d1f8341353e77a93003062cff1872e10681eab18df99226",
    },
    SourceProfile {
        id: "disc-station-vol03-disk1-alt1",
        sha256: "2ec8333fe9dc98e27d19a5ce9642651bbcb0e6654d8b0fa3cd769d15a81bc140",
    },
];

#[derive(Clone, Copy)]
struct SourceProfile {
    id: &'static str,
    sha256: &'static str,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct SourceReport {
    pub profile: String,
    pub sha256: String,
    pub size: usize,
    pub installer_sha256: String,
}

pub(crate) struct VerifiedSource {
    pub image: Vec<u8>,
    pub report: SourceReport,
    pub system_files: BTreeMap<String, Vec<u8>>,
    pub tool_files: BTreeMap<String, Vec<u8>>,
    pub installer: Vec<u8>,
}

pub fn verify_source_path(path: &Path) -> Result<SourceReport> {
    Ok(load_verified_source(path)?.report)
}

pub(crate) fn load_verified_source(path: &Path) -> Result<VerifiedSource> {
    let image = fs::read(path)
        .with_context(|| format!("failed to read source disk: {}", path.display()))?;
    ensure!(
        image.len() == DISK_SIZE,
        "unsupported source size: expected {DISK_SIZE} bytes, got {}",
        image.len()
    );
    let image_sha256 = sha256_hex(&image);
    let profile = SOURCE_PROFILES
        .iter()
        .find(|profile| profile.sha256 == image_sha256)
        .with_context(|| {
            format!("unsupported Disc Station Vol. 03 Disk 1 SHA-256: {image_sha256}")
        })?;
    validate_pc98_fat12_geometry(&image)?;

    let mount_image = fatfs_mount_copy(&image)?;
    let filesystem = FileSystem::new(Cursor::new(mount_image), FsOptions::new())
        .context("failed to mount supported source as FAT12")?;
    ensure!(
        matches!(filesystem.fat_type(), FatType::Fat12),
        "supported source did not mount as FAT12"
    );
    let root = filesystem.root_dir();

    let installer = read_file(&root, INSTALLER_NAME)?;
    let installer_sha256 = sha256_hex(&installer);
    ensure!(
        installer_sha256 == INSTALLER_SHA256,
        "MADOU.EXE failed SHA-256 verification: expected {INSTALLER_SHA256}, got {installer_sha256}"
    );

    let mut system_files = BTreeMap::new();
    for name in SYSTEM_FILES {
        system_files.insert(name.to_owned(), read_file(&root, name)?);
    }
    let mut tool_files = BTreeMap::new();
    for name in TOOL_FILES {
        tool_files.insert(name.to_owned(), read_file(&root, name)?);
    }

    drop(root);
    drop(filesystem);

    Ok(VerifiedSource {
        image,
        report: SourceReport {
            profile: profile.id.to_owned(),
            sha256: image_sha256,
            size: DISK_SIZE,
            installer_sha256,
        },
        system_files,
        tool_files,
        installer,
    })
}

pub(crate) fn fatfs_mount_copy(image: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        image.len() > FATFS_SIGNATURE_OFFSET + 1,
        "image is too short for the fatfs compatibility signature"
    );
    let mut mount_copy = image.to_vec();
    mount_copy[FATFS_HIDDEN_SECTORS_OFFSET..FATFS_TOTAL_SECTORS_32_OFFSET].fill(0);
    mount_copy[FATFS_TOTAL_SECTORS_32_OFFSET..FATFS_TOTAL_SECTORS_32_OFFSET + 4].fill(0);
    mount_copy[FATFS_SIGNATURE_OFFSET..FATFS_SIGNATURE_OFFSET + 2].copy_from_slice(&[0x55, 0xaa]);
    Ok(mount_copy)
}

fn read_file<T: fatfs::ReadWriteSeek>(
    directory: &fatfs::Dir<'_, T>,
    name: &str,
) -> Result<Vec<u8>> {
    let mut file = directory
        .open_file(name)
        .with_context(|| format!("required source file is missing: {name}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("failed to read FAT12 file: {name}"))?;
    Ok(bytes)
}

pub(crate) fn validate_pc98_fat12_geometry(source: &[u8]) -> Result<()> {
    let bpb = source
        .get(..BOOT_SECTOR_SIZE)
        .context("source is missing its 1024-byte boot sector")?;
    let u16_at = |offset: usize| -> Result<u16> {
        let bytes: [u8; 2] = bpb
            .get(offset..offset + 2)
            .context("truncated BIOS parameter block")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid BIOS parameter block field"))?;
        Ok(u16::from_le_bytes(bytes))
    };
    let observed = (
        u16_at(11)?,
        bpb[13],
        u16_at(14)?,
        bpb[16],
        u16_at(17)?,
        u16_at(19)?,
        bpb[21],
        u16_at(22)?,
        u16_at(24)?,
        u16_at(26)?,
    );
    let expected = (1024, 1, 1, 2, 192, 1232, 0xfe, 2, 8, 2);
    if observed != expected {
        bail!("supported source has unexpected PC-98 FAT12 geometry: {observed:?}");
    }
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}
