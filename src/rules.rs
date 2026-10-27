use std::cmp::Reverse;
use std::path::Path;

use glob::{MatchOptions, Pattern};

use crate::git::Repo;
use crate::profile::{Config, Profile};

const OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

fn normalize_remote(url: &str) -> String {
    remote_host_path(url).to_ascii_lowercase()
}

/// Reduces every way of writing a remote to `host/owner/repo`:
/// `git@github.com:acme/api.git`, `https://user:token@github.com/acme/api.git`
/// and `ssh://git@github.com:22/acme/api` all give `github.com/acme/api`.
/// Patterns go through it too, so a rule can be written in whichever form the
/// hosting provider shows. With a scheme the host part ends at the first `/`,
/// so a `:` before an `@` there is a password, and only there can a port follow
/// the host.
pub fn remote_host_path(url: &str) -> String {
    let url = url.trim();
    let (has_scheme, mut s) = match url.split_once("://") {
        Some((_, rest)) => (true, rest),
        None => (false, url),
    };
    let host_end = if has_scheme {
        s.find('/')
    } else {
        s.find(['/', ':'])
    }
    .unwrap_or(s.len());
    if let Some(at) = s[..host_end].rfind('@') {
        s = &s[at + 1..];
    }
    let (host, mut path) = s.split_once(['/', ':']).unwrap_or((s, ""));
    if has_scheme
        && let Some((port, rest)) = path.split_once('/')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        path = rest;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    format!("{host}/{path}").trim_end_matches('/').to_string()
}

fn expand_path(pattern: &str) -> String {
    let expanded = crate::git::expand_tilde(Path::new(pattern))
        .display()
        .to_string();
    match expanded.trim_end_matches('/') {
        "" => expanded,
        trimmed => trimmed.to_string(),
    }
}

/// `pattern` with its leading directories resolved through symlinks, when that
/// changes it. git reports a repository's resolved path, so a rule written
/// through a symlink would otherwise never match. The longest leading directory
/// that exists is used, so a rule for a folder not created yet still sees
/// through a symlinked parent.
fn resolve_path(pattern: &str) -> Option<String> {
    if !Path::new(pattern).is_absolute() {
        return None;
    }
    let mut end = pattern[..literal_len(pattern)].rfind('/');
    while let Some(e) = end.filter(|&e| e > 0) {
        if let Ok(real) = std::fs::canonicalize(&pattern[..e]) {
            let resolved = format!("{}{}", real.display(), &pattern[e..]);
            return (resolved != pattern).then_some(resolved);
        }
        end = pattern[..e].rfind('/');
    }
    None
}

fn matches(pattern: &str, subject: &str) -> bool {
    Pattern::new(pattern).is_ok_and(|p| p.matches_with(subject, OPTIONS))
}

fn path_matches(pattern: &str, root: &str) -> bool {
    matches(pattern, root)
        || pattern
            .strip_suffix("/**")
            .is_some_and(|dir| matches(dir, root))
}

