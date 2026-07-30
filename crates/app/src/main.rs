//! `reclass` binary entry point: parse a handful of flags and hand off to the
//! egui front-end, which owns `app_state::AppState`.

#[cfg(feature = "gui")]
use reclass::gui;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: reclass [--pid <N>] [--addr <expr>] [--project <file.ron>]");
        return Ok(());
    }
    let pid = parse_pid(&args);
    let addr = parse_opt(&args, "--addr");
    let project = parse_opt(&args, "--project");
    run_frontend(pid, addr, project)
}

fn parse_pid(args: &[String]) -> Option<i32> {
    parse_opt(args, "--pid").and_then(|s| s.parse().ok())
}

fn parse_opt(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter();
    let eqp = format!("{flag}=");
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&eqp) {
            return Some(rest.to_string());
        }
    }
    None
}

#[cfg(feature = "gui")]
fn run_frontend(
    pid: Option<i32>,
    addr: Option<String>,
    project: Option<String>,
) -> anyhow::Result<()> {
    gui::run(pid, addr, project)
}

#[cfg(not(feature = "gui"))]
fn run_frontend(
    _pid: Option<i32>,
    _addr: Option<String>,
    _project: Option<String>,
) -> anyhow::Result<()> {
    anyhow::bail!("reclass was built without a frontend (enable the `gui` feature)")
}
