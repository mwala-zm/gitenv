mod app;
mod check;
mod clone;
mod cmd;
mod doctor;
mod export;
mod fix;
mod form;
mod gh;
mod git;
mod hook;
mod profile;
mod report;
mod rules;
mod scan;
mod ui;

use std::io;
use std::path::{Path, PathBuf};
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
    /// Sets user.name and user.email, and the signing key, gpg.format,
    /// commit.gpgsign, core.sshCommand and [profiles.git] keys the profile
    /// has, exactly as Enter does in the UI. Without PROFILE, applies the
    /// profile whose rules claim the repo, which makes `gitenv apply` a
    /// one-liner for a post-checkout hook or a clone wrapper.
    Apply {
        /// Profile name; defaults to the one whose rules claim this repo
        profile: Option<String>,
        /// Directory inside the repository
        #[arg(long, short, default_value = ".")]
        path: PathBuf,
    },
    /// Clone a repository with the profile its remote rules name
    ///
    /// Matches URL against every profile's remote rules, runs `git clone`
    /// with that profile's ssh key and [profiles.git] settings, so the
    /// first fetch already uses the right key, then writes the profile
    /// into the new clone and switches the GitHub CLI to its account.
    /// Arguments for git clone go after `--`. When no rule claims the URL
    /// and --profile is not given, the clone still happens with git's own
    /// identity, and the command says so and exits 1.
    Clone {
        /// Repository to clone, in any form git accepts
        url: String,
        /// Directory to clone into (default: what git would pick)
        dir: Option<PathBuf>,
        /// Profile to use instead of the one the rules pick
        #[arg(long, short)]
        profile: Option<String>,
        /// Extra arguments for git clone, after `--`
        #[arg(last = true)]
        git_args: Vec<String>,
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
    /// List the commits you made, grouped by the profile each repo belongs to
    ///
    /// Walks every repository under DIR as `scan` does, and lists your
    /// commits there over all branches: those whose author email is any
    /// profile's. Repositories are grouped by the profile a rule claims
    /// them for, else the one their identity matches. A commit made with
    /// a different email than its repository's profile is marked, so a
    /// slip stands out. --since and --until take anything git does, such
    /// as "2 weeks ago" or 2026-10-01. --csv prints one row per commit.
    Report {
        /// Directory to search (default: the current directory)
        dir: Option<PathBuf>,
        /// Start of the period
        #[arg(long, default_value = "30 days ago")]
        since: String,
        /// End of the period (default: now)
        #[arg(long)]
        until: Option<String>,
        /// One row per commit: date, profile, repo, commit, author email, subject
        #[arg(long)]
        csv: bool,
    },
    /// Check that each profile would work: keys exist and log in
    ///
    /// Looks for missing ssh and signing key files, a GPG key missing from
    /// the keyring, signing switched on without a key, profiles with the
    /// same name or identity, and bad rule patterns. Unless --offline, also
    /// runs `ssh -T` with each profile's ssh key against every host its
    /// remote rules name, and shows which account the host says it is.
    /// Exits 1 if anything failed.
    Doctor {
        /// Skip logging in to hosts over ssh
        #[arg(long)]
        offline: bool,
    },
    /// Re-author unpushed commits made with the wrong identity
    ///
    /// Finds commits on HEAD that no remote branch, tag or other local
    /// branch has, and whose author or committer is not the profile this
    /// repo should use (the one a rule claims it for, else the one it is
    /// on). Rebuilds them with that profile, keeping author dates and
    /// messages, without touching the work tree or index, then applies the
    /// profile. Refuses during a rebase or merge, or when it cannot tell
    /// what is pushed. Asks first unless --yes.
    FixAuthors {
        /// Rewrite without asking
        #[arg(long, short)]
        yes: bool,
        /// Directory inside the repository
        #[arg(long, short, default_value = ".")]
        path: PathBuf,
    },
    /// Install or remove `gitenv check` as pre-commit and pre-push hooks
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Write profiles and rules as git config, so git picks the identity
    /// by itself
    ///
    /// Each profile with rules gets a file with its identity, and
    /// includes.gitconfig loads them with `includeIf`: path rules as
    /// `gitdir/i:`, remote rules as `hasconfig:remote.*.url:` (git 2.36+).
    /// The right identity then applies in a fresh clone before its first
    /// commit, without gitenv running. Once exported, the TUI rewrites the
    /// files whenever you edit profiles.
    Export {
        /// Also add the file to include.path in your global git config
        #[arg(long)]
        install: bool,
    },
    /// Print a shell completion script to stdout.
    ///
    /// For example: `gitenv completions zsh > ~/.zfunc/_gitenv`.
    Completions {
        /// Shell to generate the script for
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
enum HookAction {
    /// Write the hooks; never replaces a hook gitenv did not write
    ///
    /// With --global, the hooks go in the global core.hooksPath
    /// (~/.githooks unless one is set) and cover every repository; each
    /// repository's own hook still runs after the check.
    Install {
        /// For every repository, through core.hooksPath
        #[arg(long)]
        global: bool,
        /// Directory inside the repository
        #[arg(long, short, default_value = ".")]
        path: PathBuf,
    },
    /// Remove the hooks gitenv wrote
    Remove {
        /// The global hooks, unsetting core.hooksPath if gitenv set it
        #[arg(long)]
        global: bool,
        /// Directory inside the repository
        #[arg(long, short, default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Check { path }) => Ok(in_dir(path, check::run)),
        Some(Command::Apply { profile, path }) => Ok(in_dir(Some(path), |dir| {
            cmd::apply(profile.as_deref(), dir)
        })),
        Some(Command::Clone {
            url,
            dir,
            profile,
            git_args,
        }) => Ok(clone::run(&clone::Options {
            url,
            dir,
            profile,
            git_args,
        })),
        Some(Command::Status { short, path }) => Ok(in_dir(path, |dir| cmd::status(short, dir))),
        Some(Command::Scan { dir, problems, fix }) => Ok(in_dir(dir, |dir| {
            scan::run(
                dir,
                &scan::Options {
                    problems_only: problems,
                    fix,
                },
            )
        })),
        Some(Command::Report {
            dir,
            since,
            until,
            csv,
        }) => Ok(in_dir(dir, |dir| {
            report::run(dir, &report::Options { since, until, csv })
        })),
        Some(Command::Doctor { offline }) => Ok(doctor::run(offline)),
        Some(Command::FixAuthors { yes, path }) => Ok(in_dir(Some(path), |dir| fix::run(dir, yes))),
        Some(Command::Hook { action }) => Ok(match action {
            HookAction::Install { global, path } => {
                in_dir(Some(path), |dir| hook::run(hook::install(dir, global)))
            }
            HookAction::Remove { global, path } => {
                in_dir(Some(path), |dir| hook::run(hook::remove(dir, global)))
            }
        }),
        Some(Command::Export { install }) => Ok(export::run(install)),
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

fn in_dir(arg: Option<PathBuf>, run: impl FnOnce(&Path) -> ExitCode) -> ExitCode {
    match resolve_dir(arg) {
        Ok(dir) => run(&dir),
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            ExitCode::from(2)
        }
    }
}

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