fn literal_len(pattern: &str) -> usize {
    pattern.find(['*', '?', '[']).unwrap_or(pattern.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Remote(String),
    Path(String),
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::Remote(p) => write!(f, "remote matches {p}"),
            Reason::Path(p) => write!(f, "path matches {p}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rule<'a> {
    pub profile: &'a Profile,
    pub reason: Reason,
    pub rank: (bool, usize),
    pub pattern: String,
    pub resolved: Option<String>,
}

pub fn all(config: &Config) -> Vec<Rule<'_>> {
    config
        .profiles
        .iter()
        .flat_map(|profile| {
            let remotes = profile.remotes.iter().map(move |rule| {
                let pattern = normalize_remote(rule);
                Rule {
                    profile,
                    reason: Reason::Remote(rule.clone()),
                    rank: (true, literal_len(&pattern)),
                    pattern,
                    resolved: None,
                }
            });
            let paths = profile.paths.iter().map(move |rule| {
                let pattern = expand_path(rule);
                Rule {
                    profile,
                    reason: Reason::Path(rule.clone()),
                    rank: (false, literal_len(&pattern)),
                    resolved: resolve_path(&pattern),
                    pattern,
                }
            });
            remotes.chain(paths)
        })
        .collect()
}

/// The profile whose rule claims `repo`, and the rule. The most specific
/// matching rule wins: a remote rule beats a path rule, then the one with the
/// longer literal prefix, then the earlier one in the file.
pub fn expected<'a>(config: &'a Config, repo: &Repo) -> Option<(&'a Profile, Reason)> {
    let urls: Vec<String> = repo
        .rule_remotes()
        .into_iter()
        .map(normalize_remote)
        .collect();
    let root = repo.root.display().to_string();
    all(config)
        .into_iter()
        .filter(|rule| match rule.reason {
            Reason::Remote(_) => urls.iter().any(|url| matches(&rule.pattern, url)),
            Reason::Path(_) => {
                path_matches(&rule.pattern, &root)
                    || rule
                        .resolved
                        .as_ref()
                        .is_some_and(|p| path_matches(p, &root))
            }
        })
        .min_by_key(|rule| Reverse(rule.rank))
        .map(|rule| (rule.profile, rule.reason))
}

pub fn validate(patterns: &[String]) -> Result<(), String> {
    for p in patterns {
        Pattern::new(p).map_err(|e| format!("Bad pattern '{p}': {}", e.msg))?;
    }
    Ok(())
}

