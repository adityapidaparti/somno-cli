//! On-disk state in an isolated SOMNO_CONFIG_DIR. Kept in its own test binary
//! because it sets a process-wide environment variable.

use std::os::unix::fs::PermissionsExt;

#[test]
fn private_files_round_trip_and_remove() {
    let dir = std::env::temp_dir().join(format!("somno-store-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("SOMNO_CONFIG_DIR", &dir);

    assert_eq!(somno::store::read::<serde_json::Value>("x.json").unwrap(), None);
    let path = somno::store::write("x.json", &serde_json::json!({"token": "t"})).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(!dir.join(".x.json.tmp").exists(), "temp file is renamed away");
    assert_eq!(somno::store::read::<serde_json::Value>("x.json").unwrap().unwrap()["token"], "t");

    {
        let _held = somno::store::lock("x.lock").unwrap();
        assert!(std::fs::File::open(dir.join("x.lock")).unwrap().try_lock().is_err(), "lock is exclusive");
    }
    assert!(std::fs::File::open(dir.join("x.lock")).unwrap().try_lock().is_ok(), "released on drop");

    std::fs::write(dir.join("bad.json"), "{not json").unwrap();
    assert!(somno::store::read::<serde_json::Value>("bad.json").is_err(), "corrupt files are errors, not empty");

    assert!(somno::store::remove("x.json").unwrap());
    assert!(!somno::store::remove("x.json").unwrap());

    // The log defaults to the config dir, is private, and can be turned off.
    somno::log::event("test", "first");
    somno::log::event("test", format_args!("second {}", 2));
    let log = dir.join("somno.log");
    assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
    let tail = somno::log::tail(1).unwrap();
    assert_eq!(tail.len(), 1);
    assert!(tail[0].ends_with(" test second 2"), "{}", tail[0]);
    std::env::set_var("SOMNO_LOG", "off");
    somno::log::event("test", "dropped");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
    std::env::remove_var("SOMNO_LOG");
    std::fs::remove_dir_all(&dir).unwrap();
}
