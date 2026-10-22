//! `somno oura configure` without a terminal prompt. A dead proxy stands in
//! for the network, so nothing reaches Oura.

use std::process::{Command, Stdio};

#[test]
fn configure_takes_the_client_from_the_environment_without_prompting() {
    let dir = std::env::temp_dir().join(format!("somno-configure-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = Command::new(env!("CARGO_BIN_EXE_somno"))
        .args(["oura", "configure"])
        .env("SOMNO_CONFIG_DIR", &dir)
        .env("SOMNO_LOG", "off")
        .env("SOMNO_OURA_CLIENT_ID", "test-client-id")
        .env("SOMNO_OURA_CLIENT_SECRET", "test-client-secret")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("required"), "it prompted instead of using the environment: {stderr}");
    // Oura can't be reached, so configure saves with a warning, as documented.
    let saved = std::fs::read_to_string(dir.join("oura-client.json")).unwrap_or_else(|e| panic!("{e}; stderr: {stderr}"));
    assert!(saved.contains("test-client-id"), "{saved}");
    std::fs::remove_dir_all(&dir).unwrap();
}
