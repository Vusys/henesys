use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// A henesys command with an isolated state dir and no user config or leaked env.
fn henesys(dir: &Path, slots: u32) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_henesys"));
    c.env("HENESYS_DIR", dir.join("state"))
        .env("HENESYS_SLOTS", slots.to_string())
        .env("HENESYS_CONFIG", dir.join("none.toml"))
        .env_remove("HENESYS_HELD")
        .env_remove("HENESYS_DISABLE")
        .env_remove("HENESYS_MAX_WAIT");
    c
}

fn run_job(dir: &Path, slots: u32, args: &[&str]) -> Command {
    let mut c = henesys(dir, slots);
    c.arg("run").arg("--").args(args);
    c
}

fn background(mut c: Command) -> JoinHandle<Output> {
    thread::spawn(move || c.output().unwrap())
}

// Waits until the state dir shows `held` admitted and `waiting` queued, so jobs queue in a known order.
fn settle(dir: &Path, held: usize, waiting: usize) {
    let count = |sub: &str| fs::read_dir(dir.join("state").join(sub)).map_or(0, |d| d.count());
    let start = Instant::now();
    while (count("held"), count("wait")) != (held, waiting) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "state never reached held={held} wait={waiting}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn exit_code_passes_through() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_job(dir.path(), 2, &["sh", "-c", "exit 7"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn signal_exit_is_128_plus_signal() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_job(dir.path(), 2, &["sh", "-c", "kill -9 $$"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(137));
}

#[test]
fn missing_command_exits_127() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_job(dir.path(), 2, &["henesys-no-such-command"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
    assert!(stderr(&out).contains("command not found"));
}

#[test]
fn bad_weight_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = henesys(dir.path(), 2)
        .args(["run", "-w", "0", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125));
    assert!(stderr(&out).starts_with("henesys: weight must be a positive integer"));
}

#[test]
fn child_sees_held_marker() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_job(dir.path(), 2, &["sh", "-c", "echo $HENESYS_HELD"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
}

#[test]
fn third_job_waits_on_two_slots() {
    let dir = tempfile::tempdir().unwrap();
    let mut handles = Vec::new();
    handles.push(background(run_job(dir.path(), 2, &["sleep", "2"])));
    settle(dir.path(), 1, 0);
    handles.push(background(run_job(dir.path(), 2, &["sleep", "2"])));
    settle(dir.path(), 2, 0);
    handles.push(background(run_job(dir.path(), 2, &["sleep", "2"])));
    settle(dir.path(), 2, 1);
    let outs: Vec<Output> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for out in &outs {
        assert_eq!(out.status.code(), Some(0));
    }
    assert!(outs[0].stderr.is_empty(), "fast path must print nothing");
    let third = stderr(&outs[2]);
    assert!(third.contains("[henesys] waiting"), "{third}");
    assert!(third.contains("#1 in line"), "{third}");
    assert!(third.contains("got 1 slot after"), "{third}");
}

#[test]
fn priority_jumps_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let order = dir.path().join("order");
    let holder = background(run_job(dir.path(), 1, &["sleep", "2"]));
    settle(dir.path(), 1, 0);
    let script = r#"echo "$1" >> "$0""#;
    let a = background(run_job(
        dir.path(),
        1,
        &["sh", "-c", script, order.to_str().unwrap(), "A"],
    ));
    settle(dir.path(), 1, 1);
    let mut b = henesys(dir.path(), 1);
    b.args([
        "run",
        "-p",
        "--",
        "sh",
        "-c",
        script,
        order.to_str().unwrap(),
        "B",
    ]);
    let b = background(b);
    for h in [holder, a, b] {
        assert_eq!(h.join().unwrap().status.code(), Some(0));
    }
    assert_eq!(fs::read_to_string(&order).unwrap(), "B\nA\n");
}

#[test]
fn strict_head_of_line_no_backfill() {
    let dir = tempfile::tempdir().unwrap();
    let order = dir.path().join("order");
    let script = r#"echo "$1" >> "$0""#;
    let holder = background(run_job(dir.path(), 3, &["sleep", "2"]));
    settle(dir.path(), 1, 0);
    let mut a = henesys(dir.path(), 3);
    a.args([
        "run",
        "-w",
        "3",
        "--",
        "sh",
        "-c",
        script,
        order.to_str().unwrap(),
        "A",
    ]);
    let a = background(a);
    settle(dir.path(), 1, 1);
    let b = background(run_job(
        dir.path(),
        3,
        &["sh", "-c", script, order.to_str().unwrap(), "B"],
    ));
    settle(dir.path(), 1, 2);
    for h in [holder, a, b] {
        assert_eq!(h.join().unwrap().status.code(), Some(0));
    }
    assert_eq!(fs::read_to_string(&order).unwrap(), "A\nB\n");
}

#[test]
fn nested_run_does_not_wait() {
    let dir = tempfile::tempdir().unwrap();
    let inner = env!("CARGO_BIN_EXE_henesys");
    let started = Instant::now();
    let out = run_job(dir.path(), 1, &[inner, "run", "--", "echo", "inner"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "inner\n");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn disabled_runs_at_once_while_budget_is_full() {
    let dir = tempfile::tempdir().unwrap();
    let holder = background(run_job(dir.path(), 1, &["sleep", "3"]));
    thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let mut c = run_job(dir.path(), 1, &["echo", "hi"]);
    c.env("HENESYS_DISABLE", "1");
    let out = c.output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(holder.join().unwrap().status.code(), Some(0));
}

#[test]
fn max_wait_gives_up_with_75() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let holder = background(run_job(dir.path(), 1, &["sleep", "3"]));
    thread::sleep(Duration::from_millis(300));
    let mut c = henesys(dir.path(), 1);
    c.args(["run", "--max-wait", "1", "--", "touch"])
        .arg(&marker);
    let out = c.output().unwrap();
    assert_eq!(out.status.code(), Some(75));
    assert!(!marker.exists());
    assert!(stderr(&out).contains("gave up after"));
    assert_eq!(holder.join().unwrap().status.code(), Some(0));
}
