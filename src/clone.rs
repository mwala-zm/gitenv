use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::cmd;
use crate::git;
use crate::profile::{Config, Profile};
use crate::rules;

pub struct Options {
    pub url: String,
    pub dir: Option<PathBuf>,
    pub profile: Option<String>,
    pub git_args: Vec<String>,
}

pub fn run(opts: &Options) -> ExitCode {
    match try_run(opts) {
        Ok(Outcome::Applied(msg)) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Unclaimed(msg)) | Ok(Outcome::Overridden(msg)) => {
            eprintln!("{msg}");
            ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            ExitCode::from(2)
        }
    }
}

enum Outcome {
    Applied(String),
    Overridden(String),
    Unclaimed(String),
}

fn try_run(opts: &Options) -> Result<Outcome> {
    let config = Config::load(&Config::path()?)?;
    let profile = match &opts.profile {
        Some(name) => Some(cmd::named(&config, name)?),
        None => rules::for_remote(&config, &opts.url).map(|(p, _)| p),
    };
    let target = opts
        .dir
        .clone()
        .or_else(|| target_dir(&opts.url))
        .context("cannot tell where git would put this clone; name the directory")?;

    let mut git = Command::new("git");
    if let Some(profile) = profile {
        for (key, value) in clone_config(profile) {
            git.arg("-c").arg(format!("{key}={value}"));
        }
    }
    git.arg("clone")
        .args(&opts.git_args)
        .arg(&opts.url)
        .arg(&target);
    let status = git
        .status()
        .context("running git; is it installed and on PATH?")?;
    if !status.success() {
        bail!("git clone failed");
    }

    let Some(profile) = profile else {
        return Ok(Outcome::Unclaimed(format!(
            "gitenv: cloned {} with git's own identity; no profile rule claims {}.\n\
             Pick one: gitenv apply <PROFILE> --path {}",
            target.display(),
            rules::hide_password(&opts.url),
            target.display()
        )));
    };
    let repo = git::discover(&target)?.with_context(|| {
        format!(
            "cloned {}, but it is not a working repository gitenv can write to (a bare clone?)",
            target.display()
        )
    })?;
    git::apply(&repo, profile)?;
    let applied = format!(
        "gitenv: cloned {} as profile '{}'{}",
        target.display(),
        profile.name,
        cmd::gh_note(profile)
    );
    Ok(match git::still_overridden(&repo.root, profile) {
        Some(why) => Outcome::Overridden(format!("{applied}, but {why}")),
        None => Outcome::Applied(applied),
    })
}

/// The git settings the clone itself must run with: the profile's ssh key, so
/// the first fetch already offers the right key, and its `[profiles.git]` keys,
/// since an `insteadOf` there may rewrite the URL. Name and email wait for
/// `apply`; a clone makes no commits.
fn clone_config(profile: &Profile) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = profile
        .git
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(key) = &profile.ssh_key {
        out.push(("core.sshCommand".into(), git::ssh_command(key)));
    }
    out
}

/// The directory `git clone URL` creates when none is named: the last path
/// segment, without `.git` or a trailing slash, as git's `guess_dir_name` does
/// for the common forms. A bare `host:` or `file:///` with nothing after gives
/// `None`, and git would refuse it too.
pub fn target_dir(url: &str) -> Option<PathBuf> {
    let url = url.trim().trim_end_matches('/');
    let path = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let last = path.rsplit(['/', ':']).next()?;
    let last = last.strip_suffix(".git").unwrap_or(last);
    let last = last.strip_suffix(".bundle").unwrap_or(last);
    let name = last.trim_end_matches('/');
    if name.is_empty() || name == "." || name == ".." || name.contains('@') {
        return None;
    }
    Some(PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_dir_matches_what_git_would_pick() {
        let dir = |u: &str| target_dir(u).map(|p| p.display().to_string());
        assert_eq!(dir("git@github.com:acme/api.git"), Some("api".into()));
        assert_eq!(dir("https://github.com/acme/api"), Some("api".into()));
        assert_eq!(dir("https://github.com/acme/api.git/"), Some("api".into()));
        assert_eq!(dir("ssh://git@host:22/x/y/web.git"), Some("web".into()));
        assert_eq!(dir("/srv/git/thing.git"), Some("thing".into()));
        assert_eq!(dir("../thing"), Some("thing".into()));
        assert_eq!(dir("git@github.com:"), None);
    }

    #[test]
    fn clone_config_carries_the_key_and_extra_settings() {
        let profile = Profile {
            name: "acme".into(),
            user_name: "Ada".into(),
            email: "ada@acme.example".into(),
            ssh_key: Some("/home/ada/.ssh/id_acme".into()),
            git: [("pull.rebase".to_string(), "true".to_string())].into(),
            ..Default::default()
        };
        let config = clone_config(&profile);
        assert_eq!(config[0], ("pull.rebase".into(), "true".into()));
        assert_eq!(config[1].0, "core.sshCommand");
        assert!(config[1].1.contains("id_acme"), "{}", config[1].1);
        assert!(clone_config(&Profile::default()).is_empty());
    }
}