pub fn validate_paths(patterns: &[String]) -> Result<(), String> {
    validate(patterns)?;
    match patterns
        .iter()
        .find(|p| !Path::new(&expand_path(p)).is_absolute())
    {
        Some(p) => Err(format!(
            "Path rule '{p}' must start with / or ~/ to match anything"
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_matches(pattern: &str, url: &str) -> bool {
        matches(&normalize_remote(pattern), &normalize_remote(url))
    }

    #[test]
    fn remotes_normalise_to_host_slash_path() {
        for url in [
            "git@github.com:acme/api.git",
            "https://github.com/acme/api.git",
            "https://github.com/acme/api",
            "ssh://git@github.com/acme/api.git",
            "ssh://git@github.com:22/acme/api",
            "GIT@GitHub.com:Acme/API.git",
            "https://user@github.com/acme/api/",
        ] {
            assert_eq!(normalize_remote(url), "github.com/acme/api", "{url}");
        }
        assert_eq!(normalize_remote("github.com"), "github.com");
    }

    #[test]
    fn remote_patterns_match_any_spelling() {
        let url = "git@github.com:acme/api.git";
        assert!(remote_matches("github.com/acme/*", url));
        assert!(remote_matches("git@github.com:acme/*", url));
        assert!(remote_matches("https://github.com/acme/*.git", url));
        assert!(remote_matches("github.com/acme/api", url));
        assert!(remote_matches("*/acme/*", url));
        assert!(!remote_matches("github.com/other/*", url));
        assert!(!remote_matches("github.com/*", url));
        assert!(remote_matches("github.com/**", url));
    }

    #[test]
    fn path_patterns_cover_the_directory_itself_and_children() {
        let root = "/work/acme/api";
        assert!(path_matches("/work/acme/**", root));
        assert!(path_matches("/work/acme/*", root));
        assert!(path_matches("/work/acme/api", root));
        assert!(path_matches("/work/acme/api/**", root));
        assert!(!path_matches("/work/other/**", root));
        assert!(!path_matches("/work/*", root));
    }

    #[cfg(unix)]
    #[test]
    fn path_patterns_see_through_symlinks() {
        let base = std::env::temp_dir().join(format!("gitenv-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real/api")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let root = std::fs::canonicalize(base.join("real/api")).unwrap();
        let root = root.display().to_string();
        let pattern = format!("{}/link/**", base.display());
        let resolved = resolve_path(&pattern).unwrap();
        assert!(path_matches(&resolved, &root), "{resolved} vs {root}");
        let exact = format!("{}/link/api", base.display());
        assert!(path_matches(&resolve_path(&exact).unwrap(), &root));
        let missing = format!("{}/link/not-yet/**", base.display());
        assert!(
            resolve_path(&missing)
                .unwrap()
                .ends_with("/real/not-yet/**")
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn tilde_in_path_pattern_is_home() {
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        let root = home.join("work/acme/api").display().to_string();
        assert!(path_matches(&expand_path("~/work/acme/**"), &root));
    }

    fn profile(name: &str, remotes: &[&str], paths: &[&str]) -> Profile {
        Profile {
            name: name.into(),
            user_name: "Ada".into(),
            email: format!("ada@{name}.example"),
            remotes: remotes.iter().map(|s| s.to_string()).collect(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn repo(remote: Option<&str>, root: &str) -> Repo {
        Repo {
            root: root.into(),
            remote: remote.map(Into::into),
            effective: Default::default(),
            local: Default::default(),
            authors: Vec::new(),
        }
    }

    #[test]
    fn first_claiming_profile_wins_and_says_why() {
        let config = Config {
            profiles: vec![
                profile("personal", &["github.com/ada/*"], &["/home/ada/play/**"]),
                profile("acme", &["github.com/acme/*"], &["/home/ada/work/**"]),
            ],
        };
        let (p, why) = expected(
            &config,
            &repo(Some("git@github.com:acme/api.git"), "/tmp/x"),
        )
        .unwrap();
        assert_eq!(p.name, "acme");
        assert_eq!(why, Reason::Remote("github.com/acme/*".into()));

        let (p, why) = expected(&config, &repo(None, "/home/ada/work/thing")).unwrap();
        assert_eq!(p.name, "acme");
        assert_eq!(why, Reason::Path("/home/ada/work/**".into()));

        assert!(expected(&config, &repo(Some("git@gitlab.com:x/y.git"), "/tmp/x")).is_none());
    }

    #[test]
    fn most_specific_rule_wins_whatever_the_order() {
        let config = Config {
            profiles: vec![
                profile("personal", &["github.com/*/*"], &["/home/ada/projects/**"]),
                profile("agency", &[], &["/home/ada/projects/clients/agency/**"]),
                profile("acme", &["github.com/acme/*"], &[]),
            ],
        };
        let (p, why) = expected(
            &config,
            &repo(None, "/home/ada/projects/clients/agency/site"),
        )
        .unwrap();
        assert_eq!(p.name, "agency");
        assert_eq!(
            why,
            Reason::Path("/home/ada/projects/clients/agency/**".into())
        );

        let (p, _) = expected(&config, &repo(None, "/home/ada/projects/blog")).unwrap();
        assert_eq!(p.name, "personal");

        let (p, _) = expected(
            &config,
            &repo(Some("git@github.com:acme/api.git"), "/tmp/x"),
        )
        .unwrap();
        assert_eq!(p.name, "acme");

        let (p, why) = expected(
            &config,
            &repo(
                Some("git@github.com:acme/api.git"),
                "/home/ada/projects/clients/agency/api",
            ),
        )
        .unwrap();
        assert_eq!(p.name, "acme");
        assert_eq!(why, Reason::Remote("github.com/acme/*".into()));
    }

    #[test]
    fn validate_rejects_bad_globs() {
        assert!(validate(&["github.com/acme/*".into()]).is_ok());
        assert!(validate(&["[unclosed".into()]).is_err());
    }

    #[test]
    fn credentials_in_urls_are_not_the_host() {
        for url in [
            "https://user:pass@github.com/acme/api.git/",
            "https://oauth2:glpat-xyz@github.com/acme/api.git",
            "https://x-access-token:ghs_1@github.com/acme/api",
        ] {
            assert_eq!(normalize_remote(url), "github.com/acme/api", "{url}");
        }
    }

    #[test]
    fn trailing_slash_path_rule_and_relative_rules() {
        assert!(path_matches(&expand_path("/work/trail/"), "/work/trail"));
        assert!(validate_paths(&["~/work/**".into(), "/srv/*".into()]).is_ok());
        assert!(validate_paths(&["work/**".into()]).is_err());
        assert!(resolve_path("p/rel/**").is_none());
    }
}
