//! Which profile a repository *should* be on.
//!
//! A profile can claim repositories by remote URL or by path. Each rule is
//! a glob. The most specific matching rule wins: a remote rule beats a path
//! rule, then the rule with the longer literal prefix, then the earlier one.

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

/// Reduce every way of writing a remote to `host/owner/repo`, lowercase:
///
/// - `git@github.com:acme/api.git`        -> `github.com/acme/api`
/// - `https://github.com/acme/api.git`    -> `github.com/acme/api`
/// - `ssh://git@github.com:22/acme/api`   -> `github.com/acme/api`
///
/// Patterns are passed through the same function, so a rule can be written
/// in whichever form the user copies from their hosting provider.
fn normalize_remote(url: &str) -> String {
    let url = url.trim();
    let (has_scheme, mut s) = match url.split_once("://") {
        Some((_, rest)) => (true, rest),
        None => (false, url),
    };
    // Drop `user@`, but only when the `@` is part of the host, not the path.
    let host_end = s.find(['/', ':']).unwrap_or(s.len());
    if let Some(at) = s[..host_end].rfind('@') {
        s = &s[at + 1..];
    }
    let (host, mut path) = s.split_once(['/', ':']).unwrap_or((s, ""));
    // `ssh://host:22/path` carries a port; scp-like `host:path` never does.
    if has_scheme
        && let Some((port, rest)) = path.split_once('/')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        path = rest;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    format!("{host}/{path}")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// A leading `~` is the home directory.
fn expand_path(pattern: &str) -> String {
    crate::git::expand_tilde(Path::new(pattern))
        .display()
        .to_string()
}

fn matches(pattern: &str, subject: &str) -> bool {
    Pattern::new(pattern).is_ok_and(|p| p.matches_with(subject, OPTIONS))
}

/// `dir/**` also matches `dir` itself.
fn path_matches(pattern: &str, root: &str) -> bool {
    matches(pattern, root)
        || pattern
            .strip_suffix("/**")
            .is_some_and(|dir| matches(dir, root))
}

/// How much of a pattern is plain text before the first wildcard.
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

pub fn expected<'a>(config: &'a Config, repo: &Repo) -> Option<(&'a Profile, Reason)> {
    let url = repo.remote.as_deref().map(normalize_remote);
    let root = repo.root.display().to_string();
    config
        .profiles
        .iter()
        .flat_map(|profile| {
            let remotes = profile.remotes.iter().filter_map(|rule| {
                let pattern = normalize_remote(rule);
                matches(&pattern, url.as_deref()?)
                    .then(|| ((true, literal_len(&pattern)), Reason::Remote(rule.clone())))
            });
            let paths = profile.paths.iter().filter_map(|rule| {
                let pattern = expand_path(rule);
                path_matches(&pattern, &root)
                    .then(|| ((false, literal_len(&pattern)), Reason::Path(rule.clone())))
            });
            remotes
                .chain(paths)
                .map(move |(rank, why)| (rank, profile, why))
        })
        // `min_by_key` keeps the first of equals, so ties go to file order.
        .min_by_key(|(rank, ..)| Reverse(*rank))
        .map(|(_, profile, why)| (profile, why))
}

pub fn validate(patterns: &[String]) -> Result<(), String> {
    for p in patterns {
        Pattern::new(p).map_err(|e| format!("Bad pattern '{p}': {}", e.msg))?;
    }
    Ok(())
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
        // `*` stops at a slash: owner-level rule does not match the repo.
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

        // A remote says more about a repo than where it was cloned to.
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
}
