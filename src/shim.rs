use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use lexopt::prelude::*;

use crate::config;
use crate::rules;
use crate::run::{self, RunOpts};

const USAGE: &str = "usage: henesys shim install <dir> [tool...]\n";

// Shim mode: `tool` is the basename of argv[0]. Returns the exit code.
pub fn run(tool: &OsStr, args: Vec<OsString>) -> i32 {
    let me = match env::current_exe().and_then(fs::canonicalize) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("henesys: {e}");
            return 125;
        }
    };
    let Some(real) = find_real(tool, &me) else {
        eprintln!(
            "henesys: no real '{}' found on PATH after the henesys shims",
            tool.to_string_lossy()
        );
        return 127;
    };
    if config::env_flag("HENESYS_DISABLE") || config::env_flag("HENESYS_HELD") {
        return exec(&real, tool, &args);
    }
    // slots and max-wait are read again inside run; checking them here keeps a bad value from failing the command.
    let checked = config::load_config().and_then(|file| {
        config::slots(&file)?;
        config::max_wait()?;
        rules::load(&file)
    });
    let rules = match checked {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "henesys: {e}; running {} without the queue",
                tool.to_string_lossy()
            );
            return exec(&real, tool, &args);
        }
    };

    let tool_str = tool.to_string_lossy();
    let strs: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cwd = env::current_dir().unwrap_or_default();
    match rules::classify(&rules, &tool_str, &strs, &cwd) {
        None => exec(&real, tool, &args),
        Some(weight) => {
            let dir = cwd.file_name().unwrap_or(cwd.as_os_str()).to_string_lossy();
            run::run(RunOpts {
                weight,
                label: Some(format!("{dir}: {tool_str} {}", strs.join(" "))),
                priority: false,
                max_wait: None,
                program: real.into_os_string(),
                args,
                arg0: Some(tool.to_owned()),
            })
        }
    }
}

// First executable `tool` on PATH that is not a henesys shim.
fn find_real(tool: &OsStr, me: &Path) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|dir| dir.join(tool)).find(|c| {
        fs::metadata(c).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            && fs::canonicalize(c).ok().as_deref() != Some(me)
    })
}

fn exec(real: &Path, tool: &OsStr, args: &[OsString]) -> i32 {
    let err = Command::new(real).arg0(tool).args(args).exec();
    run::spawn_failed(real.as_os_str(), &err)
}

// `shim install <dir> [tool...]`; `parser` is positioned after `shim`.
pub fn install_cmd(parser: &mut lexopt::Parser) -> i32 {
    match parser.next() {
        Ok(Some(Value(v))) if v == "install" => {}
        _ => {
            eprint!("{USAGE}");
            return 125;
        }
    }
    let words: Vec<OsString> = match parser.raw_args() {
        Ok(w) => w.collect(),
        Err(e) => {
            eprintln!("henesys: {e}");
            return 125;
        }
    };
    let Some((dir, given)) = words.split_first() else {
        eprint!("{USAGE}");
        return 125;
    };
    let tools: Vec<String> = if given.is_empty() {
        match config::load_config().and_then(|file| rules::load(&file)) {
            Ok(r) => rules::tools(&r),
            Err(e) => {
                eprintln!("henesys: {e}");
                return 125;
            }
        }
    } else {
        given
            .iter()
            .map(|t| t.to_string_lossy().into_owned())
            .collect()
    };
    install(Path::new(dir), &tools)
}

fn install(dir: &Path, tools: &[String]) -> i32 {
    let exe = match env::current_exe().and_then(fs::canonicalize) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("henesys: {e}");
            return 125;
        }
    };
    if let Err(e) = fs::create_dir_all(dir) {
        eprintln!("henesys: {}: {e}", dir.display());
        return 125;
    }
    let mut code = 0;
    for tool in tools {
        let path = dir.join(tool);
        if fs::symlink_metadata(&path).is_err() {
            match symlink(&exe, &path) {
                Ok(()) => println!("installed {}", path.display()),
                Err(e) => {
                    eprintln!("henesys: {}: {e}", path.display());
                    code = 125;
                }
            }
        } else if fs::canonicalize(&path).ok().as_deref() == Some(exe.as_path()) {
            println!("ok {}", path.display());
        } else {
            eprintln!(
                "henesys: {} exists and is not a henesys shim; remove it first",
                path.display()
            );
            code = 125;
        }
    }
    println!(
        "Put {0} first on PATH, e.g. export PATH=\"{0}:$PATH\"",
        dir.display()
    );
    code
}
