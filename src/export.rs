//! The `export` subcommand: write profiles and their rules as git's own
//! conditional includes, so git picks the identity by itself, even in a
//! fresh clone, without gitenv running.
//!
//! Each profile with rules gets a file holding its identity, and
//! `includes.gitconfig` pulls those in with `includeIf`: path rules become
//! `gitdir/i:` conditions and remote rules `hasconfig:remote.*.url:`
//! conditions. When several match, git keeps the last value it reads, so
//! the blocks are written least specific first and the rule gitenv would
//! pick is the one that sticks.

use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::git;
use crate::profile::{Config, Profile};
use crate::rules::{self, Reason, Rule};

pub const INCLUDES: &str = "includes.gitconfig";
const HEADER: &str = "# Written by `gitenv export`; changes here are overwritten.";

/// Beside the profiles file, in a directory of its own so cleaning up old
/// files never touches anything else.
pub fn dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("gitenv-includes")
}

/// Double-quote a value or subsection the way git config reads it back.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `url.git@host:acme/.insteadOf` -> (`[url "git@host:acme/"]`, `insteadOf`).
fn split_key(key: &str) -> (String, &str) {
    let first = key.find('.').unwrap_or(key.len());
    let last = key.rfind('.').unwrap_or(key.len());
    if first == last {
        return (
            format!("[{}]", &key[..first]),
            key.get(first + 1..).unwrap_or(""),
        );
    }
    let header = format!("[{} {}]", &key[..first], quote(&key[first + 1..last]));
    (header, &key[last + 1..])
}

/// Render `key = value` pairs as git config, one section per run of keys
/// that share a header.
fn render(entries: &[(String, String)]) -> String {
    let mut out = format!("{HEADER}\n");
    let mut current = String::new();
    for (key, value) in entries {
        let (header, var) = split_key(key);
        if header != current {
            out.push_str(&header);
            out.push('\n');
            current = header;
        }
        out.push_str(&format!("\t{var} = {}\n", quote(value)));
    }
    out
}

pub fn profile_file(profile: &Profile) -> String {
    let mut entries = vec![
        ("user.name".to_string(), profile.user_name.clone()),
        ("user.email".to_string(), profile.email.clone()),
    ];
    if let Some(key) = &profile.signing_key {
        entries.push(("user.signingkey".into(), key.clone()));
    }
    if let Some(format) = profile.signing_format {
        entries.push(("gpg.format".into(), format.as_git().into()));
    }
    if let Some(sign) = profile.sign_commits {
        entries.push(("commit.gpgsign".into(), sign.to_string()));
    }
    if let Some(key) = &profile.ssh_key {
        entries.push(("core.sshCommand".into(), git::ssh_command(key)));
    }
    entries.extend(profile.git.iter().map(|(k, v)| (k.clone(), v.clone())));
    render(&entries)
}

/// The `includeIf` conditions that hold wherever `rule` claims a repo.
pub fn conditions(rule: &Rule) -> Vec<String> {
    match &rule.reason {
        Reason::Path(_) => {
            // git matches against the `.git` directory, not the work tree.
            let gitdir = |pattern: &str| {
                let pattern = pattern.trim_end_matches('/');
                if pattern.ends_with("/**") {
                    format!("gitdir/i:{pattern}")
                } else {
                    format!("gitdir/i:{pattern}/.git")
                }
            };
            std::iter::once(&rule.pattern)
                .chain(&rule.resolved)
                .map(|p| gitdir(p))
                .collect()
        }
        Reason::Remote(pattern) => {
            // git compares the URL as written, so spell out each form.
            let host_path = rules::remote_host_path(pattern);
            let (host, path) = host_path.split_once('/').unwrap_or((&host_path, "*"));
            let mut paths = vec![path.to_string()];
            if !path.ends_with('*') {
                paths.push(format!("{path}.git"));
            }
            let mut out = Vec::new();
            for path in &paths {
                for url in [
                    format!("*@{host}:{path}"),
                    format!("https://{host}/{path}"),
                    format!("https://*@{host}/{path}"),
                    format!("ssh://*@{host}/{path}"),
                    format!("ssh://*@{host}:*/{path}"),
                ] {
                    out.push(format!("hasconfig:remote.*.url:{url}"));
                }
            }
            out
        }
    }
}

/// A file name for each profile, unique even when names differ only in
/// characters a file name should not carry.
fn file_names<'a>(profiles: impl Iterator<Item = &'a Profile>) -> Vec<(&'a Profile, String)> {
    let mut taken: Vec<String> = Vec::new();
    profiles
        .map(|p| {
            let safe: String = p
                .name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let mut name = format!("{safe}.gitconfig");
            let mut n = 2;
            while taken.contains(&name) {
                name = format!("{safe}-{n}.gitconfig");
                n += 1;
            }
            taken.push(name.clone());
            (p, name)
        })
        .collect()
}

