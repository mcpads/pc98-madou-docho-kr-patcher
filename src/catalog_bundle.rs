use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;

pub(crate) fn read_catalog_json<T: DeserializeOwned>(path: &Path, role: &str) -> Result<T> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read {role}: {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {role}: {}", path.display()))
}

pub(crate) fn resolve_catalog_part_path(
    catalog_directory: &Path,
    relative_path: &str,
    role: &str,
) -> Result<PathBuf> {
    let relative = Path::new(relative_path);
    ensure!(
        !relative_path.is_empty()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "{role} path must stay inside its catalog directory: {relative_path:?}"
    );
    Ok(catalog_directory.join(relative))
}
