use lexopt::prelude::*;

const USAGE: &str = "\
usage: henesys <command>

commands:
  run [-w N] [-l TEXT] [-p] [--max-wait SECS] [--] <cmd> [args...]
  status [--json]
  shim install <dir> [tool...]
";

// 125 is henesys's own error code; see the frozen exit codes in #1.
fn main() {
    let mut parser = lexopt::Parser::from_env();
    let code = match parser.next() {
        Ok(Some(Long("version"))) => {
            println!("henesys {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Ok(Some(Long("help") | Short('h'))) => {
            print!("{USAGE}");
            0
        }
        Ok(None) => {
            eprint!("{USAGE}");
            125
        }
        Ok(Some(arg)) => {
            let name = match arg {
                Value(v) => v.to_string_lossy().into_owned(),
                Long(l) => format!("--{l}"),
                Short(c) => format!("-{c}"),
            };
            eprintln!("henesys: unknown command '{name}'");
            eprint!("{USAGE}");
            125
        }
        Err(e) => {
            eprintln!("henesys: {e}");
            125
        }
    };
    std::process::exit(code);
}
