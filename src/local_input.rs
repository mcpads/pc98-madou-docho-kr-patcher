//! Locally supplied inputs that the public repository does not contain.
//!
//! The Galmuri14 font, the title artwork and the translation catalogs are read
//! from an input directory at run time. The default is `assets` relative to the
//! current directory; the command line changes it with `--assets`. Tests that
//! need these inputs are marked `#[ignore = "requires ..."]` and run with
//! `cargo test -- --ignored` once the named files are in place.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};

pub const DEFAULT_INPUT_DIR: &str = "assets";

static INPUT_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Select the input directory. Call once, before any build step reads an input.
pub fn set_input_dir(path: PathBuf) -> Result<()> {
    INPUT_DIR
        .set(path)
        .map_err(|_| anyhow!("the input directory is already selected"))
}

pub(crate) fn input_dir() -> &'static Path {
    INPUT_DIR.get_or_init(|| PathBuf::from(DEFAULT_INPUT_DIR))
}

pub(crate) fn read(relative_path: &str) -> Result<Vec<u8>> {
    let path = input_dir().join(relative_path);
    std::fs::read(&path)
        .with_context(|| format!("required input {} is unavailable", path.display()))
}

pub(crate) fn read_to_string(relative_path: &str) -> Result<String> {
    let bytes = read(relative_path)?;
    String::from_utf8(bytes).with_context(|| format!("input {relative_path} is not valid UTF-8"))
}
