//! Talks to the `git` binary.
//!
//! Rails analogy: the database adapter. Nothing in here knows about the UI.
//! We shell out to `git` rather than link a library so behaviour matches
//! exactly what the developer sees on the command line.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::profile::{Profile, SigningFormat};

/// The identity git would use for a commit right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: Option<String>,
    pub email: Option<String>,
    pub signing_key: Option<String>,
    /// Value of `core.sshCommand`, which decides which ssh key is offered.
    pub ssh_command: Option<String>,
    /// Value of `gpg.format`.
    pub signing_format: Option<SigningFormat>,
    /// Value of `commit.gpgsign`, if git could parse it as a boolean.
    pub sign_commits: Option<bool>,
}

/// Parse a git boolean: `true`, `yes`, `on`, `1` and their opposites.
pub fn git_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

impl Identity {
    /// True when this identity is what `profile` would produce. Optional
    /// parts of the profile only have to match when the profile sets them.
    pub fn matches(&self, profile: &Profile) -> bool {
        let name_ok = self.name.as_deref() == Some(profile.user_name.as_str());
        let email_ok = self.email.as_deref() == Some(profile.email.as_str());
        let key_ok = match &profile.signing_key {
            Some(key) => self.signing_key.as_deref() == Some(key.as_str()),
            None => true,
        };
        let ssh_ok = match &profile.ssh_key {
            Some(key) => self.ssh_command.as_deref() == Some(ssh_command(key).as_str()),
            None => true,
        };
        // Git defaults gpg.format to openpgp, so an unset value satisfies
        // a profile that asks for openpgp. An unset commit.gpgsign is off.
        let format_ok = match profile.signing_format {
            Some(want) => self.signing_format.unwrap_or(SigningFormat::Openpgp) == want,
            None => true,
        };
        let sign_ok = match profile.sign_commits {
            Some(want) => self.sign_commits.unwrap_or(false) == want,
            None => true,
        };
        name_ok && email_ok && key_ok && ssh_ok && format_ok && sign_ok
    }
}

/// How many recent commits the author audit looks at.
pub const HISTORY_DEPTH: usize = 20;

/// One author seen in recent history and how many of those commits are theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
    pub commits: usize,
}

/// A git repository the tool is looking at.
#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub remote: Option<String>,
    /// What git will actually use: local config layered over global.
    pub effective: Identity,
    /// Only what is set in `.git/config`, ignoring global config.
    pub local: Identity,
    /// Authors of the last [`HISTORY_DEPTH`] commits on the current branch,
    /// most frequent first. Empty for a repo with no commits yet.
    pub authors: Vec<Author>,
}

impl Repo {
    /// Commits actually inspected: at most [`HISTORY_DEPTH`].
    pub fn commits_inspected(&self) -> usize {
        self.authors.iter().map(|a| a.commits).sum()
    }

    /// Recent authors whose email is not the one git would use now. These
    /// are the commits that were probably made on the wrong identity.
    pub fn foreign_authors(&self) -> Vec<&Author> {
        self.authors
            .iter()
            .filter(|a| Some(a.email.as_str()) != self.effective.email.as_deref())
            .collect()
    }
}

/// Find the repository containing `dir`. Returns `None` when `dir` is not
/// inside a git working tree.
pub fn discover(dir: &Path) -> Result<Option<Repo>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running git; is it installed and on PATH?")?;
    if !output.status.success() {
        return Ok(None);
    }
    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    Ok(Some(Repo {
        remote: config_get(&root, &["--get", "remote.origin.url"]),
        effective: read_identity(&root, &[]),
        local: read_identity(&root, &["--local"]),
        authors: recent_authors(&root, HISTORY_DEPTH),
        root,
    }))
}

/// Authors of the last `depth` commits on HEAD, most frequent first. A repo
/// without commits, or any git failure, gives an empty list.
pub fn recent_authors(root: &Path, depth: usize) -> Vec<Author> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-n", &depth.to_string(), "--format=%ae%x09%an"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut authors: Vec<Author> = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((email, name)) = line.split_once('\t') else {
            continue;
        };
        match authors.iter_mut().find(|a| a.email == email) {
            Some(a) => a.commits += 1,
            None => authors.push(Author {
                name: name.to_string(),
                email: email.to_string(),
                commits: 1,
            }),
        }
    }
    authors.sort_by_key(|a| std::cmp::Reverse(a.commits));
    authors
}

/// The identity from `~/.gitconfig` alone. Used to seed a first profile.
pub fn global_identity() -> Identity {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    read_identity(&cwd, &["--global"])
}

