use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::git::{self, git_bool};
use crate::profile::{Profile, SigningFormat};
use crate::rules;

pub const LABELS: [&str; 10] = [
    "Profile name",
    "Git user.name",
    "Git user.email",
    "Signing key (optional)",
    "Signing format (gpg/ssh/x509)",
    "Sign commits (yes/no)",
    "SSH key path (optional)",
    "Remote rules (comma-separated)",
    "Path rules (comma-separated)",
    "Extra config (k=v, comma-sep.)",
];

pub const LABEL_WIDTH: usize = 32;

#[derive(Debug, Clone, Default)]
pub struct Form {
    pub values: [String; 10],
    pub field: usize,
    pub cursor: usize,
    pub editing: Option<usize>,
    pub error: Option<String>,
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn edit(index: usize, profile: &Profile) -> Self {
        Self {
            values: [
                profile.name.clone(),
                profile.user_name.clone(),
                profile.email.clone(),
                profile.signing_key.clone().unwrap_or_default(),
                profile
                    .signing_format
                    .map(|f| f.as_git().to_string())
                    .unwrap_or_default(),
                profile
                    .sign_commits
                    .map(|s| if s { "yes" } else { "no" }.to_string())
                    .unwrap_or_default(),
                profile
                    .ssh_key
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                profile.remotes.join(", "),
                profile.paths.join(", "),
                profile
                    .git
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ],
            field: 0,
            cursor: profile.name.chars().count(),
            editing: Some(index),
            error: None,
        }
    }

    fn len(&self) -> usize {
        self.values[self.field].chars().count()
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.values[self.field]
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.values[self.field].len())
    }

    fn goto_field(&mut self, field: usize) {
        self.field = field;
        self.cursor = self.len();
    }

    pub fn title(&self) -> &'static str {
        if self.editing.is_some() {
            " Edit profile "
        } else {
            " New profile "
        }
    }

    pub fn next_field(&mut self) {
        self.goto_field((self.field + 1) % LABELS.len());
    }

    pub fn prev_field(&mut self) {
        self.goto_field((self.field + LABELS.len() - 1) % LABELS.len());
    }

    pub fn insert(&mut self, c: char) {
        if !c.is_control() {
            let at = self.byte_at(self.cursor);
            self.values[self.field].insert(at, c);
            self.cursor += 1;
            self.error = None;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.delete();
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let at = self.byte_at(self.cursor);
        self.values[self.field].remove(at);
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len();
    }

    pub fn kill_to_start(&mut self) {
        let at = self.byte_at(self.cursor);
        self.values[self.field].drain(..at);
        self.cursor = 0;
    }

    pub fn kill_to_end(&mut self) {
        let at = self.byte_at(self.cursor);
        self.values[self.field].truncate(at);
    }

    pub fn kill_word(&mut self) {
        let chars: Vec<char> = self.values[self.field].chars().collect();
        let mut start = self.cursor;
        while start > 0 && is_boundary(chars[start - 1]) {
            start -= 1;
        }
        while start > 0 && !is_boundary(chars[start - 1]) {
            start -= 1;
        }
        let (from, to) = (self.byte_at(start), self.byte_at(self.cursor));
        self.values[self.field].drain(from..to);
        self.cursor = start;
    }

    /// Uniqueness of the name is checked by `App`, which knows the other
    /// profiles.
    pub fn to_profile(&self) -> Result<Profile, String> {
        let [
            name,
            user_name,
            email,
            signing_key,
            signing_format,
            sign_commits,
            ssh_key,
            remotes,
            paths,
            extra,
        ] = self.values.each_ref().map(|v| v.trim().to_string());
        if name.is_empty() {
            return Err("Profile name is required".into());
        }
        if user_name.is_empty() {
            return Err("Git user.name is required".into());
        }
        if email.is_empty() || !email.contains('@') {
            return Err("Git user.email must look like an email address".into());
        }
        let optional = |s: String| if s.is_empty() { None } else { Some(s) };
        let list = |s: &str| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        };
        let remotes = list(&remotes);
        let paths = list(&paths);
        rules::validate(&remotes)?;
        rules::validate_paths(&paths)?;
        let signing_format = match optional(signing_format) {
            Some(s) => Some(
                SigningFormat::parse(&s)
                    .ok_or_else(|| format!("Signing format must be gpg, ssh or x509, not '{s}'"))?,
            ),
            None => None,
        };
        let mut git = BTreeMap::new();
        for pair in split_pairs(&extra) {
            let Some((k, v)) = pair.split_once('=') else {
                return Err(format!("Extra git config needs key=value, not '{pair}'"));
            };
            let k = k.trim();
            if !git::valid_key(k) {
                return Err(format!("'{k}' is not a git config key like section.name"));
            }
            git.insert(k.to_string(), v.trim().to_string());
        }
        let sign_commits = match optional(sign_commits) {
            Some(s) => Some(
                git_bool(&s).ok_or_else(|| format!("Sign commits must be yes or no, not '{s}'"))?,
            ),
            None => None,
        };
        Ok(Profile {
            name,
            user_name,
            email,
            signing_key: optional(signing_key),
            ssh_key: optional(ssh_key).map(PathBuf::from),
            signing_format,
            sign_commits,
            git,
            remotes,
            paths,
        })
    }
}

