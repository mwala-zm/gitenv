//! Non-interactive subcommands: `apply` and `status`.
//!
//! Rails analogy: more rake tasks beside `check`. They reuse the models in
//! `git`, `profile` and `rules` and print plain text, so they work from
//! scripts, clone hooks and shell prompts.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::check::{self, Verdict};
use crate::git::{self, Repo};
use crate::profile::Config;
use crate::rules;

/// Load the profiles file and find the repository containing `dir`.
pub fn load(dir: &Path) -> Result<(Config, Repo)> {
    let config = Config::load(&Config::path()?)?;
    let repo = git::discover(dir)?
        .with_context(|| format!("{} is not inside a git repository", dir.display()))?;
    Ok((config, repo))
}

/// `gitenv apply [PROFILE] [PATH]`: write a profile into the repo's local
/// config. Without a name, the profile whose rules claim the repo is used.
pub fn apply(name: Option<&str>, dir: &Path) -> ExitCode {
    match try_apply(name, dir) {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn try_apply(name: Option<&str>, dir: &Path) -> Result<String> {
    let (config, repo) = load(dir)?;
    let profile = match name {
        Some(name) => config
            .profiles
            .iter()
            .find(|p| p.name == name)
            .with_context(|| {
                let known: Vec<&str> = config.profiles.iter().map(|p| p.name.as_str()).collect();
                if known.is_empty() {
                    "no profile is named '{name}'; no profiles saved yet".replace("{name}", name)
                } else {
                    format!("no profile is named '{name}'; known: {}", known.join(", "))
                }
            })?,
        None => match rules::expected(&config, &repo) {
            Some((profile, _)) => profile,
            None => bail!(
                "no profile rule claims {}; name the profile: gitenv apply <PROFILE>",
                repo.root.display()
            ),
        },
    };
    git::apply(&repo, profile)?;
    Ok(format!(
        "gitenv: applied profile '{}' to {}",
        profile.name,
        repo.root.display()
    ))
}

/// `gitenv status [--short] [PATH]`: what git would use here and whether
/// it is right. Always prints; exit codes are the same as `check` so a
/// prompt can colour the output.
pub fn status(short: bool, dir: &Path) -> ExitCode {
    let (config, repo) = match load(dir) {
        Ok(loaded) => loaded,
        Err(err) => {
            if !short {
                eprintln!("gitenv: {err:#}");
            }
            return ExitCode::from(2);
        }
    };
    let verdict = check::verdict(&config, &repo);
    let text = if short {
        short_status(&verdict)
    } else {
        long_status(&config, &repo, &verdict)
    };
    println!("{text}");
    if verdict.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// One token for a prompt: the profile in use, with `-> name` when a rule
/// says it should be another, and `?` when nothing matches.
pub fn short_status(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Match(name) => name.clone(),
        Verdict::Mismatch { .. } => "?".into(),
        Verdict::WrongProfile {
            expected, actual, ..
        } => format!("{} -> {expected}", actual.as_deref().unwrap_or("?")),
    }
}

pub fn long_status(config: &Config, repo: &Repo, verdict: &Verdict) -> String {
    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(not set)".into());
    let mut out = Vec::new();
    out.push(format!("repo         {}", repo.root.display()));
    out.push(format!("remote       {}", show(&repo.remote)));
    out.push(format!("name         {}", show(&repo.effective.name)));
    out.push(format!("email        {}", show(&repo.effective.email)));
    if repo.effective.signing_key.is_some() {
        out.push(format!(
            "signing key  {}",
            show(&repo.effective.signing_key)
        ));
    }
    if let Some(format) = repo.effective.signing_format {
        out.push(format!("signing      {}", format.as_git()));
    }
    if let Some(sign) = repo.effective.sign_commits {
        out.push(format!("sign commits {}", if sign { "yes" } else { "no" }));
    }
    if repo.effective.ssh_command.is_some() {
        out.push(format!(
            "ssh          {}",
            show(&repo.effective.ssh_command)
        ));
    }
    let source = if repo.local.name.is_none() && repo.local.email.is_none() {
        "global config"
    } else {
        "this repo's .git/config"
    };
    out.push(format!("source       {source}"));
    let foreign = repo.foreign_authors();
    if repo.commits_inspected() > 0 {
        let line = if foreign.is_empty() {
            format!(
                "history      last {} commits all by the current email",
                repo.commits_inspected()
            )
        } else {
            let who: Vec<String> = foreign
                .iter()
                .take(3)
                .map(|a| format!("{} ({})", a.email, a.commits))
                .collect();
            format!(
                "history      {} of the last {} commits by another email: {}",
                foreign.iter().map(|a| a.commits).sum::<usize>(),
                repo.commits_inspected(),
                who.join(", ")
            )
        };
        out.push(line);
    }
    out.push(format!(
        "profile      {}",
        config
            .matching(&repo.effective)
            .map(|p| p.name.as_str())
            .unwrap_or("(none)")
    ));
    match rules::expected(config, repo) {
        Some((p, why)) => out.push(format!(
            "rule         '{}' claims this repo ({why})",
            p.name
        )),
        None => out.push("rule         (none)".into()),
    }
    out.push(match verdict {
        Verdict::Match(name) => format!("status       ok, on profile '{name}'"),
        Verdict::Mismatch {
            no_profiles: true, ..
        } => "status       no profiles saved yet".into(),
        Verdict::Mismatch { .. } => "status       matches no saved profile".into(),
        Verdict::WrongProfile { expected, .. } => {
            format!("status       should use profile '{expected}'")
        }
    });
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Identity;
    use crate::profile::Profile;
    use std::path::PathBuf;

    fn setup() -> (Config, Repo) {
        let config = Config {
            profiles: vec![
                Profile {
                    name: "personal".into(),
                    user_name: "Ada".into(),
                    email: "ada@example.com".into(),
                    ..Default::default()
                },
                Profile {
                    name: "acme".into(),
                    user_name: "Ada".into(),
                    email: "ada@acme.example".into(),
                    remotes: vec!["github.com/acme/*".into()],
                    ..Default::default()
                },
            ],
        };
        let repo = Repo {
            root: PathBuf::from("/work/acme/api"),
            remote: Some("git@github.com:acme/api.git".into()),
            effective: Identity {
                name: Some("Ada".into()),
                email: Some("ada@example.com".into()),
                ..Default::default()
            },
            local: Identity::default(),
            authors: Vec::new(),
        };
        (config, repo)
    }

    #[test]
    fn short_status_shows_the_fix() {
        let (config, mut repo) = setup();
        assert_eq!(
            short_status(&check::verdict(&config, &repo)),
            "personal -> acme"
        );
        repo.effective.email = Some("ada@acme.example".into());
        assert_eq!(short_status(&check::verdict(&config, &repo)), "acme");
        repo.effective.email = Some("who@else.example".into());
        assert_eq!(short_status(&check::verdict(&config, &repo)), "? -> acme");
        repo.remote = None;
        assert_eq!(short_status(&check::verdict(&config, &repo)), "?");
    }

    #[test]
    fn long_status_lists_everything() {
        let (config, mut repo) = setup();
        repo.authors = vec![
            git::Author {
                name: "Ada".into(),
                email: "ada@example.com".into(),
                commits: 7,
            },
            git::Author {
                name: "Ada".into(),
                email: "ada@acme.example".into(),
                commits: 3,
            },
        ];
        let text = long_status(&config, &repo, &check::verdict(&config, &repo));
        assert!(
            text.contains(
                "history      3 of the last 10 commits by another email: ada@acme.example (3)"
            ),
            "{text}"
        );
        assert!(text.contains("repo         /work/acme/api"));
        assert!(text.contains("source       global config"));
        assert!(text.contains("profile      personal"));
        assert!(text.contains("rule         'acme' claims this repo"));
        assert!(text.contains("status       should use profile 'acme'"));
    }
}
