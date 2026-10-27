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

pub fn dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("gitenv-includes")
}

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

pub struct Defaults {
    signing_format: String,
    sign_commits: String,
    ssh_command: String,
}

impl Defaults {
    fn git() -> Self {
        Self {
            signing_format: "openpgp".into(),
            sign_commits: "false".into(),
            ssh_command: "ssh".into(),
        }
    }

    fn global() -> Self {
        let get = |key: &str, fallback: String| {
            Command::new("git")
                .args(["config", "--global", "--includes", "--get", key])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or(fallback)
        };
        let git = Self::git();
        Self {
            signing_format: get("gpg.format", git.signing_format),
            sign_commits: get("commit.gpgsign", git.sign_commits),
            ssh_command: get("core.sshCommand", git.ssh_command),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Needs {
    signing_format: bool,
    sign_commits: bool,
    ssh_key: bool,
}

/// The settings in a profile's file. git keeps every key of every matching
/// include, so where a less specific rule of another profile can match the
/// same repos and sets gpg.format, commit.gpgsign or core.sshCommand, a
/// profile that leaves one unset writes the value it would have without that
/// other profile: the user's global setting, else git's default.
fn entries(profile: &Profile, needs: Needs, defaults: &Defaults) -> Vec<(String, String)> {
    let mut entries = vec![
        ("user.name".to_string(), profile.user_name.clone()),
        ("user.email".to_string(), profile.email.clone()),
    ];
    if let Some(key) = &profile.signing_key {
        entries.push(("user.signingkey".into(), key.clone()));
    }
    match profile.signing_format {
        Some(format) => entries.push(("gpg.format".into(), format.as_git().into())),
        None if needs.signing_format => {
            entries.push(("gpg.format".into(), defaults.signing_format.clone()))
        }
        None => {}
    }
    match profile.sign_commits {
        Some(sign) => entries.push(("commit.gpgsign".into(), sign.to_string())),
        None if needs.sign_commits => {
            entries.push(("commit.gpgsign".into(), defaults.sign_commits.clone()))
        }
        None => {}
    }
    match &profile.ssh_key {
        Some(key) => entries.push(("core.sshCommand".into(), git::ssh_command(key))),
        None if needs.ssh_key => {
            entries.push(("core.sshCommand".into(), defaults.ssh_command.clone()))
        }
        None => {}
    }
    entries.extend(profile.git.iter().map(|(k, v)| (k.clone(), v.clone())));
    entries
}

/// Whether some repository could match both rules. Two path rules, or two
/// remote rules, can only overlap when one's text before its first wildcard
/// starts with the other's; a path rule and a remote rule always can.
fn may_overlap(a: &Rule, b: &Rule) -> bool {
    let spellings = |r: &Rule| -> Vec<String> {
        std::iter::once(&r.pattern)
            .chain(&r.resolved)
            .map(|p| rules::literal_prefix(p).to_ascii_lowercase())
            .collect()
    };
    match (&a.reason, &b.reason) {
        (Reason::Path(_), Reason::Path(_)) | (Reason::Remote(_), Reason::Remote(_)) => {
            spellings(a).iter().any(|x| {
                spellings(b)
                    .iter()
                    .any(|y| x.starts_with(y.as_str()) || y.starts_with(x.as_str()))
            })
        }
        _ => true,
    }
}

/// `glob` matching letters of either case, as gitenv's own matching does:
/// each letter becomes a class of both cases, and letters and ranges inside
/// an existing class gain their other case too.
fn case_insensitive(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::with_capacity(glob.len() * 4);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '[' {
            let mut j = i + 1;
            if matches!(chars.get(j), Some('!' | '^')) {
                j += 1;
            }
            if chars.get(j) == Some(&']') {
                j += 1;
            }
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            if j < chars.len() {
                out.push_str(&case_class(&chars[i + 1..j]));
                i = j + 1;
                continue;
            }
        }
        if c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_lowercase());
            out.push(c.to_ascii_uppercase());
            out.push(']');
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn case_class(content: &[char]) -> String {
    let (negation, mut rest) = match content.first() {
        Some(&c @ ('!' | '^')) => (Some(c), &content[1..]),
        _ => (None, content),
    };
    let mut body = String::new();
    let mut extra = String::new();
    while let Some(&c) = rest.first() {
        if rest.len() >= 3 && rest[1] == '-' {
            let (from, to) = (c, rest[2]);
            body.extend([from, '-', to]);
            if from.is_ascii_alphabetic() && to.is_ascii_alphabetic() {
                let swap = |x: char| {
                    if x.is_ascii_lowercase() {
                        x.to_ascii_uppercase()
                    } else {
                        x.to_ascii_lowercase()
                    }
                };
                extra.extend([swap(from), '-', swap(to)]);
            }
            rest = &rest[3..];
        } else {
            body.push(c);
            if c.is_ascii_alphabetic() {
                extra.push(if c.is_ascii_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                });
            }
            rest = &rest[1..];
        }
    }
    let mut class = String::from("[");
    class.extend(negation);
    class.push_str(&body);
    class.push_str(&extra);
    class.push(']');
    class
}

/// The path part of an scp-style URL pattern (`host:<path>`). git's wildmatch
/// only lets `**` cross slashes at the start of a pattern or after a `/`, and
/// here it follows `:`, so a leading `**` is spelled out as no directory and
/// one or more.
fn scp_paths(path: &str) -> Vec<String> {
    match path.strip_prefix("**") {
        Some("") => vec!["*".into(), "*/**".into()],
        Some(rest) if rest.starts_with('/') => vec![rest[1..].to_string(), format!("*/{path}")],
        Some(rest) => vec![format!("*{rest}")],
        None => vec![path.to_string()],
    }
}

/// The `includeIf` conditions that hold wherever `rule` claims a repo. Path
/// rules match the `.git` directory git checks, as written and through
/// symlinks. Remote rules are spelled out for every URL form, with and without
/// user, port, `.git` and a trailing slash, and made case-insensitive by hand
/// because git compares remote URLs exactly.
pub fn conditions(rule: &Rule) -> Vec<String> {
    match &rule.reason {
        Reason::Path(_) => {
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
            let host_path = rules::remote_host_path(pattern);
            let (host, path) = host_path.split_once('/').unwrap_or((&host_path, "**"));
            let host = case_insensitive(host);
            let mut paths = vec![path.to_string()];
            if !path.ends_with('*') {
                paths.push(format!("{path}.git"));
            }
            let slashed: Vec<String> = paths.iter().map(|p| format!("{p}/")).collect();
            paths.extend(slashed);
            let mut urls = Vec::new();
            for path in &paths {
                let glob = case_insensitive(path);
                for prefix in ["", "*@"] {
                    for port in ["", ":*"] {
                        urls.push(format!("*://{prefix}{host}{port}/{glob}"));
                    }
                }
                for scp in scp_paths(&glob) {
                    for user in ["*@", ""] {
                        urls.push(format!("{user}{host}:{scp}"));
                        urls.push(format!("{user}{host}:/{scp}"));
                    }
                }
            }
            urls.dedup();
            urls.into_iter()
                .map(|url| format!("hasconfig:remote.*.url:{url}"))
                .collect()
        }
    }
}

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
            let mut name = format!("profile-{safe}.gitconfig");
            let mut n = 2;
            while taken.contains(&name.to_ascii_lowercase()) {
                name = format!("profile-{safe}-{n}.gitconfig");
                n += 1;
            }
            taken.push(name.to_ascii_lowercase());
            (p, name)
        })
        .collect()
}

