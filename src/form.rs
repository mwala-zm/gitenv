//! The profile editor form. Plain data plus a few methods; the UI draws it
//! and `App` decides what to do when it is submitted.

use std::path::PathBuf;

use crate::git::git_bool;
use crate::profile::{Profile, SigningFormat};
use crate::rules;

pub const LABELS: [&str; 9] = [
    "Profile name",
    "Git user.name",
    "Git user.email",
    "Signing key (optional)",
    "Signing format (gpg/ssh/x509)",
    "Sign commits (yes/no)",
    "SSH key path (optional)",
    "Remote rules (comma-separated)",
    "Path rules (comma-separated)",
];

/// Width of the widest label plus a gap, for the UI to line values up.
pub const LABEL_WIDTH: usize = 32;

#[derive(Debug, Clone, Default)]
pub struct Form {
    pub values: [String; 9],
    /// Which field has the cursor.
    pub field: usize,
    /// `Some(index)` when editing an existing profile, `None` for a new one.
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
            ],
            field: 0,
            editing: Some(index),
            error: None,
        }
    }

    pub fn title(&self) -> &'static str {
        if self.editing.is_some() {
            " Edit profile "
        } else {
            " New profile "
        }
    }

    pub fn next_field(&mut self) {
        self.field = (self.field + 1) % LABELS.len();
    }

    pub fn prev_field(&mut self) {
        self.field = (self.field + LABELS.len() - 1) % LABELS.len();
    }

    pub fn insert(&mut self, c: char) {
        if !c.is_control() {
            self.values[self.field].push(c);
            self.error = None;
        }
    }

    pub fn backspace(&mut self) {
        self.values[self.field].pop();
    }

    /// Validate and turn the form into a profile. Uniqueness of the name
    /// is checked by `App`, which knows the other profiles.
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
        rules::validate(&paths)?;
        let signing_format = match optional(signing_format) {
            Some(s) => Some(
                SigningFormat::parse(&s)
                    .ok_or_else(|| format!("Signing format must be gpg, ssh or x509, not '{s}'"))?,
            ),
            None => None,
        };
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
            remotes,
            paths,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let form = Form::edit(0, &profile);
        assert_eq!(form.values[4], "ssh");
        assert_eq!(form.values[5], "yes");
        assert_eq!(form.values[7], "github.com/acme/*, gitlab.com/acme/*");
        assert_eq!(form.to_profile().unwrap(), profile);
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
}
