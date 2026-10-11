use std::env;
use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lexopt::prelude::*;

use crate::config;
use crate::feedback::{self, Notifier};
use crate::queue::{self, Decision, Entry};
use crate::state::State;

pub const USAGE: &str =
    "usage: henesys run [-w N] [-l TEXT] [-p] [--max-wait SECS] [--] <cmd> [args...]\n";

pub struct RunOpts {
    pub weight: u32,
    pub label: Option<String>,
    pub priority: bool,
    pub max_wait: Option<Duration>,
    pub program: OsString,
    pub args: Vec<OsString>,
    pub arg0: Option<OsString>,
}

// Parses the arguments after `run`. Errors are usage errors.
pub fn parse(parser: &mut lexopt::Parser) -> Result<RunOpts, String> {
    let mut weight = 1;
    let mut label = None;
    let mut priority = false;
    let mut max_wait = None;
    let mut cmd = Vec::new();
    loop {
        let arg = parser.next().map_err(|e| e.to_string())?;
        match arg {
            Some(Short('w') | Long("weight")) => {
                let v = parser.value().map_err(|e| e.to_string())?;
                weight = v
                    .to_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|&n| n > 0)
                    .ok_or_else(|| {
                        format!(
                            "weight must be a positive integer, got '{}'",
                            v.to_string_lossy()
                        )
                    })?;
            }
            Some(Short('l') | Long("label")) => {
                label = Some(
                    parser
                        .value()
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            Some(Short('p') | Long("priority")) => priority = true,
            Some(Long("max-wait")) => {
                let v = parser.value().map_err(|e| e.to_string())?;
                let secs = v
                    .to_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or_else(|| {
                        format!(
                            "max-wait must be a non-negative integer, got '{}'",
                            v.to_string_lossy()
                        )
                    })?;
                max_wait = Some(Duration::from_secs(secs));
            }
            Some(Value(v)) => {
                cmd.push(v);
                cmd.extend(parser.raw_args().map_err(|e| e.to_string())?);
                break;
            }
            None => return Err("missing command".to_string()),
            Some(other) => return Err(other.unexpected().to_string()),
        }
    }
    let program = cmd.remove(0);
    Ok(RunOpts {
        weight,
        label,
        priority,
        max_wait,
        program,
        args: cmd,
        arg0: None,
    })
}

pub fn run(opts: RunOpts) -> i32 {
    // Nested or disabled: run at once, never wait.
    if config::env_flag("HENESYS_DISABLE") || config::env_flag("HENESYS_HELD") {
        let err = command(&opts).exec();
        return spawn_failed(&opts.program, &err);
    }
    queue_and_run(opts).unwrap_or_else(|e| {
        eprintln!("{e}");
        125
    })
}

fn queue_and_run(opts: RunOpts) -> Result<i32, String> {
    let file = config::load_config().map_err(|e| format!("henesys: {e}"))?;
    let budget = config::slots(&file).map_err(|e| format!("henesys: {e}"))?;
    let state = State::open(config::state_dir())?;
    let max_wait = match opts.max_wait {
        Some(d) => Some(d),
        None => config::max_wait().map_err(|e| format!("henesys: {e}"))?,
    };

    let cwd = env::current_dir().unwrap_or_default();
    let label = match &opts.label {
        Some(l) => feedback::truncate(l, 80),
        None => {
            let argv: Vec<OsString> = std::iter::once(opts.program.clone())
                .chain(opts.args.iter().cloned())
                .collect();
            feedback::default_label(&cwd, &argv)
        }
    };
    let now = unix_now();
    let slots = queue::clamp(opts.weight, budget);
    let entry = Entry {
        pid: std::process::id(),
        slots,
        priority: opts.priority,
        arrival_ns: now.as_nanos(),
        since: now.as_secs(),
        label,
        cwd: cwd.to_string_lossy().into_owned(),
    };

    let mut ticket = state.enqueue(entry)?;
    set_name(c"henesys-wait");
    let started = Instant::now();
    let mut notifier = Notifier::new();
    let mut waited = false;
    loop {
        let (decision, snap) = state.try_admit(&mut ticket, budget)?;
        let Decision::Wait { position, busy } = decision else {
            break;
        };
        waited = true;
        let waited_s = started.elapsed().as_secs();
        let line = feedback::waiting_line(
            waited_s,
            slots,
            budget,
            busy,
            position,
            &snap.holders,
            unix_now().as_secs(),
        );
        notifier.waiting(&line, Instant::now());
        if max_wait.is_some_and(|m| started.elapsed() >= m) {
            notifier.done();
            eprintln!("{}", feedback::gave_up_line(waited_s));
            state.release(ticket);
            return Ok(75);
        }
        thread::sleep(Duration::from_secs(1));
    }
    notifier.done();
    set_name(c"henesys");
    if waited {
        let line = feedback::got_line(slots, started.elapsed().as_secs(), &ticket.entry.label);
        eprintln!("{line}");
    }

    let result = command(&opts).env("HENESYS_HELD", "1").status();
    state.release(ticket);
    Ok(match result {
        Ok(status) => exit_code(status),
        Err(e) => spawn_failed(&opts.program, &e),
    })
}

fn command(opts: &RunOpts) -> Command {
    let mut cmd = Command::new(&opts.program);
    cmd.args(&opts.args);
    if let Some(a0) = &opts.arg0 {
        cmd.arg0(a0);
    }
    cmd
}

fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

fn spawn_failed(program: &OsStr, e: &io::Error) -> i32 {
    let name = program.to_string_lossy();
    if e.kind() == io::ErrorKind::NotFound {
        eprintln!("henesys: {name}: command not found");
        127
    } else {
        eprintln!("henesys: {name}: {e}");
        126
    }
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

// Supervisors find waiters with `pgrep -x henesys-wait`.
#[cfg(target_os = "linux")]
fn set_name(name: &CStr) {
    // SAFETY: name is a valid NUL-terminated string that outlives the call.
    unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) };
}

#[cfg(not(target_os = "linux"))]
fn set_name(_name: &CStr) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_failed_codes() {
        let nf = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(spawn_failed(OsStr::new("nope"), &nf), 127);
        let other = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(spawn_failed(OsStr::new("nope"), &other), 126);
    }
}
