use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::thread;
use std::time::{Duration, Instant};

const HENESYS: &str = env!("CARGO_BIN_EXE_henesys");

// A fake `npm` that echoes its arguments, so the test can see it ran.
fn fake_npm(dir: &Path) -> PathBuf {
    let fake = dir.join("fake");
    fs::create_dir(&fake).unwrap();
    let npm = fake.join("npm");
    fs::write(&npm, "#!/bin/sh\necho REAL \"$@\"\n").unwrap();
    fs::set_permissions(&npm, fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

fn install(shims: &Path, tools: &[&str]) -> Output {
    Command::new(HENESYS)
        .args(["shim", "install"])
        .arg(shims)
        .args(tools)
        .output()
        .unwrap()
}

// `shims/npm args` with PATH = shim dirs, then fake/, then the system dirs.
fn npm(dir: &Path, shim_dirs: &[PathBuf], args: &[&str]) -> Command {
    let mut path: Vec<PathBuf> = shim_dirs.to_vec();
    path.push(dir.join("fake"));
    path.push("/usr/bin".into());
    path.push("/bin".into());
    let mut c = Command::new(shim_dirs[0].join("npm"));
    c.args(args)
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HENESYS_DIR", dir.join("state"))
        .env("HENESYS_SLOTS", "1")
        .env("HENESYS_CONFIG", dir.join("none.toml"))
        .env_remove("HENESYS_HELD")
        .env_remove("HENESYS_DISABLE")
        .env_remove("HENESYS_MAX_WAIT");
    c
}

// Holds the only slot with `henesys run -w 1 -- sleep 3` until the state dir shows it.
fn hold_slot(dir: &Path) -> Child {
    let child = Command::new(HENESYS)
        .args(["run", "-w", "1", "--", "sleep", "3"])
        .env("HENESYS_DIR", dir.join("state"))
        .env("HENESYS_SLOTS", "1")
        .env("HENESYS_CONFIG", dir.join("none.toml"))
        .spawn()
        .unwrap();
    let held = dir.join("state").join("held");
    let start = Instant::now();
    while fs::read_dir(&held).map_or(0, |d| d.count()) != 1 {
        assert!(start.elapsed() < Duration::from_secs(10), "slot never held");
        thread::sleep(Duration::from_millis(10));
    }
    child
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn install_passthrough_runs_real_and_leaves_no_entries() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());

    let out = npm(d, &[shims], &["install"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "REAL install\n");

    let status = Command::new(HENESYS)
        .args(["status", "--json"])
        .env("HENESYS_DIR", d.join("state"))
        .env("HENESYS_CONFIG", d.join("none.toml"))
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(v["holders"], serde_json::json!([]));
    assert_eq!(v["waiters"], serde_json::json!([]));
}

#[test]
fn queued_test_runs_when_uncontended() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());

    let out = npm(d, &[shims], &["test"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), "REAL test\n");
}

#[test]
fn test_queues_behind_held_slot() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());

    let mut holder = hold_slot(d);
    let out = npm(d, &[shims], &["test"])
        .env("HENESYS_MAX_WAIT", "1")
        .output()
        .unwrap();
    holder.wait().unwrap();
    assert_eq!(out.status.code(), Some(75), "{}", stderr(&out));
    assert!(!stdout(&out).contains("REAL"));
}

#[test]
fn disable_runs_at_once_past_held_slot() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());

    let mut holder = hold_slot(d);
    let out = npm(d, &[shims], &["test"])
        .env("HENESYS_DISABLE", "1")
        .env("HENESYS_MAX_WAIT", "1")
        .output()
        .unwrap();
    holder.wait().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "REAL test\n");
}

#[test]
fn stacked_shim_dirs_reach_real() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let (a, b) = (d.join("a"), d.join("b"));
    assert!(install(&a, &["npm"]).status.success());
    assert!(install(&b, &["npm"]).status.success());

    let out = npm(d, &[a, b], &["test"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "REAL test\n");
}

#[test]
fn no_real_tool_on_path_exits_127() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());

    let out = Command::new(shims.join("npm"))
        .arg("test")
        .env("PATH", &shims)
        .env("HENESYS_CONFIG", d.join("none.toml"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
    assert!(
        stderr(&out).contains("henesys: no real 'npm' found on PATH after the henesys shims"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn install_over_plain_file_fails_and_installs_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let shims = d.join("shims");
    fs::create_dir(&shims).unwrap();
    fs::write(shims.join("npm"), "keep me").unwrap();

    let out = install(&shims, &["npm", "npx"]);
    assert_eq!(out.status.code(), Some(125));
    assert_eq!(fs::read_to_string(shims.join("npm")).unwrap(), "keep me");
    assert!(
        fs::symlink_metadata(shims.join("npx"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn bad_slots_value_warns_and_runs_real() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());
    fs::write(d.join("none.toml"), "slots = \"x\"\n").unwrap();

    // The helper sets HENESYS_SLOTS, which would hide the bad config value.
    let out = npm(d, &[shims], &["test"])
        .env_remove("HENESYS_SLOTS")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("running npm without the queue"),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "REAL test\n");
}

#[test]
fn malformed_config_warns_and_runs_real() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fake_npm(d);
    let shims = d.join("shims");
    assert!(install(&shims, &["npm"]).status.success());
    fs::write(d.join("none.toml"), "slots = ").unwrap();

    let out = npm(d, &[shims], &["test"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("running npm without the queue"),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "REAL test\n");
}
