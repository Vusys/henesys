use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::queue::Entry;

// These strings are frozen (#1): agents and supervisors grep for them.
pub fn waiting_line(
    waited_s: u64,
    want: u32,
    budget: u32,
    busy: u32,
    position: usize,
    holders: &[Entry],
    now_s: u64,
) -> String {
    let list = if holders.is_empty() {
        "nothing".to_string()
    } else {
        holders
            .iter()
            .map(|h| {
                let unit = if h.slots == 1 { "slot" } else { "slots" };
                let age = fmt_age(now_s.saturating_sub(h.since));
                format!("{} ({} {unit}, {age})", h.label, h.slots)
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    format!(
        "[henesys] waiting {waited_s}s for {want} of {budget} slots ({busy} busy), #{position} in line. Running: {list}. This is expected; it starts by itself. Do not re-run this command."
    )
}

pub fn got_line(slots: u32, waited_s: u64, label: &str) -> String {
    let unit = if slots == 1 { "slot" } else { "slots" };
    format!("[henesys] got {slots} {unit} after {waited_s}s: {label}")
}

pub fn gave_up_line(waited_s: u64) -> String {
    format!("[henesys] gave up after {waited_s}s waiting; the command did not run.")
}

pub fn fmt_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub fn default_label(cwd: &Path, argv: &[OsString]) -> String {
    let base = cwd.file_name().unwrap_or(cwd.as_os_str());
    let args: Vec<_> = argv.iter().map(|a| a.to_string_lossy()).collect();
    truncate(
        &format!("{}: {}", base.to_string_lossy(), args.join(" ")),
        80,
    )
}

// Counts chars, not bytes, so a cut never splits a code point.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub struct Notifier {
    tty: bool,
    last: Option<Instant>,
    drawn: bool,
}

impl Notifier {
    pub fn new() -> Notifier {
        Notifier {
            tty: unsafe { libc::isatty(2) } == 1,
            last: None,
            drawn: false,
        }
    }

    pub fn waiting(&mut self, line: &str, now: Instant) {
        if self.tty {
            let msg = truncate(line, term_width().saturating_sub(1));
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\x1b[2K{msg}");
            let _ = err.flush();
            self.drawn = true;
        } else if due(self.last, now) {
            let _ = writeln!(std::io::stderr(), "{line}");
            self.last = Some(now);
        }
    }

    pub fn done(&mut self) {
        if self.drawn {
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }
}

// Non-tty output repeats at most every 30 s.
fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|l| now.duration_since(l) >= Duration::from_secs(30))
}

fn term_width() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, slots: u32, since: u64) -> Entry {
        Entry {
            pid: 1,
            slots,
            priority: false,
            arrival_ns: 0,
            since,
            label: label.to_string(),
            cwd: String::new(),
        }
    }

    #[test]
    fn waiting_line_with_no_holders() {
        assert_eq!(
            waiting_line(5, 2, 6, 0, 1, &[], 10),
            "[henesys] waiting 5s for 2 of 6 slots (0 busy), #1 in line. Running: nothing. This is expected; it starts by itself. Do not re-run this command."
        );
    }

    #[test]
    fn waiting_line_with_one_holder_uses_singular_slot() {
        let h = entry("build", 1, 0);
        assert_eq!(
            waiting_line(3, 1, 4, 1, 1, &[h], 50),
            "[henesys] waiting 3s for 1 of 4 slots (1 busy), #1 in line. Running: build (1 slot, 50s). This is expected; it starts by itself. Do not re-run this command."
        );
    }

    #[test]
    fn waiting_line_with_two_holders() {
        let web = entry("web: npm run build", 3, 0);
        let api = entry("api: npm test", 2, 100);
        assert_eq!(
            waiting_line(90, 2, 6, 5, 2, &[web, api], 120),
            "[henesys] waiting 90s for 2 of 6 slots (5 busy), #2 in line. Running: web: npm run build (3 slots, 2m); api: npm test (2 slots, 20s). This is expected; it starts by itself. Do not re-run this command."
        );
    }

    #[test]
    fn fmt_age_units() {
        assert_eq!(fmt_age(45), "45s");
        assert_eq!(fmt_age(180), "3m");
        assert_eq!(fmt_age(3900), "1h05m");
    }

    #[test]
    fn got_line_singular_and_plural() {
        assert_eq!(got_line(1, 4, "x"), "[henesys] got 1 slot after 4s: x");
        assert_eq!(got_line(2, 0, "y"), "[henesys] got 2 slots after 0s: y");
    }

    #[test]
    fn gave_up_line_text() {
        assert_eq!(
            gave_up_line(75),
            "[henesys] gave up after 75s waiting; the command did not run."
        );
    }

    #[test]
    fn truncate_counts_chars_and_keeps_boundary() {
        assert_eq!(truncate("héllo wörld", 20), "héllo wörld");
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("abcde", 5), "abcde");
        assert_eq!(truncate("abcdef", 5), "abcd…");
    }

    #[test]
    fn default_label_uses_basename_and_truncates() {
        let argv = [
            OsString::from("npm"),
            OsString::from("run"),
            OsString::from("build"),
        ];
        assert_eq!(
            default_label(Path::new("/x/proj"), &argv),
            "proj: npm run build"
        );
        let long = [OsString::from("a".repeat(100))];
        let label = default_label(Path::new("/x/proj"), &long);
        assert_eq!(label.chars().count(), 80);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn due_cadence() {
        let t = Instant::now();
        assert!(due(None, t));
        assert!(!due(Some(t), t + Duration::from_secs(29)));
        assert!(due(Some(t), t + Duration::from_secs(30)));
    }
}
