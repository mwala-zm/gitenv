mod app;
mod form;
mod git;
mod profile;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::App;

const USAGE: &str = "\
gitenv: keep the right git identity for the repo you're in

Usage: gitenv [PATH]

Opens a terminal UI for the repository containing PATH (default: the
current directory). Profiles live in a TOML file; run once to create it.

Options:
  -h, --help       Show this help
  -V, --version    Show the version";

fn main() -> Result<()> {
    let dir = match std::env::args().nth(1).as_deref() {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("gitenv {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir().context("reading current directory")?,
    };
    if !dir.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }

    let mut app = App::new(&dir)?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
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