#[derive(Debug)]
pub struct Written {
    pub includes: PathBuf,
    pub profiles: usize,
    pub rules: usize,
    pub skipped: Vec<String>,
    pub keys: Vec<String>,
}

fn check_keys(profile: &Profile) -> Result<()> {
    for key in profile.git.keys() {
        if !git::valid_key(key) {
            bail!(
                "profile '{}' has '{key}' under [profiles.git], which is not a git config key",
                profile.name
            );
        }
        let lower = key.to_ascii_lowercase();
        if !profile.remotes.is_empty() && lower.starts_with("remote.") && lower.ends_with(".url") {
            bail!(
                "profile '{}' sets {key}, which git refuses in a file included by a remote rule",
                profile.name
            );
        }
    }
    Ok(())
}

/// Writes each profile with rules to its own file and an `includes.gitconfig`
/// that loads them with `includeIf`, then removes files an earlier export left
/// for profiles that are gone. git keeps the last value it reads, so blocks go
/// least specific first, ties with the earliest profile last, and the value git
/// keeps is the one from the rule gitenv would pick. Relative path rules and
/// patterns that are not valid globs never match and are skipped.
pub fn write(config: &Config, out: &Path) -> Result<Written> {
    write_with(config, out, &Defaults::global())
}

fn write_with(config: &Config, out: &Path, defaults: &Defaults) -> Result<Written> {
    let mut ranked: Vec<(usize, Rule)> = rules::all(config).into_iter().enumerate().collect();
    ranked.sort_by_key(|(i, rule)| (rule.rank, Reverse(*i)));
    let mut skipped = Vec::new();
    ranked.retain(|(_, rule)| {
        let why = match rule.reason {
            Reason::Path(_) if !Path::new(&rule.pattern).is_absolute() => {
                Some("path rules must start with / or ~/")
            }
            _ if glob::Pattern::new(&rule.pattern).is_err() => Some("not a valid glob"),
            _ => None,
        };
        if let Some(why) = why {
            skipped.push(format!("'{}': {} ({why})", rule.profile.name, rule.reason));
        }
        why.is_none()
    });

    let mut exported: Vec<&Profile> = Vec::new();
    for (_, rule) in &ranked {
        if !exported.iter().any(|p| std::ptr::eq(*p, rule.profile)) {
            exported.push(rule.profile);
        }
    }
    exported.sort_by_key(|p| config.profiles.iter().position(|q| std::ptr::eq(q, *p)));
    for profile in &exported {
        check_keys(profile)?;
    }
    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let files = file_names(exported.iter().copied());

    let needs = |profile: &Profile| {
        let mut needs = Needs::default();
        for (n, (_, mine)) in ranked.iter().enumerate() {
            if !std::ptr::eq(mine.profile, profile) {
                continue;
            }
            for (_, theirs) in &ranked[..n] {
                let other = theirs.profile;
                if std::ptr::eq(other, profile) || !may_overlap(theirs, mine) {
                    continue;
                }
                needs.signing_format |= other.signing_format.is_some();
                needs.sign_commits |= other.sign_commits.is_some();
                needs.ssh_key |= other.ssh_key.is_some();
            }
        }
        needs
    };

    let mut keep = vec![INCLUDES.to_string()];
    let mut keys = Vec::new();
    for (profile, name) in &files {
        let path = out.join(name);
        let entries = entries(profile, needs(profile), defaults);
        keys.extend(entries.iter().map(|(k, _)| git::normalize_key(k)));
        fs::write(&path, render(&entries))
            .with_context(|| format!("writing {}", path.display()))?;
        keep.push(name.clone());
    }
    keys.sort();
    keys.dedup();

    let mut text = format!("{HEADER}\n");
    for (_, rule) in &ranked {
        let Some((_, name)) = files.iter().find(|(p, _)| std::ptr::eq(*p, rule.profile)) else {
            continue;
        };
        let target = out.join(name).display().to_string();
        text.push_str(&format!("# {}: {}\n", rule.profile.name, rule.reason));
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
        skipped,
        keys,
    })
}

