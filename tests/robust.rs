use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// A henesys command on an isolated state dir, with no user config or leaked env.
fn henesys(dir: &Path, slots: u32) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_henesys"));
    c.env("HENESYS_DIR", dir.join("state"))
        .env("HENESYS_SLOTS", slots.to_string())
        .env("HENESYS_CONFIG", dir.join("none.toml"))
        .stdin(Stdio::null())
        .env_remove("HENESYS_HELD")
        .env_remove("HENESYS_DISABLE")
        .env_remove("HENESYS_MAX_WAIT");
    c
}

// Waits for a henesys job, killing it after 30 s so a hang fails the test instead of stalling the run.
fn finish(child: &mut Child) {
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn output(c: &mut Command) -> Output {
    let mut child = c
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    finish(&mut child);
    child.wait_with_output().unwrap()
}

fn run_all(cmds: Vec<Command>) -> Vec<Output> {
    thread::scope(|s| {
        let handles: Vec<_> = cmds
            .into_iter()
            .map(|mut c| s.spawn(move || output(&mut c)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn wait_until(timeout: Duration, what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn status(dir: &Path) -> serde_json::Value {
    let out = output(henesys(dir, 1).args(["status", "--json"]));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn count(dir: &Path, key: &str) -> usize {
    status(dir)[key].as_array().unwrap().len()
}

// Reads the numbers written to files in `dir` whose names start with `prefix`.
fn read_nums(dir: &Path, prefix: &str) -> Vec<u32> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
        .map(|p| fs::read_to_string(p).unwrap().trim().parse().unwrap())
        .collect()
}

// Kills the pid written to this file, even when the test panics, so no sleep is left behind.
struct Reap(PathBuf);

impl Drop for Reap {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(&self.0) {
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn budget_is_never_exceeded() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // A running marker exists only between admission and release, so the count can only undercount.
    let script =
        "touch $D/running.$$; ls $D | grep -c running > $D/count.$$; sleep 0.3; rm $D/running.$$";
    let cmds = (0..12)
        .map(|_| {
            let mut c = henesys(d, 3);
            c.env("D", d).args(["run", "--", "sh", "-c", script]);
            c
        })
        .collect();
    for out in run_all(cmds) {
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let counts = read_nums(d, "count.");
    assert_eq!(counts.len(), 12);
    assert!(counts.iter().all(|&n| n <= 3), "{counts:?}");
    assert!(counts.iter().max().unwrap() >= &2, "{counts:?}");
}

#[test]
fn weights_are_respected() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Each child sums the weights of every marker alive at once, its own included.
    let script = "touch $D/running.$$.$1; s=0; for f in $D/running.*; do w=${f##*.}; s=$((s+w)); done; echo $s > $D/sum.$$; sleep 0.3; rm $D/running.$$.$1";
    let weights = [1, 2, 3, 1, 2, 3, 1, 2, 3, 1];
    let cmds = weights
        .iter()
        .map(|w| {
            let w = w.to_string();
            let mut c = henesys(d, 4);
            c.env("D", d)
                .args(["run", "-w", &w, "--", "sh", "-c", script, "sh", &w]);
            c
        })
        .collect();
    for out in run_all(cmds) {
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let sums = read_nums(d, "sum.");
    assert_eq!(sums.len(), 10);
    assert!(sums.iter().all(|&s| s <= 4), "{sums:?}");
}

#[test]
fn kill9_of_holder_frees_slots() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let pidfile = d.join("sleep.pid");
    let _reap = Reap(pidfile.clone());
    let wrapper = format!("echo $$ > {}; exec sleep 30", pidfile.display());
    let mut holder = henesys(d, 1)
        .args(["run", "--", "sh", "-c", &wrapper])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(Duration::from_secs(10), "holder", || {
        count(d, "holders") == 1
    });
    wait_until(Duration::from_secs(10), "pidfile", || pidfile.exists());
    holder.kill().unwrap();
    holder.wait().unwrap();
    let out = output(henesys(d, 1).args(["run", "--max-wait", "3", "--", "true"]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn kill9_of_waiter_leaves_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut holder = henesys(d, 1)
        .args(["run", "--", "sleep", "3"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(Duration::from_secs(10), "holder", || {
        count(d, "holders") == 1
    });
    let mut waiter = henesys(d, 1)
        .args(["run", "--", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(Duration::from_secs(10), "waiter", || {
        count(d, "waiters") == 1
    });
    waiter.kill().unwrap();
    waiter.wait().unwrap();
    wait_until(Duration::from_secs(2), "waiter to leave", || {
        count(d, "waiters") == 0
    });
    holder.wait().unwrap();
}

#[test]
fn same_label_runs_each_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let cmds = (0..2)
        .map(|_| {
            let mut c = henesys(d, 1);
            c.current_dir(d)
                .args(["run", "-l", "same", "--", "sh", "-c", "echo x >> runs.log"]);
            c
        })
        .collect();
    for out in run_all(cmds) {
        assert!(out.status.success(), "{}", stderr(&out));
    }
    let log = fs::read_to_string(d.join("runs.log")).unwrap();
    assert_eq!(log.lines().count(), 2, "{log}");
}

#[test]
fn stdout_streams_unbuffered() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let start = Instant::now();
    let mut child = henesys(d, 1)
        .args(["run", "--", "sh", "-c", "echo a; sleep 1; echo b"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    // Read on a thread so a henesys that never prints fails the test instead of hanging it.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = BufReader::new(stdout);
        let mut first = String::new();
        out.read_line(&mut first).unwrap();
        let first_at = start.elapsed();
        let mut rest = String::new();
        out.read_to_string(&mut rest).unwrap();
        tx.send((first, first_at, rest)).unwrap();
    });
    let (first, first_at, rest) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no output within 30 s");
    assert!(
        first_at < Duration::from_millis(800),
        "first line took {first_at:?}"
    );
    assert_eq!(first, "a\n");
    assert_eq!(rest, "b\n");
    finish(&mut child);
    assert!(child.wait().unwrap().success());
}

#[test]
fn lock_is_not_inherited_by_background_child() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let _reap = Reap(d.join("bg.pid"));
    let mut child = henesys(d, 1)
        .current_dir(d)
        .args(["run", "--", "sh", "-c", "sleep 30 & echo $! > bg.pid"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    finish(&mut child);
    assert!(child.wait().unwrap().success());
    let out = output(henesys(d, 1).args(["run", "--max-wait", "2", "--", "true"]));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn version_mismatch_exits_125() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::create_dir_all(d.join("state")).unwrap();
    fs::write(d.join("state").join("VERSION"), "99\n").unwrap();
    let out = output(henesys(d, 1).args(["run", "--", "true"]));
    assert_eq!(out.status.code(), Some(125));
    assert!(
        stderr(&out).contains("state protocol 99"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn fast_path_is_silent_and_quick() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut times: Vec<Duration> = (0..5)
        .map(|_| {
            let start = Instant::now();
            let out = output(henesys(d, 1).args(["run", "--", "true"]));
            let elapsed = start.elapsed();
            assert_eq!(out.status.code(), Some(0));
            assert_eq!(stdout(&out), "");
            assert_eq!(stderr(&out), "");
            elapsed
        })
        .collect();
    times.sort();
    assert!(
        times[2] < Duration::from_millis(200),
        "median {:?}",
        times[2]
    );
}
