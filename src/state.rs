use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::queue::{self, Decision, Entry};

const PROTOCOL: &str = "1";

pub struct State {
    dir: PathBuf,
    // The state dir's mode when it is group-shared; new paths inherit its bits.
    shared: Option<u32>,
}

// The file stays locked until dropped; that lock is the entry's liveness.
pub struct Ticket {
    file: File,
    path: PathBuf,
    pub entry: Entry,
}

pub struct Snapshot {
    pub holders: Vec<Entry>,
    pub waiters: Vec<Entry>,
}

impl State {
    pub fn open(dir: PathBuf) -> Result<State, String> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| io_err(&dir, e))?;
        let uid = rustix::process::geteuid().as_raw();
        // Only the predictable /tmp fallback can be planted by another user; a shared HENESYS_DIR is group-owned.
        if dir == Path::new(&format!("/tmp/henesys-{uid}")) {
            check_owner(&dir, uid)?;
        }
        // A group-shared dir (`install -d -m 2770`) lends its bits to everything henesys creates inside it.
        let root = fs::metadata(&dir).map_err(|e| io_err(&dir, e))?.mode() & 0o7777;
        let shared = (root & 0o070 != 0).then_some(root);
        for sub in ["wait", "held"] {
            let path = dir.join(sub);
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => share(&path, shared, 0o2770)?,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_err(&path, e)),
            }
        }
        let state = State { dir, shared };
        // Locking needs no write access, so .mutex is created once here and only opened read-only after.
        let mutex = state.dir.join(".mutex");
        match OpenOptions::new().write(true).create_new(true).open(&mutex) {
            Ok(_) => share(&mutex, shared, 0o660)?,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(&mutex, e)),
        }
        let _m = state.lock_mutex()?;
        let version = state.dir.join("VERSION");
        // Written whole under the mutex, then renamed, so a crash never leaves a half-written VERSION.
        if !version.exists() {
            let tmp = state.dir.join("VERSION.tmp");
            // A crash may have left a tmp the group cannot write; clear it first.
            let _ = fs::remove_file(&tmp);
            fs::write(&tmp, "1\n").map_err(|e| io_err(&tmp, e))?;
            share(&tmp, shared, 0o660)?;
            fs::rename(&tmp, &version).map_err(|e| io_err(&version, e))?;
        }
        let v = fs::read_to_string(&version).map_err(|e| io_err(&version, e))?;
        if v.trim() != PROTOCOL {
            return Err(format!(
                "henesys: {} uses state protocol {}; this henesys speaks {PROTOCOL}. Use a matching henesys or a different HENESYS_DIR.",
                state.dir.display(),
                v.trim()
            ));
        }
        Ok(state)
    }

    pub fn enqueue(&self, entry: Entry) -> Result<Ticket, String> {
        let _m = self.lock_mutex()?;
        let path = self.dir.join("wait").join(file_name(&entry));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| io_err(&path, e))?;
        share(&path, self.shared, 0o640)?;
        // Locked before the mutex drops, so a sweeper never sees it unlocked.
        file.lock().map_err(|e| io_err(&path, e))?;
        let mut t = Ticket { file, path, entry };
        t.save()?;
        Ok(t)
    }

    // The caller's entry.slots must already be clamped to budget.
    pub fn try_admit(&self, t: &mut Ticket, budget: u32) -> Result<(Decision, Snapshot), String> {
        let _m = self.lock_mutex()?;
        let snap = self.sweep()?;
        let decision = queue::decide(budget, &snap.holders, &snap.waiters, &t.entry);
        if decision == Decision::Admit {
            let to = self.dir.join("held").join(file_name(&t.entry));
            fs::rename(&t.path, &to).map_err(|e| io_err(&t.path, e))?;
            t.path = to;
            t.entry.since = now_secs();
            t.save()?;
        }
        Ok((decision, snap))
    }

    // #9 status reads this; nothing in run needs a snapshot without admission.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let _m = self.lock_mutex()?;
        self.sweep()
    }

    pub fn release(&self, t: Ticket) {
        if let Ok(_m) = self.lock_mutex() {
            let _ = fs::remove_file(&t.path);
        }
        // t drops here, which unlocks the file.
    }

    // Caller holds the mutex.
    fn sweep(&self) -> Result<Snapshot, String> {
        let mut snap = Snapshot {
            holders: Vec::new(),
            waiters: Vec::new(),
        };
        scan(&self.dir.join("wait"), &mut snap.waiters)?;
        scan(&self.dir.join("held"), &mut snap.holders)?;
        queue::sort_waiters(&mut snap.waiters);
        snap.holders.sort_by_key(|h| h.since);
        Ok(snap)
    }

    // Held for one read or change of wait/ and held/; never across a command or a sleep.
    fn lock_mutex(&self) -> Result<File, String> {
        let path = self.dir.join(".mutex");
        let file = File::open(&path).map_err(|e| io_err(&path, e))?;
        file.lock().map_err(|e| io_err(&path, e))?;
        Ok(file)
    }
}

