//! `reclass` binary entry point. Default front-end is egui; `--tui` selects the
//! ratatui terminal UI. Both drive the same `app_state::AppState`.

#[cfg(feature = "gui")]
use reclass::gui;
#[cfg(feature = "tui")]
use reclass::tui;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage: reclass [--tui] [--pid <N>] [--addr <expr>] [--project <file.ron>]");
        return Ok(());
    }
    let use_tui = args.iter().any(|a| a == "--tui");
    let pid = parse_pid(&args);
    let addr = parse_opt(&args, "--addr");
    let project = parse_opt(&args, "--project");
    run_frontend(use_tui, pid, addr, project)
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

#[cfg(all(feature = "gui", feature = "tui"))]
fn run_frontend(
    use_tui: bool,
    pid: Option<i32>,
    addr: Option<String>,
    project: Option<String>,
) -> anyhow::Result<()> {
    if use_tui {
        tui::run(pid, addr, project)
    } else {
        gui::run(pid, addr, project)
    }
}

#[cfg(all(feature = "gui", not(feature = "tui")))]
fn run_frontend(
    _use_tui: bool,
    pid: Option<i32>,
    addr: Option<String>,
    project: Option<String>,
) -> anyhow::Result<()> {
    gui::run(pid, addr, project)
}

#[cfg(all(not(feature = "gui"), feature = "tui"))]
fn run_frontend(
    _use_tui: bool,
    pid: Option<i32>,
    addr: Option<String>,
    project: Option<String>,
) -> anyhow::Result<()> {
    tui::run(pid, addr, project)
}

#[cfg(all(not(feature = "gui"), not(feature = "tui")))]
fn run_frontend(
    _use_tui: bool,
    _pid: Option<i32>,
    _addr: Option<String>,
    _project: Option<String>,
) -> anyhow::Result<()> {
    anyhow::bail!("reclass was built without a frontend (enable `gui` and/or `tui`)")
}