/// Write `profile` into the repo's local config so it wins over global.
pub fn apply(repo: &Repo, profile: &Profile) -> Result<()> {
    config_set(&repo.root, "user.name", &profile.user_name)?;
    config_set(&repo.root, "user.email", &profile.email)?;
    match &profile.signing_key {
        Some(key) => config_set(&repo.root, "user.signingkey", key)?,
        None => config_unset(&repo.root, "user.signingkey")?,
    }
    match &profile.ssh_key {
        Some(key) => config_set(&repo.root, "core.sshCommand", &ssh_command(key))?,
        None => config_unset(&repo.root, "core.sshCommand")?,
    }
    match profile.signing_format {
        Some(format) => config_set(&repo.root, "gpg.format", format.as_git())?,
        None => config_unset(&repo.root, "gpg.format")?,
    }
    match profile.sign_commits {
        Some(sign) => config_set(
            &repo.root,
            "commit.gpgsign",
            if sign { "true" } else { "false" },
        )?,
        None => config_unset(&repo.root, "commit.gpgsign")?,
    }
    Ok(())
}

/// The `core.sshCommand` value that makes git offer `key` and nothing else.
/// The path is single-quoted so spaces survive, and a leading `~` is
/// expanded here because quoting stops the shell from doing it.
pub fn ssh_command(key: &Path) -> String {
    let path = expand_tilde(key).display().to_string();
    format!("ssh -i {} -o IdentitiesOnly=yes", shell_quote(&path))
}

/// `~` and `~/x` become the home directory and a path under it.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match directories::UserDirs::new() {
        Some(dirs) => dirs.home_dir().join(rest),
        None => path.to_path_buf(),
    }
}

/// Quote for POSIX `sh`, which is what git runs `core.sshCommand` through.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn read_identity(root: &Path, scope: &[&str]) -> Identity {
    let get = |key: &str| {
        let mut args = scope.to_vec();
        args.extend(["--get", key]);
        config_get(root, &args)
    };
    Identity {
        name: get("user.name"),
        email: get("user.email"),
        signing_key: get("user.signingkey"),
        ssh_command: get("core.sshCommand"),
        signing_format: get("gpg.format").and_then(|s| SigningFormat::parse(&s)),
        sign_commits: get("commit.gpgsign").and_then(|s| git_bool(&s)),
    }
}

fn config_get(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("config")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn config_set(root: &Path, key: &str, value: &str) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--local", key, value])
        .status()
        .context("running git config")?;
    if !status.success() {
        bail!("git config --local {key} failed");
    }
    Ok(())
}

