//! Shells out to `git` rather than linking a library, so behaviour matches
//! exactly what the developer sees on the command line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::profile::{Profile, SigningFormat};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: Option<String>,
    pub email: Option<String>,
    pub signing_key: Option<String>,
    pub ssh_command: Option<String>,
    pub signing_format: Option<SigningFormat>,
    pub sign_commits: Option<bool>,
    /// Every config value in scope, keyed as `git config --list` prints them,
    /// so a profile's extra `git` keys can be checked without knowing them.
    pub settings: BTreeMap<String, String>,
}

/// Write a config key the way `git config --list` prints it: section and
/// variable name lowercased, any subsection in between left alone.
pub fn normalize_key(key: &str) -> String {
    let Some(first) = key.find('.') else {
        return key.to_ascii_lowercase();
    };
    let last = key.rfind('.').unwrap_or(first);
    if first == last {
        return key.to_ascii_lowercase();
    }
    format!(
        "{}{}{}",
        key[..first].to_ascii_lowercase(),
        &key[first..=last],
        key[last + 1..].to_ascii_lowercase()
    )
}

pub fn git_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

impl Identity {
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
        let extra_ok = profile
            .git
            .iter()
            .all(|(k, v)| self.settings.get(&normalize_key(k)) == Some(v));
        name_ok && email_ok && key_ok && ssh_ok && format_ok && sign_ok && extra_ok
    }
}

pub const HISTORY_DEPTH: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
    pub commits: usize,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub remote: Option<String>,
    pub effective: Identity,
    pub local: Identity,
    pub authors: Vec<Author>,
}

impl Repo {
    /// The remote URLs rules are matched against: `origin` when there is
    /// one, else every remote, so a clone whose only remote is `upstream`
    /// is still claimed.
    pub fn rule_remotes(&self) -> Vec<&str> {
        let all = remote_urls(&self.effective.settings);
        if all.is_empty() || self.effective.settings.contains_key("remote.origin.url") {
            return self.remote.as_deref().into_iter().collect();
        }
        all
    }

    pub fn commits_inspected(&self) -> usize {
        self.authors.iter().map(|a| a.commits).sum()
    }

    /// `reference` is the email the repo should be using: the expected
    /// profile's when a rule claims the repo, else the effective one.
    pub fn foreign_authors(&self, reference: Option<&str>) -> Vec<&Author> {
        self.authors
            .iter()
            .filter(|a| Some(a.email.as_str()) != reference)
            .collect()
    }
}

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
    let effective = read_identity(&root, &[]);
    Ok(Some(Repo {
        remote: effective
            .settings
            .get("remote.origin.url")
            .map(String::as_str)
            .or_else(|| remote_urls(&effective.settings).first().copied())
            .map(Into::into),
        effective,
        local: read_identity(&root, &["--local"]),
        authors: recent_authors(&root, HISTORY_DEPTH),
        root,
    }))
}

/// `remote.<name>.url` values, in remote-name order.
fn remote_urls(settings: &BTreeMap<String, String>) -> Vec<&str> {
    settings
        .iter()
        .filter(|(k, _)| k.starts_with("remote.") && k.ends_with(".url"))
        .map(|(_, v)| v.as_str())
        .collect()
}

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

pub fn global_identity() -> Identity {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    read_identity(&cwd, &["--global"])
}

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
    for (key, value) in &profile.git {
        config_set(&repo.root, key, value)?;
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

/// A key set several times keeps its last value, as `git config --get` would.
fn read_identity(root: &Path, scope: &[&str]) -> Identity {
    let settings = config_list(root, scope);
    let get = |key: &str| settings.get(key).filter(|v| !v.is_empty()).cloned();
    Identity {
        name: get("user.name"),
        email: get("user.email"),
        signing_key: get("user.signingkey"),
        ssh_command: get("core.sshcommand"),
        signing_format: get("gpg.format").and_then(|s| SigningFormat::parse(&s)),
        sign_commits: get("commit.gpgsign").and_then(|s| git_bool(&s)),
        settings,
    }
}

fn config_list(root: &Path, scope: &[&str]) -> BTreeMap<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("config")
        .args(scope)
        .args(["--list", "-z"])
        .output();
    let mut map = BTreeMap::new();
    let Ok(output) = output else {
        return map;
    };
    if !output.status.success() {
        return map;
    }
    // `-z` gives `key\nvalue\0` per entry; a key without `=` has no newline.
    for entry in String::from_utf8_lossy(&output.stdout).split('\0') {
        if entry.is_empty() {
            continue;
        }
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        map.insert(key.to_string(), value.to_string());
    }
    map
}

#[cfg(test)]
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
        p.git.insert(
            "url.git@github.com:acme/.insteadOf".into(),
            "https://github.com/acme/".into(),
        );
        apply(&repo, &p).unwrap();

        let repo = discover(&dir).unwrap().unwrap();
        assert_eq!(repo.local.name.as_deref(), Some("Ada"));
        assert_eq!(repo.local.email.as_deref(), Some("ada@example.com"));
        assert_eq!(repo.local.signing_key.as_deref(), Some("KEY123"));
        assert_eq!(repo.local.signing_format, Some(SigningFormat::Ssh));
        assert_eq!(repo.local.sign_commits, Some(true));
        assert_eq!(
            repo.local
                .settings
                .get("url.git@github.com:acme/.insteadof")
                .map(String::as_str),
            Some("https://github.com/acme/")
        );
        assert!(repo.effective.matches(&p));
        p.git
            .insert("url.git@github.com:acme/.insteadOf".into(), "other".into());
        assert!(!repo.effective.matches(&p), "extra key with another value");
        assert_eq!(
            config_get(&dir, &["--local", "--get", "core.sshCommand"]).as_deref(),
            Some("ssh -i '/tmp/id_test' -o IdentitiesOnly=yes")
        );

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
        let foreign = repo.foreign_authors(repo.effective.email.as_deref());
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].email, "ada@example.com");
        assert_eq!(repo.foreign_authors(None).len(), 2);

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
    fn keys_are_normalised_like_git_prints_them() {
        assert_eq!(normalize_key("User.Name"), "user.name");
        assert_eq!(
            normalize_key("URL.git@GitHub.com:Acme/.insteadOf"),
            "url.git@GitHub.com:Acme/.insteadof"
        );
        assert_eq!(normalize_key("core"), "core");
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

    #[test]
    fn rules_see_every_remote_when_there_is_no_origin() {
        let mut repo = Repo {
            root: "/r".into(),
            remote: Some("git@github.com:ada/api.git".into()),
            effective: Identity::default(),
            local: Identity::default(),
            authors: Vec::new(),
        };
        assert_eq!(repo.rule_remotes(), vec!["git@github.com:ada/api.git"]);

        let settings = &mut repo.effective.settings;
        settings.insert(
            "remote.origin.url".into(),
            "git@github.com:ada/api.git".into(),
        );
        settings.insert(
            "remote.upstream.url".into(),
            "git@github.com:acme/api.git".into(),
        );
        assert_eq!(repo.rule_remotes(), vec!["git@github.com:ada/api.git"]);

        repo.effective.settings.remove("remote.origin.url");
        repo.effective.settings.insert(
            "remote.my.fork.url".into(),
            "git@github.com:ada/fork.git".into(),
        );
        assert_eq!(
            repo.rule_remotes(),
            vec!["git@github.com:ada/fork.git", "git@github.com:acme/api.git"]
        );
    }
}