/// Splits `k=v, k2=v2` only at commas that start another `key=`, so a value may
/// itself contain commas, as in `alias.lg=log --format=%h,%s`.
fn split_pairs(s: &str) -> Vec<String> {
    let mut pairs = Vec::new();
    let mut start = 0;
    for (i, _) in s.match_indices(',') {
        let starts_pair = s[i + 1..]
            .split_once('=')
            .is_some_and(|(key, _)| git::valid_key(key.trim()));
        if starts_pair {
            pairs.push(s[start..i].trim().to_string());
            start = i + 1;
        }
    }
    pairs.push(s[start..].trim().to_string());
    pairs.retain(|p| !p.is_empty());
    pairs
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | ',' | '=' | '@' | ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Form {
        let mut form = Form::new();
        for c in s.chars() {
            form.insert(c);
        }
        form
    }

    #[test]
    fn cursor_moves_and_edits_in_the_middle() {
        let mut form = typed("abd");
        form.left();
        form.insert('c');
        assert_eq!(form.values[0], "abcd");
        assert_eq!(form.cursor, 3);
        form.home();
        form.delete();
        assert_eq!(form.values[0], "bcd");
        form.end();
        form.backspace();
        assert_eq!(form.values[0], "bc");
        form.right();
        form.left();
        form.left();
        form.left();
        assert_eq!(form.cursor, 0, "left stops at the start");
        form.backspace();
        assert_eq!(form.values[0], "bc", "backspace at the start is a no-op");
    }

    #[test]
    fn cursor_handles_multibyte_characters() {
        let mut form = typed("héllo");
        form.left();
        form.left();
        form.left();
        form.backspace();
        assert_eq!(form.values[0], "hllo");
        form.insert('ë');
        assert_eq!(form.values[0], "hëllo");
    }

    #[test]
    fn kill_shortcuts() {
        let mut form = typed("/Users/ada/.ssh/id_acme");
        form.kill_word();
        assert_eq!(form.values[0], "/Users/ada/.ssh/");
        form.kill_word();
        assert_eq!(form.values[0], "/Users/ada/");
        form.left();
        form.left();
        form.kill_to_end();
        assert_eq!(form.values[0], "/Users/ad");
        form.kill_to_start();
        assert_eq!(form.values[0], "");
        assert_eq!(form.cursor, 0);
    }

    #[test]
    fn switching_fields_puts_the_cursor_at_the_end() {
        let mut form = typed("abc");
        form.home();
        form.next_field();
        form.insert('x');
        form.prev_field();
        assert_eq!(form.cursor, 3);
        assert_eq!(form.values[1], "x");
    }

    #[test]
    fn empty_optionals_become_none() {
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[1] = "Ada".into();
        form.values[2] = "ada@acme.example".into();
        let profile = form.to_profile().unwrap();
        assert!(profile.signing_key.is_none());
        assert!(profile.ssh_key.is_none());
    }

    #[test]
    fn rejects_missing_email() {
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[1] = "Ada".into();
        assert!(form.to_profile().is_err());
    }

    #[test]
    fn edit_round_trips() {
        let profile = Profile {
            name: "acme".into(),
            user_name: "Ada".into(),
            email: "ada@acme.example".into(),
            signing_key: Some("KEY".into()),
            ssh_key: Some("/tmp/id".into()),
            signing_format: Some(SigningFormat::Ssh),
            sign_commits: Some(true),
            remotes: vec!["github.com/acme/*".into(), "gitlab.com/acme/*".into()],
            paths: vec!["~/work/acme/**".into()],
            git: BTreeMap::from([
                ("pull.rebase".to_string(), "true".to_string()),
                (
                    "url.git@github.com:acme/.insteadOf".to_string(),
                    "https://github.com/acme/".to_string(),
                ),
            ]),
        };
        let form = Form::edit(0, &profile);
        assert_eq!(
            form.values[9],
            "pull.rebase=true, url.git@github.com:acme/.insteadOf=https://github.com/acme/"
        );
        assert_eq!(form.values[4], "ssh");
        assert_eq!(form.values[5], "yes");
        assert_eq!(form.values[7], "github.com/acme/*, gitlab.com/acme/*");
        assert_eq!(form.to_profile().unwrap(), profile);
    }

    #[test]
    fn bad_extra_config_is_reported() {
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[1] = "Ada".into();
        form.values[2] = "ada@acme.example".into();
        form.values[9] = "pull.rebase".into();
        assert!(form.to_profile().unwrap_err().contains("key=value"));
        form.values[9] = "rebase=true".into();
        assert!(form.to_profile().unwrap_err().contains("section.name"));
    }

    #[test]
    fn bad_signing_values_are_reported() {
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[1] = "Ada".into();
        form.values[2] = "ada@acme.example".into();
        form.values[4] = "pigeon".into();
        assert!(form.to_profile().unwrap_err().contains("Signing format"));
        form.values[4] = "ssh".into();
        form.values[5] = "maybe".into();
        assert!(form.to_profile().unwrap_err().contains("Sign commits"));
        form.values[5] = "no".into();
        assert_eq!(form.to_profile().unwrap().sign_commits, Some(false));
    }

    #[test]
    fn bad_rule_is_reported() {
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[1] = "Ada".into();
        form.values[2] = "ada@acme.example".into();
        form.values[7] = "[oops".into();
        assert!(form.to_profile().unwrap_err().contains("Bad pattern"));
    }

    #[test]
    fn extra_config_values_may_contain_commas() {
        let mut form = typed("p");
        form.values[1] = "Ada".into();
        form.values[2] = "ada@example.com".into();
        form.values[9] = "alias.lg=log --format=%h,%s, pull.rebase=true".into();
        let profile = form.to_profile().unwrap();
        assert_eq!(profile.git["alias.lg"], "log --format=%h,%s");
        assert_eq!(profile.git["pull.rebase"], "true");
        let again = Form::edit(0, &profile).to_profile().unwrap();
        assert_eq!(again, profile);
    }
}