fn config_unset(root: &Path, key: &str) -> Result<()> {
    // Exit code 5 means "key was not set", which is fine for us.
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--local", "--unset", key])
        .status()
        .context("running git config --unset")?;
    match status.code() {
        Some(0) | Some(5) => Ok(()),
        _ => bail!("git config --local --unset {key} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(key: Option<&str>) -> Profile {
        Profile {
            name: "p".into(),
            user_name: "Ada".into(),
            email: "ada@example.com".into(),
            signing_key: key.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn matches_on_name_and_email() {
        let id = Identity {
            name: Some("Ada".into()),
            email: Some("ada@example.com".into()),
            ..Default::default()
        };
        assert!(id.matches(&profile(None)));
        assert!(!id.matches(&profile(Some("KEY"))));
    }

    #[test]
    fn ssh_key_must_match_when_profile_sets_one() {
        let mut p = profile(None);
        p.ssh_key = Some("/tmp/id_a".into());
        let mut id = Identity {
            name: Some("Ada".into()),
            email: Some("ada@example.com".into()),
            ssh_command: Some(ssh_command(Path::new("/tmp/id_b"))),
            ..Default::default()
        };
        assert!(!id.matches(&p));
        id.ssh_command = Some(ssh_command(Path::new("/tmp/id_a")));
        assert!(id.matches(&p));
        id.ssh_command = None;
        assert!(!id.matches(&p));
    }

    #[test]
    fn ssh_command_quotes_the_path_and_expands_home() {
        assert_eq!(
            ssh_command(Path::new("/tmp/my key")),
            "ssh -i '/tmp/my key' -o IdentitiesOnly=yes"
        );
        assert_eq!(
            ssh_command(Path::new("/tmp/it's")),
            r"ssh -i '/tmp/it'\''s' -o IdentitiesOnly=yes"
        );
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        assert_eq!(
            ssh_command(Path::new("~/.ssh/id_acme")),
            format!(
                "ssh -i '{}' -o IdentitiesOnly=yes",
                home.join(".ssh/id_acme").display()
            )
        );
    }

    #[test]
    fn apply_writes_local_config_and_discover_reads_it_back() {
        let dir = std::env::temp_dir().join(format!("gitenv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .arg("init")
                .arg("-q")
                .status()
                .unwrap()
                .success()
        );

        let repo = discover(&dir).unwrap().expect("temp dir should be a repo");
        let mut p = profile(Some("KEY123"));
        p.ssh_key = Some("/tmp/id_test".into());
        p.signing_format = Some(SigningFormat::Ssh);
        p.sign_commits = Some(true);
        apply(&repo, &p).unwrap();

        let repo = discover(&dir).unwrap().unwrap();
        assert_eq!(repo.local.name.as_deref(), Some("Ada"));
        assert_eq!(repo.local.email.as_deref(), Some("ada@example.com"));
        assert_eq!(repo.local.signing_key.as_deref(), Some("KEY123"));
        assert_eq!(repo.local.signing_format, Some(SigningFormat::Ssh));
        assert_eq!(repo.local.sign_commits, Some(true));
        assert!(repo.effective.matches(&p));
        assert_eq!(
            config_get(&dir, &["--local", "--get", "core.sshCommand"]).as_deref(),
            Some("ssh -i '/tmp/id_test' -o IdentitiesOnly=yes")
        );

        // Applying a profile without a key or ssh path clears both.
        apply(&repo, &profile(None)).unwrap();
        let repo = discover(&dir).unwrap().unwrap();
        assert!(repo.local.signing_key.is_none());
        assert!(config_get(&dir, &["--local", "--get", "core.sshCommand"]).is_none());
        assert!(repo.local.signing_format.is_none());
        assert!(repo.local.sign_commits.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recent_authors_counts_who_made_the_last_commits() {
        let dir = std::env::temp_dir().join(format!("gitenv-authors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .env("GIT_COMMITTER_NAME", "C")
                    .env("GIT_COMMITTER_EMAIL", "c@example.com")
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        let repo = discover(&dir).unwrap().unwrap();
        assert!(repo.authors.is_empty(), "no commits yet");
        assert_eq!(repo.commits_inspected(), 0);

        git(&[
            "-c",
            "user.name=Ada",
            "-c",
            "user.email=ada@acme.example",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ]);
        git(&[
            "-c",
            "user.name=Ada",
            "-c",
            "user.email=ada@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "two",
        ]);
        git(&[
            "-c",
            "user.name=Ada",
            "-c",
            "user.email=ada@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "three",
        ]);
        git(&["config", "--local", "user.email", "ada@acme.example"]);
        git(&["config", "--local", "user.name", "Ada"]);

        let repo = discover(&dir).unwrap().unwrap();
        assert_eq!(repo.commits_inspected(), 3);
        assert_eq!(repo.authors[0].email, "ada@example.com");
        assert_eq!(repo.authors[0].commits, 2);
        let foreign = repo.foreign_authors();
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].email, "ada@example.com");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn signing_format_and_gpgsign_count_towards_the_match() {
        let mut p = profile(Some("~/.ssh/id.pub"));
        p.signing_format = Some(SigningFormat::Ssh);
        p.sign_commits = Some(true);
        let mut id = Identity {
            name: Some("Ada".into()),
            email: Some("ada@example.com".into()),
            signing_key: Some("~/.ssh/id.pub".into()),
            ..Default::default()
        };
        assert!(
            !id.matches(&p),
            "unset format is openpgp, unset gpgsign is off"
        );
        id.signing_format = Some(SigningFormat::Ssh);
        assert!(!id.matches(&p), "still not signing");
        id.sign_commits = Some(true);
        assert!(id.matches(&p));

        // A profile asking for openpgp is satisfied by git's default.
        p.signing_format = Some(SigningFormat::Openpgp);
        id.signing_format = None;
        assert!(id.matches(&p));
    }

    #[test]
    fn git_booleans() {
        assert_eq!(git_bool("yes"), Some(true));
        assert_eq!(git_bool("TRUE"), Some(true));
        assert_eq!(git_bool("0"), Some(false));
        assert_eq!(git_bool("maybe"), None);
    }

    #[test]
    fn wrong_email_does_not_match() {
        let id = Identity {
            name: Some("Ada".into()),
            email: Some("ada@other.example".into()),
            ..Default::default()
        };
        assert!(!id.matches(&profile(None)));
    }
}
