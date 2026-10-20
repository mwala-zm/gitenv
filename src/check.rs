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

use anyhow::Result;

use crate::git::{Identity, Repo};
use crate::profile::Config;
use crate::rules::{self, Reason};

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
    /// A rule says this repo belongs to `expected`, but the identity is
    /// something else: another profile (`actual`) or nothing saved at all.
    WrongProfile {
        root: PathBuf,
        expected: String,
        reason: Reason,
        actual: Option<String>,
        identity: Identity,
    },
}

impl Verdict {
    pub fn is_ok(&self) -> bool {
        matches!(self, Verdict::Match(_))
    }
}

/// Decide whether `repo` is on a saved profile.
pub fn verdict(config: &Config, repo: &Repo) -> Verdict {
    let actual = config.matching(&repo.effective);
    if let Some((expected, reason)) = rules::expected(config, repo)
        && actual != Some(expected)
    {
        return Verdict::WrongProfile {
            root: repo.root.clone(),
            expected: expected.name.clone(),
            reason,
            actual: actual.map(|p| p.name.clone()),
            identity: repo.effective.clone(),
        };
    }
    match actual {
        Some(profile) => Verdict::Match(profile.name.clone()),
        None => Verdict::Mismatch {
            root: repo.root.clone(),
            identity: repo.effective.clone(),
            no_profiles: config.profiles.is_empty(),
        },
    }
}

fn write_identity(f: &mut fmt::Formatter<'_>, identity: &Identity) -> fmt::Result {
    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(not set)".into());
    writeln!(f, "  name         {}", show(&identity.name))?;
    writeln!(f, "  email        {}", show(&identity.email))?;
    if identity.signing_key.is_some() {
        writeln!(f, "  signing key  {}", show(&identity.signing_key))?;
    }
    if identity.ssh_command.is_some() {
        writeln!(f, "  ssh          {}", show(&identity.ssh_command))?;
    }
    Ok(())
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
                write_identity(f, identity)?;
                write!(f, "  run `gitenv` in that repo to pick a profile")
            }
            Verdict::WrongProfile {
                root,
                expected,
                reason,
                actual,
                identity,
            } => {
                writeln!(
                    f,
                    "gitenv: {} should use profile '{expected}' ({reason})",
                    root.display()
                )?;
                match actual {
                    Some(name) => writeln!(f, "  but its identity is profile '{name}':")?,
                    None => writeln!(f, "  but its identity matches no saved profile:")?,
                }
                write_identity(f, identity)?;
                write!(
                    f,
                    "  run `gitenv` in that repo and press Enter on '{expected}'"
                )
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
    let (config, repo) = crate::cmd::load(dir)?;
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
            authors: Vec::new(),
        }
    }

    fn config() -> Config {
        Config {
            profiles: vec![Profile {
                name: "acme".into(),
                user_name: "Ada".into(),
                email: "ada@acme.example".into(),
                ..Default::default()
            }],
        }
    }

    fn config_with_rules() -> Config {
        let mut config = config();
        config.profiles[0].remotes = vec!["github.com/acme/*".into()];
        config.profiles.push(Profile {
            name: "personal".into(),
            user_name: "Ada".into(),
            email: "ada@example.com".into(),
            ..Default::default()
        });
        config
    }

    #[test]
    fn rule_overrides_a_match_on_another_profile() {
        let mut r = repo(identity("ada@example.com"));
        r.remote = Some("git@github.com:acme/api.git".into());
        let v = verdict(&config_with_rules(), &r);
        assert!(!v.is_ok());
        let text = v.to_string();
        assert!(text.contains("should use profile 'acme'"), "{text}");
        assert!(text.contains("remote matches github.com/acme/*"), "{text}");
        assert!(text.contains("is profile 'personal'"), "{text}");
        assert!(text.contains("press Enter on 'acme'"), "{text}");
    }

    #[test]
    fn rule_plus_unknown_identity_names_the_expected_profile() {
        let mut r = repo(identity("someone@else.example"));
        r.remote = Some("git@github.com:acme/api.git".into());
        let text = verdict(&config_with_rules(), &r).to_string();
        assert!(text.contains("matches no saved profile"), "{text}");
        assert!(text.contains("someone@else.example"), "{text}");
    }

    #[test]
    fn rule_satisfied_is_a_match() {
        let mut r = repo(identity("ada@acme.example"));
        r.remote = Some("git@github.com:acme/api.git".into());
        assert_eq!(
            verdict(&config_with_rules(), &r),
            Verdict::Match("acme".into())
        );
    }

    #[test]
    fn no_rule_falls_back_to_any_profile() {
        let mut r = repo(identity("ada@example.com"));
        r.remote = Some("git@gitlab.com:someone/thing.git".into());
        assert_eq!(
            verdict(&config_with_rules(), &r),
            Verdict::Match("personal".into())
        );
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