pub fn refresh(config: &Config, config_path: &Path) -> Result<bool> {
    let out = dir(config_path);
    if !out.join(INCLUDES).exists() {
        return Ok(false);
    }
    write(config, &out)?;
    Ok(true)
}

fn global_config_file() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GIT_CONFIG_GLOBAL").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = directories::UserDirs::new()
        .context("could not find your home directory")?
        .home_dir()
        .to_path_buf();
    let dotfile = home.join(".gitconfig");
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".config"), PathBuf::from)
        .join("git/config");
    Ok(if !dotfile.exists() && xdg.exists() {
        xdg
    } else {
        dotfile
    })
}

fn same_file(written: &str, config_file: &Path, target: &Path) -> bool {
    let path = git::expand_tilde(Path::new(written));
    let path = match config_file.parent() {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path,
    };
    let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(&path) == real(target)
}

#[derive(Debug, PartialEq, Eq)]
enum Installed {
    Added,
    Moved,
    AlreadyThere,
}

/// Makes `config_file` include `includes` after every setting of `keys`, since
/// git keeps the last value it reads. A new `[include]` section is appended
/// rather than using `git config --add`, which would put the line in an
/// existing `[include]` section, often at the top of the file.
fn install_into(config_file: &Path, includes: &Path, keys: &[String]) -> Result<Installed> {
    let output = Command::new("git")
        .args(["config", "--file"])
        .arg(config_file)
        .args(["--includes", "--list", "-z"])
        .output()
        .context("running git config --file")?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let entries: Vec<(&str, &str)> = text
        .split('\0')
        .filter(|e| !e.is_empty())
        .map(|e| e.split_once('\n').unwrap_or((e, "")))
        .collect();
    let mut ours: Vec<&str> = entries
        .iter()
        .filter(|(k, v)| *k == "include.path" && same_file(v, config_file, includes))
        .map(|(_, v)| *v)
        .collect();
    ours.sort_unstable();
    ours.dedup();
    let position =
        |pred: &dyn Fn(&str, &str) -> bool| entries.iter().rposition(|(k, v)| pred(k, v));
    let last_ours = position(&|k, v| k == "include.path" && same_file(v, config_file, includes));
    let last_setting = position(&|k, _| {
        k == "include.path" || keys.iter().any(|key| *key == git::normalize_key(k))
    });
    let outcome = match (last_ours, last_setting) {
        (Some(ours), Some(setting)) if setting > ours => Installed::Moved,
        (Some(_), _) => return Ok(Installed::AlreadyThere),
        (None, _) => Installed::Added,
    };
    for value in ours {
        let status = Command::new("git")
            .args(["config", "--file"])
            .arg(config_file)
            .args(["--fixed-value", "--unset-all", "include.path", value])
            .status()
            .context("running git config --unset-all")?;
        if !status.success() {
            bail!(
                "could not remove the old include.path from {}",
                config_file.display()
            );
        }
    }
    let mut current = fs::read_to_string(config_file).unwrap_or_default();
    if !current.is_empty() && !current.ends_with('\n') {
        current.push('\n');
    }
    current.push_str(&format!(
        "[include]\n\tpath = {}\n",
        quote(&includes.display().to_string())
    ));
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(config_file, current)
        .with_context(|| format!("writing {}", config_file.display()))?;
    Ok(outcome)
}

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
    for rule in &written.skipped {
        lines.push(format!(
            "  skipped {rule}: path rules must start with / or ~/"
        ));
    }
    if written.rules == 0 {
        lines.push("  no profile has rules yet, so git will not switch identity by itself".into());
    }
    if config.profiles.iter().any(|p| !p.remotes.is_empty()) && !git_has_hasconfig() {
        lines.push(
            "  warning: remote rules need git 2.36 or newer; only path rules will apply".into(),
        );
    }
    if do_install {
        let file = global_config_file()?;
        lines.push(
            match install_into(&file, &written.includes, &written.keys)? {
                Installed::Added => format!("  added it to the end of {}", file.display()),
                Installed::Moved => format!(
                    "  moved it to the end of {}, after settings that were overriding it",
                    file.display()
                ),
                Installed::AlreadyThere => format!("  {} already includes it", file.display()),
            },
        );
        lines.push("  the TUI keeps it up to date when you edit profiles".into());
    } else {
        lines.push("  to have git read it, run: gitenv export --install".into());
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Repo;

    fn write(config: &Config, out: &Path) -> Result<Written> {
        write_with(config, out, &Defaults::git())
    }

    fn profile(name: &str, email: &str) -> Profile {
        Profile {
            name: name.into(),
            user_name: "Ada".into(),
            email: email.into(),
            ..Default::default()
        }
    }

    fn base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitenv-export-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_with(global: &Path, dir: &Path, args: &[&str]) -> (bool, String) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", global)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )
    }

    fn export_to(config: &Config, base: &Path) -> PathBuf {
        let written = write(config, &base.join("out")).unwrap();
        let global = base.join("global.gitconfig");
        fs::write(
            &global,
            format!(
                "[user]\n\temail = global@example.com\n[include]\n\tpath = {}\n",
                quote(&written.includes.display().to_string())
            ),
        )
        .unwrap();
        global
    }

    fn repo_with_remote(global: &Path, dir: &Path, url: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        git_with(global, dir, &["init", "-q"]);
        if let Some(url) = url {
            git_with(global, dir, &["remote", "add", "origin", url]);
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
        let text = render(&entries(&p, Needs::default(), &Defaults::git()));
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
    fn git_and_gitenv_agree_on_every_url_spelling() {
        let base = base("urls");
        let mut acme = profile("acme", "ada@acme.example");
        acme.remotes = vec!["github.com/acme/*".into()];
        let mut lab = profile("lab", "ada@lab.example");
        lab.remotes = vec!["gitlab.com/**".into()];
        let mut bucket = profile("bucket", "ada@bucket.example");
        bucket.remotes = vec!["bitbucket.org".into(), "git.example/[a-c]cme/*".into()];
        let config = Config {
            profiles: vec![acme, lab, bucket],
        };
        let global = export_to(&config, &base);
        let urls = [
            "git@github.com:acme/api.git",
            "git@github.com:Acme/api.git",
            "git@GitHub.com:acme/api.git",
            "https://github.com/ACME/api",
            "ssh://github.com/acme/api",
            "ssh://git@github.com:22/acme/api.git",
            "github.com:acme/api",
            "git@github.com:/acme/api.git",
            "http://github.com/acme/api",
            "git://github.com/acme/api",
            "git+ssh://git@github.com/acme/api",
            "https://github.com:443/acme/api",
            "https://github.com/acme/api/",
            "https://github.com/acme/api.git/",
            "https://user:tok@github.com/acme/api.git",
            "git@gitlab.com:a/b/c.git",
            "git@gitlab.com:x.git",
            "git@gitlab.com:/a/b.git",
            "https://gitlab.com/a/b/c",
            "git@github.com:someone/x.git",
            "git@github.com:acme/api/extra.git",
            "git@bitbucket.org:team/repo.git",
            "https://bitbucket.org/team/sub/repo",
            "git@git.example:Acme/x.git",
            "https://git.example/BCME/y.GIT",
            "git@git.example:dcme/z.git",
        ];
        for (n, url) in urls.iter().enumerate() {
            let dir = base.join(format!("r{n}"));
            repo_with_remote(&global, &dir, Some(url));
            let (_, email) = git_with(&global, &dir, &["config", "user.email"]);
            let repo = Repo {
                root: dir.clone(),
                remote: Some(url.to_string()),
                effective: Default::default(),
                local: Default::default(),
                authors: Vec::new(),
            };
            let want = rules::expected(&config, &repo)
                .map_or("global@example.com".to_string(), |(p, _)| p.email.clone());
            assert_eq!(email, want, "{url}");
        }
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_broader_profile_leaks_nothing_into_a_narrower_ones_repos() {
        let base = base("leak");
        let mut personal = profile("personal", "ada@example.com");
        personal.paths = vec![format!("{}/w/**", base.display())];
        personal.signing_key = Some("PERSONALKEY".into());
        personal.sign_commits = Some(true);
        personal.signing_format = Some(crate::profile::SigningFormat::Ssh);
        personal.ssh_key = Some("/keys/personal".into());
        let mut acme = profile("acme", "ada@acme.example");
        acme.paths = vec![format!("{}/w/clients/acme/**", base.display())];
        let global = export_to(
            &Config {
                profiles: vec![personal, acme],
            },
            &base,
        );
        let site = base.join("w/clients/acme/site");
        repo_with_remote(&global, &site, None);
        let get = |key: &str| git_with(&global, &site, &["config", key]).1;
        assert_eq!(get("user.email"), "ada@acme.example");
        assert_eq!(get("commit.gpgsign"), "false");
        assert_eq!(get("gpg.format"), "openpgp");
        assert_eq!(get("core.sshCommand"), "ssh");
        let blog = base.join("w/blog");
        repo_with_remote(&global, &blog, None);
        assert_eq!(
            git_with(&global, &blog, &["config", "commit.gpgsign"]).1,
            "true"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn file_names_never_clash_with_each_other_or_the_includes_file() {
        let base = base("names");
        let mut includes = profile("includes", "inc@example.com");
        includes.paths = vec![format!("{}/inc/**", base.display())];
        let mut big = profile("Work", "big@example.com");
        big.paths = vec![format!("{}/big/**", base.display())];
        let mut small = profile("work", "small@example.com");
        small.paths = vec![format!("{}/small/**", base.display())];
        let global = export_to(
            &Config {
                profiles: vec![includes, big, small],
            },
            &base,
        );
        for (dir, email) in [
            ("inc/a", "inc@example.com"),
            ("big/a", "big@example.com"),
            ("small/a", "small@example.com"),
        ] {
            let dir = base.join(dir);
            repo_with_remote(&global, &dir, None);
            let (ok, got) = git_with(&global, &dir, &["config", "user.email"]);
            assert!(ok);
            assert_eq!(got, email);
        }
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn bad_keys_are_refused_and_relative_rules_skipped() {
        let base = base("bad");
        let mut p = profile("p", "e@x");
        p.paths = vec!["/abs/**".into(), "rel/**".into()];
        let written = write(
            &Config {
                profiles: vec![p.clone()],
            },
            &base,
        )
        .unwrap();
        assert_eq!(written.rules, 1);
        assert_eq!(written.skipped.len(), 1);

        p.git.insert("core".into(), "v".into());
        let err = write(
            &Config {
                profiles: vec![p.clone()],
            },
            &base,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a git config key"), "{err}");

        p.git.clear();
        p.remotes = vec!["github.com/acme/*".into()];
        p.git.insert("remote.upstream.url".into(), "x".into());
        let err = write(&Config { profiles: vec![p] }, &base).unwrap_err();
        assert!(err.to_string().contains("refuses"), "{err}");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn install_goes_after_settings_that_would_override_it() {
        let base = base("install");
        let file = base.join("global.gitconfig");
        let includes = base.join("out/includes.gitconfig");
        fs::create_dir_all(base.join("out")).unwrap();
        fs::write(&includes, "").unwrap();
        fs::write(
            &file,
            "[include]\n\tpath = ~/.gitconfig.local\n[user]\n\tname = Global\n\temail = g@x\n",
        )
        .unwrap();
        let keys = vec!["user.email".to_string(), "user.name".to_string()];
        assert_eq!(
            install_into(&file, &includes, &keys).unwrap(),
            Installed::Added
        );
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.ends_with(&format!(
            "[include]\n\tpath = {}\n",
            quote(&includes.display().to_string())
        )));
        assert_eq!(
            install_into(&file, &includes, &keys).unwrap(),
            Installed::AlreadyThere
        );

        fs::write(&file, format!("{text}[user]\n\temail = later@x\n")).unwrap();
        assert_eq!(
            install_into(&file, &includes, &keys).unwrap(),
            Installed::Moved
        );
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(text.matches("includes.gitconfig").count(), 1, "{text}");
        assert!(text.trim_end().ends_with("includes.gitconfig\""), "{text}");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn git_picks_the_identity_from_the_export() {
        let base = base("e2e");
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
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("old.gitconfig"), format!("{HEADER}\n")).unwrap();
        fs::write(out.join("mine.gitconfig"), "[user]\n").unwrap();

        let written = write(&config, &out).unwrap();
        assert_eq!(written.profiles, 2);
        assert_eq!(written.rules, 2);
        assert!(!out.join("old.gitconfig").exists());
        assert!(out.join("mine.gitconfig").exists(), "not ours, left alone");
        assert!(out.join("profile-acme_corp.gitconfig").exists());
        assert!(!out.join("profile-spare.gitconfig").exists());

        let global = export_to(&config, &base);
        let blog = base.join("code/blog");
        repo_with_remote(&global, &blog, None);
        assert_eq!(
            git_with(&global, &blog, &["config", "user.email"]).1,
            "ada@example.com"
        );
        let api = base.join("elsewhere/api");
        repo_with_remote(&global, &api, Some("git@github.com:acme/api.git"));
        assert_eq!(
            git_with(&global, &api, &["config", "user.email"]).1,
            "ada@acme.example"
        );
        assert_eq!(
            git_with(&global, &api, &["config", "core.sshCommand"]).1,
            "ssh -i '/keys/acme' -o IdentitiesOnly=yes"
        );
        let both = base.join("code/acme-site");
        repo_with_remote(&global, &both, Some("git@github.com:acme/site.git"));
        assert_eq!(
            git_with(&global, &both, &["config", "user.email"]).1,
            "ada@acme.example"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn defaults_only_where_a_broader_rule_overlaps_and_from_the_global_config() {
        let base = base("defaults");
        let mut signer = profile("signer", "s@example.com");
        signer.paths = vec![format!("{}/a/**", base.display())];
        signer.sign_commits = Some(true);
        signer.ssh_key = Some("/keys/s".into());
        let mut apart = profile("apart", "apart@example.com");
        apart.paths = vec![format!("{}/b/**", base.display())];
        let mut inside = profile("inside", "inside@example.com");
        inside.paths = vec![format!("{}/a/inner/**", base.display())];
        let config = Config {
            profiles: vec![signer, apart, inside],
        };
        let defaults = Defaults {
            signing_format: "ssh".into(),
            sign_commits: "true".into(),
            ssh_command: "ssh -i ~/.ssh/global".into(),
        };
        let out = base.join("out");
        write_with(&config, &out, &defaults).unwrap();
        let apart = fs::read_to_string(out.join("profile-apart.gitconfig")).unwrap();
        assert!(
            !apart.contains("gpgsign") && !apart.contains("sshCommand"),
            "{apart}"
        );
        let inside = fs::read_to_string(out.join("profile-inside.gitconfig")).unwrap();
        assert!(inside.contains("gpgsign = \"true\""), "{inside}");
        assert!(
            inside.contains("sshCommand = \"ssh -i ~/.ssh/global\""),
            "{inside}"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn classes_become_case_insensitive_too() {
        assert_eq!(case_insensitive("[a-c]x"), "[a-cA-C][xX]");
        assert_eq!(case_insensitive("[!x]y"), "[!xX][yY]");
        assert_eq!(case_insensitive("[]a]"), "[]aA]");
        assert_eq!(case_insensitive("a?*"), "[aA]?*");
    }

    #[test]
    fn install_handles_duplicates_and_later_includes() {
        let base = base("install2");
        let file = base.join("global.gitconfig");
        let local = base.join("local.gitconfig");
        let includes = base.join("out/includes.gitconfig");
        fs::create_dir_all(base.join("out")).unwrap();
        fs::write(&includes, "").unwrap();
        fs::write(&local, "[user]\n\temail = local@x\n").unwrap();
        let ours = quote(&includes.display().to_string());
        fs::write(
            &file,
            format!("[include]\n\tpath = {ours}\n[include]\n\tpath = {ours}\n[user]\n\temail = later@x\n"),
        )
        .unwrap();
        let keys = vec!["user.email".to_string()];
        assert_eq!(
            install_into(&file, &includes, &keys).unwrap(),
            Installed::Moved
        );
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(text.matches("includes.gitconfig").count(), 1, "{text}");

        let with_local = format!(
            "{text}[include]\n\tpath = {}\n",
            quote(&local.display().to_string())
        );
        fs::write(&file, with_local).unwrap();
        assert_eq!(
            install_into(&file, &includes, &keys).unwrap(),
            Installed::Moved
        );
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.trim_end().ends_with("includes.gitconfig\""), "{text}");
        fs::remove_dir_all(&base).unwrap();
    }
}
