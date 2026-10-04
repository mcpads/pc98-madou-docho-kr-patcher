use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use anyhow::{Context, Result, bail, ensure};
use delharc::LhaDecodeReader;

pub(crate) fn extract_lha_sfx(executable: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let archive_offset = mz_executable_length(executable)?;
    let archive = executable
        .get(archive_offset..)
        .context("installer executable does not contain an LHa archive body")?;
    ensure!(
        archive
            .get(2..7)
            .is_some_and(|method| method.starts_with(b"-lh")),
        "installer archive does not start at the MZ-declared executable boundary"
    );

    let mut reader = LhaDecodeReader::new(Cursor::new(archive))
        .map_err(|error| anyhow::anyhow!("failed to parse installer LHa header: {error}"))?;
    let mut files = BTreeMap::new();

    loop {
        let name = reader.header().parse_pathname_to_str().to_ascii_uppercase();
        ensure!(!name.is_empty(), "installer contains an empty file name");
        ensure!(
            !name.contains('/') && !name.contains('\\'),
            "installer contains an unsupported nested path: {name}"
        );
        ensure!(
            reader.is_decoder_supported(),
            "installer uses an unsupported LHa method for {name}: {:?}",
            reader.header().compression_method()
        );

        let mut bytes = Vec::with_capacity(reader.header().original_size as usize);
        reader
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to decode installer entry: {name}"))?;
        reader
            .crc_check()
            .map_err(|error| anyhow::anyhow!("LHa CRC check failed for {name}: {error}"))?;
        if files.insert(name.clone(), bytes).is_some() {
            bail!("installer contains a duplicate file name: {name}");
        }

        if !reader
            .seek_next_file()
            .map_err(|error| anyhow::anyhow!("failed to parse the next installer entry: {error}"))?
        {
            break;
        }
    }

    ensure!(!files.is_empty(), "installer LHa archive is empty");
    Ok(files)
}

fn mz_executable_length(executable: &[u8]) -> Result<usize> {
    ensure!(executable.len() >= 6, "installer has a truncated MZ header");
    ensure!(
        &executable[..2] == b"MZ",
        "installer is not an MZ executable"
    );
    let last_page_bytes = u16::from_le_bytes([executable[2], executable[3]]) as usize;
    let page_count = u16::from_le_bytes([executable[4], executable[5]]) as usize;
    ensure!(page_count > 0, "installer MZ header declares zero pages");
    ensure!(
        last_page_bytes <= 512,
        "installer MZ header has an invalid last-page size"
    );
    let length = if last_page_bytes == 0 {
        page_count
            .checked_mul(512)
            .context("installer MZ length overflow")?
    } else {
        page_count
            .checked_sub(1)
            .and_then(|pages| pages.checked_mul(512))
            .and_then(|bytes| bytes.checked_add(last_page_bytes))
            .context("installer MZ length overflow")?
    };
    ensure!(
        length < executable.len(),
        "installer MZ header does not leave an appended archive"
    );
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mz_length_uses_partial_last_page() {
        let mut executable = vec![0_u8; 2_000];
        executable[..2].copy_from_slice(b"MZ");
        executable[2..4].copy_from_slice(&409_u16.to_le_bytes());
        executable[4..6].copy_from_slice(&4_u16.to_le_bytes());
        assert_eq!(mz_executable_length(&executable).unwrap(), 1_945);
    }

    #[test]
    fn mz_length_rejects_missing_archive() {
        let mut executable = vec![0_u8; 512];
        executable[..2].copy_from_slice(b"MZ");
        executable[4..6].copy_from_slice(&1_u16.to_le_bytes());
        assert!(
            mz_executable_length(&executable)
                .unwrap_err()
                .to_string()
                .contains("does not leave an appended archive")
        );
    }
}