pub struct Written {
    pub includes: PathBuf,
    pub profiles: usize,
    pub rules: usize,
}

/// Write every file into `out` and remove ones an earlier export left for
/// profiles that are gone or no longer have rules.
pub fn write(config: &Config, out: &Path) -> Result<Written> {
    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let with_rules = config
        .profiles
        .iter()
        .filter(|p| !p.remotes.is_empty() || !p.paths.is_empty());
    let files = file_names(with_rules);

    let mut keep = vec![INCLUDES.to_string()];
    for (profile, name) in &files {
        let path = out.join(name);
        fs::write(&path, profile_file(profile))
            .with_context(|| format!("writing {}", path.display()))?;
        keep.push(name.clone());
    }

    let mut ranked: Vec<(usize, Rule)> = rules::all(config).into_iter().enumerate().collect();
    // Least specific first, and among equals the earliest in the file last,
    // so the value git keeps is the rule gitenv would pick.
    ranked.sort_by_key(|(i, rule)| (rule.rank, Reverse(*i)));
    let mut text = format!("{HEADER}\n");
    for (_, rule) in &ranked {
        let Some((_, name)) = files.iter().find(|(p, _)| std::ptr::eq(*p, rule.profile)) else {
            continue;
        };
        let target = out.join(name).display().to_string();
        for condition in conditions(rule) {
            text.push_str(&format!(
                "[includeIf {}]\n\tpath = {}\n",
                quote(&condition),
                quote(&target)
            ));
        }
    }
    let includes = out.join(INCLUDES);
    fs::write(&includes, text).with_context(|| format!("writing {}", includes.display()))?;

    for entry in fs::read_dir(out)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let ours = fs::read_to_string(entry.path()).is_ok_and(|t| t.starts_with(HEADER));
        if name.ends_with(".gitconfig") && !keep.contains(&name) && ours {
            fs::remove_file(entry.path()).ok();
        }
    }
    Ok(Written {
        includes,
        profiles: files.len(),
        rules: ranked.len(),
    })
}

/// Rewrite the files after the profiles change, but only once the user has
/// exported before; returns whether anything was written.
pub fn refresh(config: &Config, config_path: &Path) -> Result<bool> {
    let out = dir(config_path);
    if !out.join(INCLUDES).exists() {
        return Ok(false);
    }
    write(config, &out)?;
    Ok(true)
}

fn included(existing: &[String], path: &Path) -> bool {
    existing
        .iter()
        .any(|p| git::expand_tilde(Path::new(p)) == path)
}

/// Add `include.path` to the global config unless it is there already.
fn install(includes: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["config", "--global", "--get-all", "include.path"])
        .output()
        .context("running git config --global")?;
    let existing: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    if included(&existing, includes) {
        return Ok(false);
    }
    let status = Command::new("git")
        .args(["config", "--global", "--add", "include.path"])
        .arg(includes)
        .status()
        .context("running git config --global --add")?;
    if !status.success() {
        bail!("git config --global --add include.path failed");
    }
    Ok(true)
}

/// `hasconfig:remote.*.url:` needs git 2.36.
fn git_has_hasconfig() -> bool {
    let Ok(output) = Command::new("git").arg("--version").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (major, minor) >= (2, 36)
}

