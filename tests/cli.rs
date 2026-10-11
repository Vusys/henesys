use std::process::{Command, Output};

fn henesys(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_henesys"))
        .args(args)
        .output()
        .expect("run henesys")
}

#[test]
fn version_prints_crate_version() {
    let out = henesys(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("henesys {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_exits_zero_with_usage_on_stdout() {
    let out = henesys(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("usage: henesys"));
}

#[test]
fn no_arguments_exits_125_with_usage_on_stderr() {
    let out = henesys(&[]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: henesys"));
}

#[test]
fn unknown_command_exits_125() {
    let out = henesys(&["bogus"]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("henesys: unknown command 'bogus'\n"));
}
