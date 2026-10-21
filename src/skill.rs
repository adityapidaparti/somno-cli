//! The agent skill that ships inside the binary, so `somno skill install`
//! always installs the skill written for this version of the CLI.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const NAME: &str = "somno";
pub const SKILL: &str = include_str!("../skills/somno/SKILL.md");

/// `~/.claude/skills`, which Claude Code and Grok CLI both read.
pub fn default_dir() -> Result<PathBuf> {
    let home = directories::BaseDirs::new().context("could not find your home directory; pass --dir")?;
    Ok(home.home_dir().join(".claude").join("skills"))
}

/// Write the skill to `<dir>/<name>/SKILL.md` and say what happened. A
/// `<dir>/<name>` that is a symbolic link (to a checkout, say) is left alone,
/// and a file that already matches is not rewritten.
pub fn install(dir: &Path, name: &str, skill: &str) -> Result<String> {
    let target = dir.join(name);
    if let Ok(link) = fs::read_link(&target) {
        return Ok(format!("{} links to {}; left it as it is", target.display(), link.display()));
    }
    fs::create_dir_all(&target).with_context(|| format!("could not create {}", target.display()))?;
    let file = target.join("SKILL.md");
    if fs::read_to_string(&file).is_ok_and(|current| current == skill) {
        return Ok(format!("already up to date: {}", file.display()));
    }
    fs::write(&file, skill).with_context(|| format!("could not write {}", file.display()))?;
    Ok(format!("installed {}", file.display()))
}
