use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

const DEFAULTS: &str = r#"
[[rule]]
tool = "npm"
script = ["check*", "test*", "build*"]
weight = 2
passthrough = ["--help", "--watch"]

[[rule]]
tool = "npm"
script = ["e2e*"]
weight_from_flag = "--workers"
passthrough = ["--help", "--list"]

[[rule]]
tool = "npx"
command = ["vitest"]
weight = 2
weight_if_arg = { "*.test.ts" = 1 }
passthrough = ["--help", "--version", "--watch", "-w"]
"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    tool: String,
    script: Option<Vec<String>>,
    command: Option<Vec<String>>,
    under: Option<String>,
    weight: Option<NonZeroU32>,
    weight_from_flag: Option<String>,
    #[serde(default)]
    weight_if_arg: BTreeMap<String, u32>,
    #[serde(default)]
    passthrough: Vec<String>,
}

/// User rules replace the built-in ones entirely; an empty `rule` list counts as none.
pub fn load(config: &toml::Table) -> Result<Vec<Rule>, String> {
    let user = config
        .get("rule")
        .map(|v| v.as_array().ok_or("rule must be an array of tables"))
        .transpose()?;
    match user.filter(|r| !r.is_empty()) {
        Some(rules) => rules
            .iter()
            .enumerate()
            .map(|(i, v)| -> Result<Rule, String> {
                v.clone()
                    .try_into()
                    .map_err(|e| format!("rule {}: {e}", i + 1))
            })
            .collect(),
        None => {
            let builtin: toml::Table = DEFAULTS
                .parse()
                .map_err(|e| format!("built-in rules: {e}"))?;
            load(&builtin)
        }
    }
}

/// `Some(weight)` means queue at that weight; `None` means exec directly.
pub fn classify(rules: &[Rule], tool: &str, args: &[String], cwd: &Path) -> Option<u32> {
    let rule = rules.iter().find(|r| r.matches(tool, args, cwd))?;
    if args.iter().any(|a| rule.passthrough.contains(a)) {
        return None;
    }
    Some(rule.weight(args))
}

pub fn tools(rules: &[Rule]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for r in rules {
        if !names.contains(&r.tool) {
            names.push(r.tool.clone());
        }
    }
    names
}

impl Rule {
    fn matches(&self, tool: &str, args: &[String], cwd: &Path) -> bool {
        self.tool == tool
            && self.under.as_deref().is_none_or(|u| inside(u, cwd))
            && self
                .script
                .as_ref()
                .is_none_or(|g| script_name(tool, args).is_some_and(|s| any_glob(g, s)))
            && self.command.as_ref().is_none_or(|g| {
                let command = args
                    .iter()
                    .map(String::as_str)
                    .find(|a| !a.starts_with('-'));
                command.is_some_and(|c| any_glob(g, c))
            })
    }

    fn weight(&self, args: &[String]) -> u32 {
        self.weight_from_flag
            .as_deref()
            .and_then(|f| flag_value(args, f))
            .or_else(|| {
                self.weight_if_arg
                    .iter()
                    .find(|(g, _)| args.iter().any(|a| glob(g, a)))
                    .map(|(_, &w)| w)
            })
            .or(self.weight.map(NonZeroU32::get))
            .unwrap_or(1)
    }
}

// shortcut: flags that take a value before the script (npm --prefix dir run x) are misread, parse npm's flag table if it bites.
fn script_name<'a>(tool: &str, args: &'a [String]) -> Option<&'a str> {
    let mut words = args
        .iter()
        .map(String::as_str)
        .skip_while(|a| a.starts_with('-'));
    let first = words.next()?;
    match first {
        "run" | "run-script" => words.find(|a| !a.starts_with('-')),
        "test" | "t" | "tst" => Some("test"),
        _ if matches!(tool, "pnpm" | "yarn" | "bun") => Some(first),
        _ => None,
    }
}

// Accepts `--flag 4` and `--flag=4`; a value that is not a positive integer is ignored.
fn flag_value(args: &[String], flag: &str) -> Option<u32> {
    args.iter().enumerate().find_map(|(i, a)| {
        let v = if a == flag {
            args.get(i + 1)?.as_str()
        } else {
            a.strip_prefix(flag)?.strip_prefix('=')?
        };
        v.parse().ok().filter(|&n| n > 0)
    })
}

fn inside(under: &str, cwd: &Path) -> bool {
    let dir = match under.strip_prefix("~/") {
        Some(rest) => env::var_os("HOME")
            .map_or_else(|| PathBuf::from(under), |h| PathBuf::from(h).join(rest)),
        None => PathBuf::from(under),
    };
    let dir = fs::canonicalize(&dir).unwrap_or(dir);
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    cwd.starts_with(dir)
}

