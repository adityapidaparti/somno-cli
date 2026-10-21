//! `somno skill install` writes the skill bundled in the binary.

use std::path::PathBuf;
use std::process::Command;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("somno-skill-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn install(dir: &PathBuf) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_somno")).args(["skill", "install", "--dir"]).arg(dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn installs_the_bundled_skill_once() {
    let dir = scratch("fresh");
    assert!(install(&dir).starts_with("installed "));
    let written = std::fs::read_to_string(dir.join("somno/SKILL.md")).unwrap();
    assert_eq!(written, include_str!("../skills/somno/SKILL.md"));
    assert!(install(&dir).starts_with("already up to date: "));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn leaves_a_linked_skill_alone() {
    let dir = scratch("linked");
    let checkout = scratch("checkout");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("SKILL.md"), "local edits").unwrap();
    std::os::unix::fs::symlink(&checkout, dir.join("somno")).unwrap();
    assert!(install(&dir).contains("left it as it is"));
    assert_eq!(std::fs::read_to_string(checkout.join("SKILL.md")).unwrap(), "local edits");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&checkout).unwrap();
}
