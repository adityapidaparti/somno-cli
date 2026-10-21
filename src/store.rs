//! Private on-disk state: no background process, tokens are refreshed on demand.
//!
//! Layout (macOS: `~/Library/Application Support/somno/`, override with
//! `SOMNO_CONFIG_DIR`): `oura-client.json` (your developer app),
//! `oura-tokens.json`, `eight.json` (session token) and, only after
//! `eight login --remember`, `eight-credentials.json`. The directory is 0700,
//! files are 0600, and writes go through a temp file + rename so a crash never
//! leaves a half-written token file.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SOMNO_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let dirs = directories::ProjectDirs::from("", "", "somno")
        .context("could not determine config directory")?;
    Ok(dirs.config_dir().to_path_buf())
}

fn private_dir() -> Result<PathBuf> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {} (is it writable from here?)", dir.display()))?;
    }
    Ok(dir)
}

fn open_private(path: &PathBuf, truncate: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub fn read<T: DeserializeOwned>(name: &str) -> Result<Option<T>> {
    let path = config_dir()?.join(name);
    match fs::read_to_string(&path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("corrupt {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn write<T: Serialize>(name: &str, value: &T) -> Result<PathBuf> {
    let dir = private_dir()?;
    let path = dir.join(name);
    let temp = dir.join(format!(".{name}.tmp"));
    let mut file = open_private(&temp, true).with_context(|| format!("writing {}", temp.display()))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&temp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(path)
}

/// Returns whether a file was actually removed.
pub fn remove(name: &str) -> Result<bool> {
    let path = config_dir()?.join(name);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Exclusive advisory lock held until the returned file is dropped. Serializes
/// token refreshes across concurrent invocations: Oura refresh tokens are
/// single-use, so two processes refreshing at once would break the login.
pub fn lock(name: &str) -> Result<File> {
    let path = private_dir()?.join(name);
    let file = open_private(&path, false).with_context(|| format!("opening {}", path.display()))?;
    file.lock().with_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}
