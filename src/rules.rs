//! Which profile a repository *should* be on.
//!
//! A profile can claim repositories by remote URL or by path. Each rule is
//! a glob; the first profile with a matching rule is the expected one.
//! Rails analogy: route constraints. The identity check says whether the
//! request got to a controller at all; these say whether it was the right
//! controller.

use std::path::Path;

use glob::{MatchOptions, Pattern};

use crate::git::Repo;
use crate::profile::{Config, Profile};

/// Reduce every way of writing a remote to `host/owner/repo`, lowercase:
///
/// - `git@github.com:acme/api.git`        -> `github.com/acme/api`
/// - `https://github.com/acme/api.git`    -> `github.com/acme/api`
/// - `ssh://git@github.com:22/acme/api`   -> `github.com/acme/api`
///
/// Patterns are passed through the same function, so a rule can be written
/// in whichever form the user copies from their hosting provider.
pub fn normalize_remote(url: &str) -> String {
    let mut s = url.trim();
    let has_scheme = match s.find("://") {
        Some(idx) => {
            s = &s[idx + 3..];
            true
        }
        None => false,
    };
    // Drop `user@`, but only when the `@` is part of the host, not the path.
    let host_end = s.find(['/', ':']).unwrap_or(s.len());
    if let Some(at) = s[..host_end].rfind('@') {
        s = &s[at + 1..];
    }
    let (host, mut path) = match s.find(['/', ':']) {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => (s, ""),
    };
    // `ssh://host:22/path` carries a port; scp-like `host:path` never does.
    if has_scheme
        && let Some(slash) = path.find('/')
        && !path[..slash].is_empty()
        && path[..slash].bytes().all(|b| b.is_ascii_digit())
    {
        path = &path[slash + 1..];
    }
    let path = path
        .trim_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_matches('/'))
        .trim_end_matches('/');
    let joined = if path.is_empty() {
        host.to_string()
    } else {
        format!("{host}/{path}")
    };
    joined.to_ascii_lowercase()
}

fn options() -> MatchOptions {
    MatchOptions {
        case_sensitive: false,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

/// Does `pattern` claim the remote `url`? Both go through
/// [`normalize_remote`] first, so `github.com/acme/*` matches
/// `git@github.com:acme/api.git`.
pub fn remote_matches(pattern: &str, url: &str) -> bool {
    match Pattern::new(&normalize_remote(pattern)) {
        Ok(p) => p.matches_with(&normalize_remote(url), options()),
        Err(_) => false,
    }
}

/// Does `pattern` claim the repository at `root`? A leading `~` in the
/// pattern is the home directory, and `dir/**` also matches `dir` itself.
pub fn path_matches(pattern: &str, root: &Path) -> bool {
    let expanded = crate::git::expand_tilde(Path::new(pattern))
        .display()
        .to_string();
    let Ok(p) = Pattern::new(&expanded) else {
        return false;
    };
    let root = root.display().to_string();
    if p.matches_with(&root, options()) {
        return true;
    }
    match expanded.strip_suffix("/**") {
        Some(prefix) => Pattern::new(prefix).is_ok_and(|p| p.matches_with(&root, options())),
        None => false,
    }
}

/// Explain why a profile claims a repo: which rule fired.
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

/// The first rule on `profile` that claims `repo`, if any.
pub fn claim(profile: &Profile, repo: &Repo) -> Option<Reason> {
    if let Some(url) = &repo.remote
        && let Some(p) = profile.remotes.iter().find(|p| remote_matches(p, url))
    {
        return Some(Reason::Remote(p.clone()));
    }
    profile
        .paths
        .iter()
        .find(|p| path_matches(p, &repo.root))
        .map(|p| Reason::Path(p.clone()))
}

/// The profile this repository should be on, and the rule that says so.
/// Profiles are tried in config order; the first claim wins.
pub fn expected<'a>(config: &'a Config, repo: &Repo) -> Option<(&'a Profile, Reason)> {
    config
        .profiles
        .iter()
        .find_map(|p| claim(p, repo).map(|why| (p, why)))
}

/// Check that every rule is a well-formed glob. Used by the form so a typo
/// is reported when saving rather than silently never matching.
pub fn validate(patterns: &[String]) -> Result<(), String> {
    for p in patterns {
        Pattern::new(p).map_err(|e| format!("Bad pattern '{p}': {}", e.msg))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        let root = PathBuf::from("/work/acme/api");
        assert!(path_matches("/work/acme/**", &root));
        assert!(path_matches("/work/acme/*", &root));
        assert!(path_matches("/work/acme/api", &root));
        assert!(path_matches("/work/acme/api/**", &root));
        assert!(!path_matches("/work/other/**", &root));
        assert!(!path_matches("/work/*", &root));
    }

    #[test]
    fn tilde_in_path_pattern_is_home() {
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        let root = home.join("work/acme/api");
        assert!(path_matches("~/work/acme/**", &root));
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
    fn validate_rejects_bad_globs() {
        assert!(validate(&["github.com/acme/*".into()]).is_ok());
        assert!(validate(&["[unclosed".into()]).is_err());
    }
}
