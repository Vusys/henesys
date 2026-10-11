use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// A henesys command with an isolated state dir and no user config or leaked env.
fn henesys(dir: &Path, slots: u32) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_henesys"));
    isolate(&mut c, dir, slots);
    c
}

fn isolate(c: &mut Command, dir: &Path, slots: u32) {
    c.env("HENESYS_DIR", dir.join("state"))
        .env("HENESYS_SLOTS", slots.to_string())
        .env("HENESYS_CONFIG", dir.join("none.toml"))
        .env_remove("HENESYS_HELD")
        .env_remove("HENESYS_DISABLE")
        .env_remove("HENESYS_MAX_WAIT");
}

// Polls `check` for up to 3 s.
fn within_3s(mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !check() {
        if start.elapsed() > Duration::from_secs(3) {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

// A killed process stays a zombie until its parent reaps it, so `kill 0` alone is not enough.
fn is_dead(pid: i32) -> bool {
    // SAFETY: kill with signal 0 only probes the pid.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return true;
    }
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit(')')
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with('Z'))
    })
}

#[test]
fn sigterm_kills_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("gc.pid");
    let script = r#"sleep 30 & echo $! > "$0"; wait"#;
    let mut run = henesys(dir.path(), 1);
    run.args(["run", "--", "sh", "-c", script, pidfile.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = run.spawn().unwrap();

    let read_pid = || {
        fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
    };
    assert!(
        within_3s(|| read_pid().is_some()),
        "grandchild never started"
    );
    let gc = read_pid().unwrap();

    // Henesys signals its child's group, not its own: this test process surviving to the
    // end is the proof that it did not kill itself.
    // SAFETY: plain kill of a pid we spawned.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let mut status = None;
    assert!(
        within_3s(|| {
            status = child.try_wait().unwrap();
            status.is_some()
        }),
        "henesys did not exit after SIGTERM"
    );
    assert_eq!(status.unwrap().code(), Some(143));
    assert!(within_3s(|| is_dead(gc)), "grandchild survived SIGTERM");

    // The slot was released, so a second job on the same 1-slot budget gets in.
    let out = henesys(dir.path(), 1)
        .args(["run", "--max-wait", "2", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

// A signal the caller ignores must stay ignored in the child, not fall back to default.
#[cfg(target_os = "linux")]
#[test]
fn ignored_signal_stays_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"trap '' INT; exec "$0" run -- grep SigIgn /proc/self/status"#;
    let mut c = Command::new("sh");
    c.args(["-c", script, env!("CARGO_BIN_EXE_henesys")]);
    isolate(&mut c, dir.path(), 1);
    let out = c.output().unwrap();
    // Bit 1 is SIGINT.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("SigIgn:\t0000000000000002"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn child_gets_the_tty() {
    let dir = tempfile::tempdir().unwrap();
    let cmd = format!(
        "{} run -- sh -c 'test -t 1 && echo IS_TTY'",
        env!("CARGO_BIN_EXE_henesys")
    );
    let mut script = Command::new("script");
    script
        .args(["-qec", &cmd, "/dev/null"])
        .env("SHELL", "/bin/sh");
    isolate(&mut script, dir.path(), 1);
    let out = script.output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("IS_TTY"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(out.status.success());
}
