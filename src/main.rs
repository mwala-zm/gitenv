mod app;
mod check;
mod form;
mod git;
mod profile;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::App;

const USAGE: &str = "\
gitenv: keep the right git identity for the repo you're in

Usage: gitenv [PATH]
       gitenv check [PATH]

With no subcommand, opens a terminal UI for the repository containing
PATH (default: the current directory). Profiles live in a TOML file; run
once to create it.

check   Exit 0 when the repo's identity matches a saved profile, 1 when it
        does not, 2 when it could not look. Prints only on failure, so it
        is quiet inside a pre-commit or pre-push hook.

Options:
  -h, --help       Show this help
  -V, --version    Show the version

Environment:
  GITENV_CONFIG    Path of the profiles file (default: a per-user config dir)";

fn main() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some("-V" | "--version") => {
            println!("gitenv {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Some("check") => return Ok(check::run(&resolve_dir(args.get(1).map(String::as_str))?)),
        other => resolve_dir(other)?,
    };

    let mut app = App::new(&dir)?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

/// The directory to look at: the argument if given, else the current one.
fn resolve_dir(arg: Option<&str>) -> Result<PathBuf> {
    let dir = match arg {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir().context("reading current directory")?,
    };
    if !dir.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }
    Ok(dir)
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, app))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key);
        }
    }
    Ok(())
}
