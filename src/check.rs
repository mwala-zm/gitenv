//! The `check` subcommand: is this repo's git identity one of the saved
//! profiles? Prints nothing and exits 0 when it is, so a pre-commit or
//! pre-push hook stays quiet on the happy path. Exits 1 on a mismatch and
//! 2 when it could not even look (not a repo, unreadable config, no git).
//!
//! Rails analogy: a rake task. It reuses the models and never touches the
//! views in `ui.rs`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::git::{self, Identity, Repo};
use crate::profile::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The effective identity matches the profile with this name.
    Match(String),
    /// Nothing matches. Carries what git would use so the message can show it.
    Mismatch {
        root: PathBuf,
        identity: Identity,
        no_profiles: bool,
    },
}

impl Verdict {
    pub fn is_ok(&self) -> bool {
        matches!(self, Verdict::Match(_))
    }
}

/// Decide whether `repo` is on a saved profile.
pub fn verdict(config: &Config, repo: &Repo) -> Verdict {
    match config.matching(&repo.effective) {
        Some(profile) => Verdict::Match(profile.name.clone()),
        None => Verdict::Mismatch {
            root: repo.root.clone(),
            identity: repo.effective.clone(),
            no_profiles: config.profiles.is_empty(),
        },
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Match(name) => write!(f, "gitenv: identity matches profile '{name}'"),
            Verdict::Mismatch {
                root,
                identity,
                no_profiles,
            } => {
                if *no_profiles {
                    return write!(
                        f,
                        "gitenv: no profiles saved yet; run `gitenv` to create one"
                    );
                }
                writeln!(
                    f,
                    "gitenv: git identity in {} does not match any saved profile",
                    root.display()
                )?;
                let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(not set)".into());
                writeln!(f, "  name         {}", show(&identity.name))?;
                writeln!(f, "  email        {}", show(&identity.email))?;
                if identity.signing_key.is_some() {
                    writeln!(f, "  signing key  {}", show(&identity.signing_key))?;
                }
                if identity.ssh_command.is_some() {
                    writeln!(f, "  ssh          {}", show(&identity.ssh_command))?;
                }
                write!(f, "  run `gitenv` in that repo to pick a profile")
            }
        }
    }
}

/// Entry point for `gitenv check [PATH]`.
pub fn run(dir: &Path) -> ExitCode {
    match try_run(dir) {
        Ok(verdict) if verdict.is_ok() => ExitCode::SUCCESS,
        Ok(verdict) => {
            eprintln!("{verdict}");
            ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn try_run(dir: &Path) -> Result<Verdict> {
    let config = Config::load(&Config::path()?)?;
    let repo = git::discover(dir)?
        .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
    Ok(verdict(&config, &repo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;

    fn identity(email: &str) -> Identity {
        Identity {
            name: Some("Ada".into()),
            email: Some(email.into()),
            signing_key: None,
            ssh_command: None,
        }
    }

    fn repo(identity: Identity) -> Repo {
        Repo {
            root: "/work/acme/api".into(),
            remote: None,
            local: identity.clone(),
            effective: identity,
        }
    }

    fn config() -> Config {
        Config {
            profiles: vec![Profile {
                name: "acme".into(),
                user_name: "Ada".into(),
                email: "ada@acme.example".into(),
                signing_key: None,
                ssh_key: None,
            }],
        }
    }

    #[test]
    fn matching_identity_passes() {
        let v = verdict(&config(), &repo(identity("ada@acme.example")));
        assert_eq!(v, Verdict::Match("acme".into()));
        assert!(v.is_ok());
    }

    #[test]
    fn unknown_identity_fails_and_shows_what_git_would_use() {
        let v = verdict(&config(), &repo(identity("ada@example.com")));
        assert!(!v.is_ok());
        let text = v.to_string();
        assert!(text.contains("/work/acme/api"));
        assert!(text.contains("ada@example.com"));
        assert!(text.contains("run `gitenv`"));
    }

    #[test]
    fn unset_identity_is_shown_as_not_set() {
        let v = verdict(&config(), &repo(Identity::default()));
        assert!(v.to_string().contains("(not set)"));
    }

    #[test]
    fn no_profiles_is_a_mismatch_with_a_hint() {
        let v = verdict(&Config::default(), &repo(identity("ada@acme.example")));
        assert!(!v.is_ok());
        assert!(v.to_string().contains("no profiles saved"));
    }
}