pub fn run(do_install: bool) -> ExitCode {
    match try_run(do_install) {
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

fn try_run(do_install: bool) -> Result<String> {
    let config_path = Config::path()?;
    let config = Config::load(&config_path)?;
    let written = write(&config, &dir(&config_path))?;
    let mut lines = vec![format!(
        "gitenv: wrote {} profile(s) and {} rule(s) to {}",
        written.profiles,
        written.rules,
        written.includes.display()
    )];
    if written.rules == 0 {
        lines.push("  no profile has rules yet, so git will not switch identity by itself".into());
    }
    if config.profiles.iter().any(|p| !p.remotes.is_empty()) && !git_has_hasconfig() {
        lines.push(
            "  warning: remote rules need git 2.36 or newer; only path rules will apply".into(),
        );
    }
    if do_install {
        if install(&written.includes)? {
            lines.push("  added it to include.path in your global git config".into());
        } else {
            lines.push("  your global git config already includes it".into());
        }
        lines.push("  the TUI keeps it up to date when you edit profiles".into());
    } else {
        lines.push("  to have git read it, run: gitenv export --install".into());
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, email: &str) -> Profile {
        Profile {
            name: name.into(),
            user_name: "Ada".into(),
            email: email.into(),
            ..Default::default()
        }
    }

    #[test]
    fn values_and_subsections_are_quoted() {
        let mut p = profile("acme", "ada@acme.example");
        p.user_name = r#"Ada "the" \ Lovelace"#.into();
        p.git.insert(
            "url.git@github.com:acme/.insteadOf".into(),
            "https://github.com/acme/".into(),
        );
        let text = profile_file(&p);
        assert!(
            text.contains("[user]\n\tname = \"Ada \\\"the\\\" \\\\ Lovelace\"\n\temail"),
            "{text}"
        );
        assert!(
            text.contains(
                "[url \"git@github.com:acme/\"]\n\tinsteadOf = \"https://github.com/acme/\""
            ),
            "{text}"
        );
    }

    #[test]
    fn remote_rules_cover_ssh_and_https_spellings() {
        let mut p = profile("acme", "e");
        p.remotes = vec!["git@github.com:acme/*".into(), "github.com/acme/api".into()];
        let config = Config { profiles: vec![p] };
        let rules = rules::all(&config);
        let wild = conditions(&rules[0]);
        assert!(wild.contains(&"hasconfig:remote.*.url:*@github.com:acme/*".into()));
        assert!(wild.contains(&"hasconfig:remote.*.url:https://github.com/acme/*".into()));
        assert_eq!(wild.len(), 5);
        let exact = conditions(&rules[1]);
        assert!(exact.contains(&"hasconfig:remote.*.url:*@github.com:acme/api.git".into()));
        assert_eq!(exact.len(), 10);
    }

    #[test]
    fn path_rules_match_the_git_dir() {
        let mut p = profile("acme", "e");
        p.paths = vec!["/nowhere/work/acme/**".into(), "/nowhere/work/one".into()];
        let config = Config { profiles: vec![p] };
        let rules = rules::all(&config);
        assert_eq!(
            conditions(&rules[0]),
            vec!["gitdir/i:/nowhere/work/acme/**"]
        );
        assert_eq!(
            conditions(&rules[1]),
            vec!["gitdir/i:/nowhere/work/one/.git"]
        );
    }

    #[test]
    fn include_path_already_present_is_detected() {
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        let path = home.join(".config/gitenv/gitenv-includes/includes.gitconfig");
        assert!(included(&[path.display().to_string()], &path));
        assert!(included(
            &["~/.config/gitenv/gitenv-includes/includes.gitconfig".into()],
            &path
        ));
        assert!(!included(&["/elsewhere".into()], &path));
    }

    /// Real git, with a throwaway global config pointing at the export.
    #[test]
    fn git_picks_the_identity_from_the_export() {
        let base = std::env::temp_dir().join(format!("gitenv-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let out = base.join("out");
        let mut personal = profile("personal", "ada@example.com");
        personal.paths = vec![format!("{}/code/**", base.display())];
        let mut acme = profile("acme corp", "ada@acme.example");
        acme.remotes = vec!["github.com/acme/*".into()];
        acme.ssh_key = Some("/keys/acme".into());
        let unruled = profile("spare", "spare@example.com");
        let config = Config {
            profiles: vec![personal, acme, unruled],
        };
        // A file from an earlier export for a profile that has gone.
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("old.gitconfig"), format!("{HEADER}\n")).unwrap();
        fs::write(out.join("mine.gitconfig"), "[user]\n").unwrap();

        let written = write(&config, &out).unwrap();
        assert_eq!(written.profiles, 2);
        assert_eq!(written.rules, 2);
        assert!(!out.join("old.gitconfig").exists());
        assert!(out.join("mine.gitconfig").exists(), "not ours, left alone");
        assert!(out.join("acme_corp.gitconfig").exists());
        assert!(!out.join("spare.gitconfig").exists());

        let global = base.join("global.gitconfig");
        fs::write(
            &global,
            format!(
                "[include]\n\tpath = {}\n",
                quote(&written.includes.display().to_string())
            ),
        )
        .unwrap();
        let git = |dir: &Path, args: &[&str]| -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", &global)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        let init = |dir: &Path, remote: Option<&str>| {
            fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q"]);
            if let Some(url) = remote {
                git(dir, &["remote", "add", "origin", url]);
            }
        };

        let blog = base.join("code/blog");
        init(&blog, None);
        assert_eq!(git(&blog, &["config", "user.email"]), "ada@example.com");

        let api = base.join("elsewhere/api");
        init(&api, Some("git@github.com:acme/api.git"));
        assert_eq!(git(&api, &["config", "user.email"]), "ada@acme.example");
        assert_eq!(
            git(&api, &["config", "core.sshCommand"]),
            "ssh -i '/keys/acme' -o IdentitiesOnly=yes"
        );

        let web = base.join("elsewhere/web");
        init(&web, Some("https://github.com/acme/web.git"));
        assert_eq!(git(&web, &["config", "user.email"]), "ada@acme.example");

        // Both rules match; the remote rule is more specific and wins, as
        // it does in gitenv itself.
        let both = base.join("code/acme-site");
        init(&both, Some("git@github.com:acme/site.git"));
        assert_eq!(git(&both, &["config", "user.email"]), "ada@acme.example");

        let other = base.join("elsewhere/other");
        init(&other, Some("git@github.com:someone/other.git"));
        assert_eq!(git(&other, &["config", "user.email"]), "");

        fs::remove_dir_all(&base).unwrap();
    }
}
