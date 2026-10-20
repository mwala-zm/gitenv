mod app;
mod check;
mod form;
mod git;
mod profile;
mod rules;
mod ui;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::App;

/// Keep the right git identity for the repo you're in.
///
/// With no subcommand, opens a terminal UI for the repository containing
/// PATH (default: the current directory). Profiles live in a TOML file;
/// run once to create it.
#[derive(Debug, Parser)]
#[command(
    name = "gitenv",
    version,
    about,
    long_about,
    args_conflicts_with_subcommands = true,
    after_help = "Environment:\n  GITENV_CONFIG  Path of the profiles file (default: a per-user config dir)"
)]
struct Cli {
    /// Directory inside the repository to look at
    path: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check that the repo is on the right profile; quiet on success
    ///
    /// Exits 0 when the repo's identity matches the profile it should be
    /// on, 1 when it does not, 2 when it could not look (not a repo,
    /// config unreadable, git missing). Prints only on failure, so it is
    /// quiet inside a pre-commit or pre-push hook.
    Check {
        /// Directory inside the repository to check
        path: Option<PathBuf>,
    },
    /// Print a shell completion script to stdout.
    ///
    /// For example: `gitenv completions zsh > ~/.zfunc/_gitenv`.
    Completions {
        /// Shell to generate the script for
        shell: Shell,
    },
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Check { path }) => Ok(match resolve_dir(path) {
            Ok(dir) => check::run(&dir),
            Err(err) => {
                eprintln!("gitenv: {err:#}");
                ExitCode::from(2)
            }
        }),
        Some(Command::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "gitenv", &mut io::stdout());
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let dir = resolve_dir(cli.path)?;
            let mut app = App::new(&dir)?;
            let mut terminal = ratatui::init();
            let result = run(&mut terminal, &mut app);
            ratatui::restore();
            result.map(|()| ExitCode::SUCCESS)
        }
    }
}

/// The directory to look at: the argument if given, else the current one.
fn resolve_dir(arg: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match arg {
        Some(path) => path,
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