impl Ticket {
    // Rewrites through the locked fd, so the lock never leaves the file.
    fn save(&mut self) -> Result<(), String> {
        let json = serde_json::to_string(&self.entry)
            .map_err(|e| format!("henesys: {}: {e}", self.path.display()))?;
        self.file.set_len(0).map_err(|e| io_err(&self.path, e))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| io_err(&self.path, e))?;
        self.file
            .write_all(json.as_bytes())
            .map_err(|e| io_err(&self.path, e))
    }
}

// Deletes files nobody holds a lock on (a dead owner); keeps the rest.
fn scan(dir: &Path, out: &mut Vec<Entry>) -> Result<(), String> {
    for item in fs::read_dir(dir).map_err(|e| io_err(dir, e))? {
        let path = item.map_err(|e| io_err(dir, e))?.path();
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err(&path, e)),
        };
        match file.try_lock() {
            Ok(()) => fs::remove_file(&path).map_err(|e| io_err(&path, e))?,
            Err(TryLockError::WouldBlock) => {
                let text = fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
                // An unparsable live file is skipped, not fatal.
                if let Ok(entry) = serde_json::from_str::<Entry>(&text) {
                    out.push(entry);
                }
            }
            Err(TryLockError::Error(e)) => return Err(io_err(&path, e)),
        }
    }
    Ok(())
}

