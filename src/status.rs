use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lexopt::prelude::*;
use serde::Serialize;

use crate::config;
use crate::feedback::fmt_age;
use crate::queue::{self, Entry};
use crate::state::{Snapshot, State};

// Usage and henesys errors exit 125 (#1).
pub fn run(parser: &mut lexopt::Parser) -> i32 {
    let mut json = false;
    loop {
        match parser.next() {
            Ok(Some(Long("json"))) => json = true,
            Ok(Some(arg)) => {
                eprintln!("henesys: {}", arg.unexpected());
                return 125;
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("henesys: {e}");
                return 125;
            }
        }
    }
    let (dir, budget, snap) = match load() {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("{e}");
            return 125;
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let out = if json {
        serde_json::to_string_pretty(&report(&snap, budget, &dir, now))
            .expect("plain structs always serialize")
    } else {
        text(&snap, budget, &dir, now)
    };
    // Ignore write errors: a closed pipe (`| head`) must not panic.
    let _ = writeln!(std::io::stdout(), "{out}");
    0
}

fn load() -> Result<(PathBuf, u32, Snapshot), String> {
    let budget = config::slots(&config::load_config()?)?;
    let dir = config::state_dir();
    // Reporting on an idle box must not create the state dir.
    let snap = if dir.exists() {
        State::open(dir.clone())?.snapshot()?
    } else {
        Snapshot {
            holders: Vec::new(),
            waiters: Vec::new(),
        }
    };
    Ok((dir, budget, snap))
}

fn text(snap: &Snapshot, budget: u32, dir: &Path, now: u64) -> String {
    let mut lines = vec![format!(
        "{}, {} busy, {} waiting  ({})",
        count_slots(budget),
        queue::busy(&snap.holders),
        snap.waiters.len(),
        dir.display()
    )];
    for h in &snap.holders {
        lines.push(row("held", h, now.saturating_sub(h.since), ""));
    }
    for (i, w) in snap.waiters.iter().enumerate() {
        let note = if w.priority { "  (priority)" } else { "" };
        let kind = format!("wait#{}", i + 1);
        lines.push(row(&kind, w, now.saturating_sub(w.since), note));
    }
    lines.join("\n")
}

fn row(kind: &str, e: &Entry, age_s: u64, note: &str) -> String {
    format!(
        "{kind:<8}{:<9}{:<5}pid {}  {}{note}",
        count_slots(e.slots),
        fmt_age(age_s),
        e.pid,
        // Labels come from other users' argv; keep escape codes out of the terminal.
        e.label.replace(|c: char| c.is_control(), "?")
    )
}

fn count_slots(n: u32) -> String {
    if n == 1 {
        "1 slot".to_string()
    } else {
        format!("{n} slots")
    }
}

// Frozen interface (#9): supervisors parse this, so keep key order and names.
#[derive(Serialize)]
struct Report<'a> {
    version: u32,
    dir: String,
    slots: u32,
    busy: u32,
    holders: Vec<Holder<'a>>,
    waiters: Vec<Waiter<'a>>,
}

#[derive(Serialize)]
struct Holder<'a> {
    pid: u32,
    slots: u32,
    label: &'a str,
    cwd: &'a str,
    since: u64,
    age_s: u64,
}

#[derive(Serialize)]
struct Waiter<'a> {
    pid: u32,
    slots: u32,
    label: &'a str,
    cwd: &'a str,
    since: u64,
    waited_s: u64,
    position: usize,
    priority: bool,
}

fn report<'a>(snap: &'a Snapshot, budget: u32, dir: &Path, now: u64) -> Report<'a> {
    Report {
        version: 1,
        dir: dir.display().to_string(),
        slots: budget,
        busy: queue::busy(&snap.holders),
        holders: snap
            .holders
            .iter()
            .map(|h| Holder {
                pid: h.pid,
                slots: h.slots,
                label: &h.label,
                cwd: &h.cwd,
                since: h.since,
                age_s: now.saturating_sub(h.since),
            })
            .collect(),
        waiters: snap
            .waiters
            .iter()
            .enumerate()
            .map(|(i, w)| Waiter {
                pid: w.pid,
                slots: w.slots,
                label: &w.label,
                cwd: &w.cwd,
                since: w.since,
                waited_s: now.saturating_sub(w.since),
                position: i + 1,
                priority: w.priority,
            })
            .collect(),
    }
}
