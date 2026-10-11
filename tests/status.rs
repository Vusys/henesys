use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

// Builds the state directly, so this does not depend on `run` (#7).
fn henesys(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_henesys"))
        .args(args)
        .env("HENESYS_DIR", root)
        .env("HENESYS_SLOTS", "1")
        .env("HENESYS_CONFIG", root.join("missing.toml"))
        .output()
        .expect("run henesys")
}

// Keep the returned file open: its lock is the entry's liveness.
fn place(root: &Path, sub: &str, name: &str, pid: u32, label: &str) -> File {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let entry = format!(
        r#"{{"pid":{pid},"slots":1,"priority":false,"arrival_ns":1,"since":{since},"label":"{label}","cwd":"/x"}}"#
    );
    let mut file = File::create(root.join(sub).join(name)).unwrap();
    file.lock().unwrap();
    file.write_all(entry.as_bytes()).unwrap();
    file
}

#[test]
fn idle_box_reports_without_creating_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("henesys");
    let out = henesys(&root, &["status"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("0 busy"), "{text}");
    assert!(!root.exists());

    let out = henesys(&root, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["holders"], serde_json::json!([]));
    assert_eq!(v["waiters"], serde_json::json!([]));
    assert!(!root.exists());
}

#[test]
fn one_holder_and_one_waiter() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("henesys");
    fs::create_dir_all(root.join("held")).unwrap();
    fs::create_dir_all(root.join("wait")).unwrap();
    let _held = place(
        &root,
        "held",
        "0-00000000000000000001-100.json",
        100,
        "hold",
    );
    let _wait = place(
        &root,
        "wait",
        "1-00000000000000000002-200.json",
        200,
        "waitme",
    );

    let out = henesys(&root, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["busy"], 1);
    assert_eq!(v["holders"][0]["pid"], 100);
    assert_eq!(v["holders"][0]["label"], "hold");
    assert_eq!(v["waiters"][0]["pid"], 200);
    assert_eq!(v["waiters"][0]["label"], "waitme");
    assert_eq!(v["waiters"][0]["position"], 1);

    let out = henesys(&root, &["status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("wait#1"), "{text}");
    assert!(text.contains("waitme"), "{text}");
}

#[test]
fn extra_argument_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = henesys(&dir.path().join("henesys"), &["status", "extra"]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("henesys: "));
}