// Priority files sort first by name; arrival and pid break ties.
fn file_name(e: &Entry) -> String {
    let p = if e.priority { 0 } else { 1 };
    format!("{p}-{:020}-{}.json", e.arrival_ns, e.pid)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// Gives a path henesys just created the shared dir's bits, masked; the umask would strip group write. No-op when private.
fn share(path: &Path, shared: Option<u32>, mask: u32) -> Result<(), String> {
    let Some(root) = shared else {
        return Ok(());
    };
    fs::set_permissions(path, fs::Permissions::from_mode(root & mask)).map_err(|e| io_err(path, e))
}

fn check_owner(dir: &Path, uid: u32) -> Result<(), String> {
    let owner = fs::metadata(dir).map_err(|e| io_err(dir, e))?.uid();
    if owner == uid {
        return Ok(());
    }
    Err(format!(
        "henesys: {} is owned by uid {owner}, not uid {uid}; use a different HENESYS_DIR",
        dir.display()
    ))
}

fn io_err(path: &Path, e: std::io::Error) -> String {
    format!("henesys: {}: {e}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn entry(pid: u32, slots: u32, arrival_ns: u128) -> Entry {
        Entry {
            pid,
            slots,
            priority: false,
            arrival_ns,
            since: 0,
            label: String::new(),
            cwd: String::new(),
        }
    }

    fn open() -> (tempfile::TempDir, State) {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path().join("henesys")).unwrap();
        (dir, state)
    }

    // Drops the lock but keeps the file, as a crash would.
    fn crash(t: Ticket) -> PathBuf {
        let Ticket { file, path, .. } = t;
        drop(file);
        path
    }

    #[test]
    fn round_trip() {
        let (_d, s) = open();
        let mut a = s.enqueue(entry(1, 2, 1)).unwrap();
        assert_eq!(s.try_admit(&mut a, 3).unwrap().0, Decision::Admit);
        let snap = s.snapshot().unwrap();
        assert_eq!(snap.holders.len(), 1);
        assert_eq!(snap.holders[0].pid, 1);
        assert!(snap.waiters.is_empty());
        s.release(a);
        let snap = s.snapshot().unwrap();
        assert!(snap.holders.is_empty() && snap.waiters.is_empty());
    }

    #[test]
    fn waits_until_holder_releases() {
        let (_d, s) = open();
        let mut a = s.enqueue(entry(1, 3, 1)).unwrap();
        assert_eq!(s.try_admit(&mut a, 3).unwrap().0, Decision::Admit);
        let mut b = s.enqueue(entry(2, 1, 2)).unwrap();
        assert_eq!(
            s.try_admit(&mut b, 3).unwrap().0,
            Decision::Wait {
                position: 1,
                busy: 3
            }
        );
        s.release(a);
        assert_eq!(s.try_admit(&mut b, 3).unwrap().0, Decision::Admit);
    }

    #[test]
    fn snapshot_sweeps_unlocked_file() {
        let (_d, s) = open();
        let path = s.dir.join("held").join("1-00000000000000000001-99.json");
        fs::write(&path, serde_json::to_string(&entry(99, 1, 1)).unwrap()).unwrap();
        let snap = s.snapshot().unwrap();
        assert!(snap.holders.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn own_ticket_survives_snapshot() {
        let (_d, s) = open();
        let a = s.enqueue(entry(1, 1, 1)).unwrap();
        let snap = s.snapshot().unwrap();
        assert_eq!(snap.waiters, vec![a.entry.clone()]);
        assert!(a.path.exists());
        s.release(a);
    }

    #[test]
    fn version_mismatch_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(state_dir.join("VERSION"), "99\n").unwrap();
        let err = State::open(state_dir.clone())
            .err()
            .expect("open should fail");
        assert_eq!(
            err,
            format!(
                "henesys: {} uses state protocol 99; this henesys speaks 1. Use a matching henesys or a different HENESYS_DIR.",
                state_dir.display()
            )
        );
    }

    #[test]
    fn check_owner_accepts_own_dir_and_rejects_others() {
        let dir = tempfile::tempdir().unwrap();
        let uid = rustix::process::geteuid().as_raw();
        assert_eq!(check_owner(dir.path(), uid), Ok(()));
        assert!(check_owner(dir.path(), uid.wrapping_add(1)).is_err());
    }

    #[test]
    fn stray_version_tmp_from_crash_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(state_dir.join("VERSION.tmp"), "").unwrap();
        State::open(state_dir.clone()).unwrap();
        assert_eq!(
            fs::read_to_string(state_dir.join("VERSION")).unwrap(),
            "1\n"
        );
    }

    #[test]
    fn lock_is_not_inherited_by_child() {
        let (_d, s) = open();
        let mut a = s.enqueue(entry(1, 1, 1)).unwrap();
        s.try_admit(&mut a, 1).unwrap();
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        let path = crash(a);
        assert!(child.try_wait().unwrap().is_none());
        let fresh = State::open(s.dir.clone()).unwrap();
        // Other tests fork in parallel; a forked child holds the fd until its exec closes it, so allow a moment.
        let swept = (0..50).any(|_| {
            let empty = fresh.snapshot().unwrap().holders.is_empty();
            if !empty {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            empty
        });
        assert!(swept);
        assert!(!path.exists());
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
