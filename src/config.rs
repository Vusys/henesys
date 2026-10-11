use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Slot budget: `HENESYS_SLOTS`, then the config file's `slots`, then cores - 2.
pub fn slots(file: &toml::Table) -> Result<u32, String> {
    let cores = std::thread::available_parallelism().ok().map(|n| n.get());
    resolve_slots(
        env_value("HENESYS_SLOTS").as_deref(),
        file.get("slots"),
        cores,
    )
}

// Non-UTF-8 values become a debug string, which never parses, so they fail as bad values.
fn env_value(name: &str) -> Option<String> {
    env::var_os(name).map(|v| v.into_string().unwrap_or_else(|v| format!("{v:?}")))
}

pub fn state_dir() -> PathBuf {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    resolve_state_dir(
        env::var_os("HENESYS_DIR"),
        env::var_os("XDG_RUNTIME_DIR"),
        uid,
    )
}

pub fn config_path() -> Option<PathBuf> {
    resolve_config_path(
        env::var_os("HENESYS_CONFIG"),
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

pub fn load_config() -> Result<toml::Table, String> {
    match config_path() {
        Some(path) => read_config(&path),
        None => Ok(toml::Table::new()),
    }
}

pub fn max_wait() -> Result<Option<Duration>, String> {
    parse_max_wait(env_value("HENESYS_MAX_WAIT").as_deref())
}

pub fn env_flag(name: &str) -> bool {
    env::var_os(name).is_some_and(|v| !matches!(v.as_encoded_bytes(), b"" | b"0"))
}

fn resolve_slots(
    env: Option<&str>,
    file: Option<&toml::Value>,
    cores: Option<usize>,
) -> Result<u32, String> {
    if let Some(s) = env.filter(|s| !s.is_empty()) {
        return s
            .parse::<u32>()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("HENESYS_SLOTS must be a positive integer, got '{s}'"));
    }
    if let Some(v) = file {
        return v
            .as_integer()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("slots in config file must be a positive integer, got {v}"));
    }
    let default = cores.map_or(1, |c| c.saturating_sub(2).max(1));
    Ok(u32::try_from(default).unwrap_or(u32::MAX))
}

fn resolve_state_dir(
    henesys_dir: Option<OsString>,
    xdg_runtime: Option<OsString>,
    uid: u32,
) -> PathBuf {
    if let Some(d) = non_empty(henesys_dir) {
        return d.into();
    }
    if let Some(d) = non_empty(xdg_runtime) {
        return PathBuf::from(d).join("henesys");
    }
    PathBuf::from(format!("/tmp/henesys-{uid}"))
}

fn resolve_config_path(
    henesys_config: Option<OsString>,
    xdg_config: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(p) = non_empty(henesys_config) {
        return Some(p.into());
    }
    if let Some(d) = non_empty(xdg_config) {
        return Some(PathBuf::from(d).join("henesys").join("config.toml"));
    }
    non_empty(home).map(|h| {
        PathBuf::from(h)
            .join(".config")
            .join("henesys")
            .join("config.toml")
    })
}

fn non_empty(v: Option<OsString>) -> Option<OsString> {
    v.filter(|s| !s.is_empty())
}

fn read_config(path: &Path) -> Result<toml::Table, String> {
    match fs::read_to_string(path) {
        Ok(text) => text.parse().map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn parse_max_wait(v: Option<&str>) -> Result<Option<Duration>, String> {
    match v.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => s
            .parse::<u64>()
            .map(|n| Some(Duration::from_secs(n)))
            .map_err(|_| format!("HENESYS_MAX_WAIT must be a non-negative integer, got '{s}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(s.into())
    }

    #[test]
    fn slots_precedence() {
        let file = toml::Value::Integer(5);
        assert_eq!(resolve_slots(Some("2"), Some(&file), Some(8)), Ok(2));
        assert_eq!(resolve_slots(Some("2"), None, Some(8)), Ok(2));
        assert_eq!(resolve_slots(None, Some(&file), Some(8)), Ok(5));
        assert_eq!(resolve_slots(None, None, Some(8)), Ok(6));
        assert_eq!(resolve_slots(Some(""), Some(&file), Some(8)), Ok(5));
    }

    #[test]
    fn slots_default_from_cores() {
        assert_eq!(resolve_slots(None, None, Some(1)), Ok(1));
        assert_eq!(resolve_slots(None, None, Some(2)), Ok(1));
        assert_eq!(resolve_slots(None, None, Some(3)), Ok(1));
        assert_eq!(resolve_slots(None, None, Some(8)), Ok(6));
        assert_eq!(resolve_slots(None, None, None), Ok(1));
    }

    #[test]
    fn slots_invalid_values_are_errors() {
        let err = resolve_slots(Some("abc"), None, None).unwrap_err();
        assert_eq!(err, "HENESYS_SLOTS must be a positive integer, got 'abc'");
        assert!(resolve_slots(Some("0"), None, None).is_err());
        assert!(resolve_slots(None, Some(&toml::Value::Integer(0)), None).is_err());
        assert!(resolve_slots(None, Some(&toml::Value::String("4".into())), None).is_err());
    }

    #[test]
    fn state_dir_precedence() {
        assert_eq!(
            resolve_state_dir(os("/a"), os("/run/u"), 1000),
            PathBuf::from("/a")
        );
        assert_eq!(
            resolve_state_dir(None, os("/run/u"), 1000),
            PathBuf::from("/run/u/henesys")
        );
        assert_eq!(
            resolve_state_dir(None, None, 1000),
            PathBuf::from("/tmp/henesys-1000")
        );
        assert_eq!(
            resolve_state_dir(os(""), os(""), 1000),
            PathBuf::from("/tmp/henesys-1000")
        );
    }

    #[test]
    fn config_path_precedence() {
        assert_eq!(
            resolve_config_path(os("/c.toml"), os("/xdg"), os("/home/u")),
            Some(PathBuf::from("/c.toml"))
        );
        assert_eq!(
            resolve_config_path(None, os("/xdg"), os("/home/u")),
            Some(PathBuf::from("/xdg/henesys/config.toml"))
        );
        assert_eq!(
            resolve_config_path(None, os(""), os("/home/u")),
            Some(PathBuf::from("/home/u/.config/henesys/config.toml"))
        );
        assert_eq!(resolve_config_path(None, None, None), None);
    }

    #[test]
    fn load_config_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_config(&dir.path().join("missing.toml")),
            Ok(toml::Table::new())
        );
    }

    #[test]
    fn load_config_malformed_file_names_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "slots = ").unwrap();
        let err = read_config(&path).unwrap_err();
        assert!(err.starts_with(&format!("{}: ", path.display())), "{err}");
    }

    #[test]
    fn max_wait_parsing() {
        assert_eq!(parse_max_wait(None), Ok(None));
        assert_eq!(parse_max_wait(Some("")), Ok(None));
        assert_eq!(
            parse_max_wait(Some("90")),
            Ok(Some(Duration::from_secs(90)))
        );
        assert!(parse_max_wait(Some("-1")).is_err());
        assert!(parse_max_wait(Some("1.5")).is_err());
    }
}
