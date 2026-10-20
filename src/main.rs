mod app;
mod check;
mod cmd;
mod form;
mod git;
mod profile;
mod rules;
mod scan;
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
    /// Write a profile into the repo's .git/config without the UI
    ///
    /// Sets user.name, user.email and, when the profile has them,
    /// user.signingkey and core.sshCommand, exactly as Enter does in the
    /// UI. Without PROFILE, applies the profile whose rules claim the
    /// repo, which makes `gitenv apply` a one-liner for a post-checkout
    /// hook or a clone wrapper.
    Apply {
        /// Profile name; defaults to the one whose rules claim this repo
        profile: Option<String>,
        /// Directory inside the repository
        #[arg(long, short, default_value = ".")]
        path: PathBuf,
    },
    /// Show what git would use here and whether it is the right profile
    ///
    /// Exit codes match `check`. With --short, prints one token for a
    /// shell prompt: the profile name, `personal -> acme` when a rule
    /// says another profile should be in use, or `?` when nothing matches.
    Status {
        /// One token for a shell prompt
        #[arg(long, short)]
        short: bool,
        /// Directory inside the repository
        path: Option<PathBuf>,
    },
    /// List every repository under a directory and whether each is on
    /// the right profile
    ///
    /// Prints a table and a summary. Exits 1 if any repository is on the
    /// wrong profile or matches none. Hidden directories, node_modules and
    /// target are skipped, and a repository's own subdirectories are not
    /// searched for nested repositories.
    Scan {
        /// Directory to search (default: the current directory)
        dir: Option<PathBuf>,
        /// Only list repositories that need attention
        #[arg(long, short)]
        problems: bool,
        /// Apply the expected profile wherever a rule claims a repo that
        /// is on the wrong identity
        #[arg(long)]
        fix: bool,
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
        Some(Command::Apply { profile, path }) => Ok(match resolve_dir(Some(path)) {
            Ok(dir) => cmd::apply(profile.as_deref(), &dir),
            Err(err) => {
                eprintln!("gitenv: {err:#}");
                ExitCode::from(2)
            }
        }),
        Some(Command::Status { short, path }) => Ok(match resolve_dir(path) {
            Ok(dir) => cmd::status(short, &dir),
            Err(err) => {
                eprintln!("gitenv: {err:#}");
                ExitCode::from(2)
            }
        }),
        Some(Command::Scan { dir, problems, fix }) => Ok(match resolve_dir(dir) {
            Ok(dir) => scan::run(
                &dir,
                &scan::Options {
                    problems_only: problems,
                    fix,
                },
            ),
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