fn any_glob(globs: &[String], s: &str) -> bool {
    globs.iter().any(|g| glob(g, s))
}

// `*` matches any run of bytes, including none; the whole string must match.
fn glob(pat: &str, s: &str) -> bool {
    let (p, t) = (pat.as_bytes(), s.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(config: &str) -> Vec<Rule> {
        load(&config.parse().unwrap()).unwrap()
    }

    fn cls(rules: &[Rule], tool: &str, args: &str) -> Option<u32> {
        let args: Vec<String> = args.split_whitespace().map(String::from).collect();
        classify(rules, tool, &args, Path::new("/"))
    }

    #[test]
    fn npm_defaults() {
        let r = rules("");
        for a in [
            "run test",
            "test",
            "t",
            "tst",
            "run-script build",
            "run -s check:types",
            "-s run test",
        ] {
            assert_eq!(cls(&r, "npm", a), Some(2), "{a}");
        }
        for a in ["install", "run lint", "run build -- --watch", "run", ""] {
            assert_eq!(cls(&r, "npm", a), None, "{a}");
        }
        assert_eq!(cls(&r, "npm", "run e2e -- --workers 4"), Some(4));
        assert_eq!(cls(&r, "npm", "run e2e -- --workers=3"), Some(3));
        assert_eq!(cls(&r, "npm", "run e2e"), Some(1));
        assert_eq!(cls(&r, "npm", "run e2e -- --workers abc"), Some(1));
    }

    #[test]
    fn npx_defaults() {
        let r = rules("");
        assert_eq!(cls(&r, "npx", "vitest"), Some(2));
        assert_eq!(cls(&r, "npx", "--yes vitest"), Some(2));
        assert_eq!(cls(&r, "npx", "vitest run src/a.test.ts"), Some(1));
        assert_eq!(cls(&r, "npx", "vitest --watch"), None);
        assert_eq!(cls(&r, "npx", "tsc"), None);
    }

    #[test]
    fn pnpm_bare_and_run_script() {
        let r = rules("[[rule]]\ntool = \"pnpm\"\nscript = [\"build\"]\n");
        assert_eq!(cls(&r, "pnpm", "build"), Some(1));
        assert_eq!(cls(&r, "pnpm", "run build"), Some(1));
    }

    #[test]
    fn under_matches_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let r = rules(&format!(
            "[[rule]]\ntool = \"npm\"\nunder = {:?}\n",
            dir.path().to_str().unwrap()
        ));
        let args = vec!["install".to_string()];
        assert_eq!(classify(&r, "npm", &args, &sub), Some(1));
        assert_eq!(classify(&r, "npm", &args, Path::new("/")), None);
    }

    #[test]
    fn under_expands_home() {
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        let r = rules("[[rule]]\ntool = \"npm\"\nunder = \"~/\"\n");
        let args = vec!["install".to_string()];
        assert_eq!(classify(&r, "npm", &args, &home), Some(1));
        assert_eq!(classify(&r, "npm", &args, Path::new("/")), None);
    }

    #[test]
    fn user_rules_replace_defaults() {
        let r = rules("[[rule]]\ntool = \"cargo\"\nweight = 3\n");
        assert_eq!(cls(&r, "npm", "run test"), None);
        assert_eq!(cls(&r, "cargo", "build"), Some(3));
    }

    #[test]
    fn zero_weight_is_an_error() {
        assert!(load(&"[[rule]]\ntool = \"npm\"\nweight = 0\n".parse().unwrap()).is_err());
    }

    #[test]
    fn unknown_field_names_rule() {
        let config = "[[rule]]\ntool = \"npm\"\n\n[[rule]]\ntool = \"npm\"\nscirpt = [\"x\"]\n";
        let err = load(&config.parse().unwrap()).err().unwrap();
        assert!(
            err.starts_with("rule 2:") && err.contains("scirpt"),
            "{err}"
        );
    }

    #[test]
    fn glob_matching() {
        assert!(glob("*", ""));
        assert!(glob("*", "abc"));
        assert!(glob("a*", "a"));
        assert!(glob("a*", "abc"));
        assert!(glob("*b", "ab"));
        assert!(glob("a*b", "axxb"));
        assert!(glob("ab", "ab"));
        assert!(glob("", ""));
        assert!(!glob("ab", "abc"));
        assert!(!glob("a*", "b"));
        assert!(!glob("a*b", "abc"));
    }

    #[test]
    fn tools_are_unique_in_rule_order() {
        assert_eq!(tools(&rules("")), ["npm", "npx"]);
    }
}
