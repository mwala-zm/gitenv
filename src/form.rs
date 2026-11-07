use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::git::{self, Repo, git_bool};
use crate::profile::{Config, Profile, SigningFormat};
use crate::rules;

pub const LABELS: [&str; 11] = [
    "Profile name",
    "Git user.name",
    "Git user.email",
    "Signing key (optional)",
    "Signing format (gpg/ssh/x509)",
    "Sign commits (yes/no)",
    "SSH key path (optional)",
    "GitHub CLI account (optional)",
    "Remote rules (comma-separated)",
    "Path rules (comma-separated)",
    "Extra config (k=v, comma-sep.)",
];

pub const LABEL_WIDTH: usize = 32;

#[derive(Debug, Clone, Default)]
pub struct Form {
    pub values: [String; 11],
    pub field: usize,
    pub cursor: usize,
    pub editing: Option<usize>,
    pub error: Option<String>,
    pub picker: Option<Picker>,
}

/// Values to choose from for one field: what goes in, and what is shown.
#[derive(Debug, Clone, Default)]
pub struct Picker {
    pub items: Vec<(String, String)>,
    pub selected: usize,
}

/// What the rules being typed would do to the repositories gitenv knows of:
/// those they would claim, and those a more specific rule of another
/// profile, or an equally specific one earlier in the file, keeps, with that
/// profile's name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    pub claimed: Vec<PathBuf>,
    pub lost: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Good(String),
    Bad(String),
}

pub const SIGNING_KEY: usize = 3;
pub const SIGNING_FORMAT: usize = 4;
pub const SIGN_COMMITS: usize = 5;
pub const SSH_KEY: usize = 6;
pub const GH_ACCOUNT: usize = 7;
pub const REMOTES: usize = 8;
pub const PATHS: usize = 9;

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
                profile.gh_account.clone().unwrap_or_default(),
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
            picker: None,
        }
    }

    /// A new profile taken from what git uses in `repo`: its name, email and
    /// signing settings, the ssh key an `ssh -i` core.sshCommand offers, and a
    /// remote rule for every repository of the same owner, or a path rule for
    /// this folder when it has no remote. It is named after the owner, or the
    /// folder.
    pub fn from_repo(repo: &Repo) -> Self {
        let id = &repo.effective;
        let host_path = repo.remote.as_deref().map(rules::remote_host_path);
        let owner = host_path
            .as_deref()
            .and_then(|hp| hp.split('/').nth(1))
            .filter(|o| !o.is_empty())
            .map(str::to_ascii_lowercase);
        let folder = repo
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let rule = match (&host_path, &owner) {
            (Some(hp), Some(owner)) => {
                let host = hp.split('/').next().unwrap_or_default();
                (format!("{host}/{owner}/*"), String::new())
            }
            _ => (String::new(), crate::scan::tildify(&repo.root)),
        };
        let name = owner.unwrap_or(folder);
        let mut values: [String; 11] = Default::default();
        values[0] = name;
        values[1] = id.name.clone().unwrap_or_default();
        values[2] = id.email.clone().unwrap_or_default();
        values[3] = id.signing_key.clone().unwrap_or_default();
        values[4] = id
            .signing_format
            .map(|f| f.as_git().to_string())
            .unwrap_or_default();
        values[5] = id
            .sign_commits
            .map(|s| if s { "yes" } else { "no" }.to_string())
            .unwrap_or_default();
        values[6] = id
            .ssh_command
            .as_deref()
            .and_then(ssh_key_of)
            .map(|k| crate::scan::tildify(Path::new(&k)))
            .unwrap_or_default();
        values[8] = rule.0;
        values[9] = rule.1;
        Self {
            cursor: values[0].chars().count(),
            values,
            ..Self::default()
        }
    }

    fn format(&self) -> Option<SigningFormat> {
        SigningFormat::parse(self.values[SIGNING_FORMAT].trim())
    }

    /// What can be said about field `field` as typed so far: a key file that
    /// is missing or public, a value the field does not take, a rule that
    /// does not parse. Saving reports the same, but this shows it at once.
    pub fn check(&self, field: usize) -> Option<Check> {
        let value = self.values[field].trim();
        let bad = |s: &str| Some(Check::Bad(s.to_string()));
        match field {
            2 if !value.is_empty() && !value.contains('@') => bad("an email address needs an @"),
            SIGNING_KEY if !value.is_empty() && self.format() == Some(SigningFormat::Ssh) => {
                if value.starts_with("key::") || value.starts_with("ssh-") {
                    Some(Check::Good("an inline ssh key".into()))
                } else {
                    key_file(value)
                }
            }
            SIGNING_FORMAT if !value.is_empty() && self.format().is_none() => {
                bad("gpg, ssh or x509")
            }
            SIGN_COMMITS if !value.is_empty() => match git_bool(value) {
                None => bad("yes or no"),
                Some(true) if self.values[SIGNING_KEY].trim().is_empty() => {
                    bad("signing needs a signing key")
                }
                Some(_) => None,
            },
            SSH_KEY if !value.is_empty() => {
                if value.ends_with(".pub") {
                    return bad("that is the public key; give the private one");
                }
                key_file(value)
            }
            REMOTES | PATHS if !value.is_empty() => {
                let checked = if field == REMOTES {
                    rules::validate(&list(value))
                } else {
                    rules::validate_paths(&split_paths(value))
                };
                checked.err().map(Check::Bad)
            }
            10 => split_pairs(value)
                .into_iter()
                .find_map(|pair| match pair.split_once('=') {
                    None => Some(Check::Bad(format!("'{pair}' needs key=value"))),
                    Some((k, _)) if !git::valid_key(k.trim()) => Some(Check::Bad(format!(
                        "'{}' is not a key like section.name",
                        k.trim()
                    ))),
                    Some(_) => None,
                }),
            _ => None,
        }
    }

    /// The profile as typed so far, kept to the rules that parse, standing
    /// in where it will be saved: over the profile being edited, or last.
    /// Each of `repos` it matches is claimed, or lost to whichever rule of
    /// another profile is more specific.
    pub fn preview(&self, config: &Config, repos: &[Repo]) -> Preview {
        let profile = Profile {
            name: self.values[0].trim().to_string(),
            remotes: list(&self.values[REMOTES])
                .into_iter()
                .filter(|r| rules::validate(std::slice::from_ref(r)).is_ok())
                .collect(),
            paths: split_paths(&self.values[PATHS])
                .into_iter()
                .filter(|p| !p.is_empty() && rules::validate_paths(std::slice::from_ref(p)).is_ok())
                .collect(),
            ..Default::default()
        };
        let mut config = config.clone();
        let at = match self.editing {
            Some(i) if i < config.profiles.len() => {
                config.profiles[i] = profile;
                i
            }
            _ => {
                config.profiles.push(profile);
                config.profiles.len() - 1
            }
        };
        let ours = &config.profiles[at];
        let mut preview = Preview::default();
        for repo in repos {
            let found = rules::matching(&config, repo);
            match found.iter().position(|r| std::ptr::eq(r.profile, ours)) {
                Some(0) => preview.claimed.push(repo.root.clone()),
                Some(_) => preview
                    .lost
                    .push((repo.root.clone(), found[0].profile.name.clone())),
                None => {}
            }
        }
        preview
    }

    /// Whether Ctrl-O lists values for the focused field.
    pub fn pickable(&self) -> bool {
        matches!(
            self.field,
            SIGNING_KEY | SIGNING_FORMAT | SIGN_COMMITS | SSH_KEY | GH_ACCOUNT
        )
    }

    /// The values the focused field can take, looked up in `ssh_dir`, gpg's
    /// keyring and gh's logins as the field needs.
    pub fn candidates(&self, ssh_dir: &Path) -> Vec<(String, String)> {
        let same = |v: &str| (v.to_string(), v.to_string());
        match self.field {
            SIGNING_KEY if self.format() == Some(SigningFormat::Ssh) => ssh_keys(ssh_dir, true),
            SIGNING_KEY => gpg_secret_keys(),
            SIGNING_FORMAT => ["gpg", "ssh", "x509"].map(same).to_vec(),
            SIGN_COMMITS => ["yes", "no"].map(same).to_vec(),
            SSH_KEY => ssh_keys(ssh_dir, false),
            GH_ACCOUNT => crate::gh::accounts().iter().map(|a| same(a)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn open_picker(&mut self, items: Vec<(String, String)>) {
        let current = self.values[self.field].trim();
        let selected = items.iter().position(|(v, _)| v == current).unwrap_or(0);
        self.picker = Some(Picker { items, selected });
    }

    /// Puts the picked value in the field and closes the picker.
    pub fn pick(&mut self) {
        if let Some(picker) = self.picker.take()
            && let Some((value, _)) = picker.items.get(picker.selected)
        {
            self.values[self.field] = value.clone();
            self.cursor = self.len();
            self.error = None;
        }
    }

    pub fn move_picker(&mut self, delta: isize) {
        if let Some(p) = &mut self.picker
            && !p.items.is_empty()
        {
            p.selected = (p.selected as isize + delta).rem_euclid(p.items.len() as isize) as usize;
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
            gh_account,
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
        let remotes = list(&remotes);
        let paths = split_paths(&paths);
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
            gh_account: optional(gh_account),
            remotes,
            paths,
        })
    }
}

fn list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

fn key_file(value: &str) -> Option<Check> {
    let path = git::expand_tilde(Path::new(value));
    Some(if path.is_file() {
        Check::Good(format!("{} exists", path.display()))
    } else {
        Check::Bad(format!("no such file: {}", path.display()))
    })
}

/// Keys in `dir`, as `~` paths: the private ones, which have a `.pub` next
/// to them, or with `public` those `.pub` files themselves.
fn ssh_keys(dir: &Path, public: bool) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys: Vec<(String, String)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let is_pub = p.extension().is_some_and(|e| e == "pub");
            if public {
                is_pub
            } else {
                !is_pub
                    && p.with_extension(match p.extension() {
                        Some(e) => format!("{}.pub", e.to_string_lossy()),
                        None => "pub".into(),
                    })
                    .is_file()
            }
        })
        .map(|p| {
            let shown = crate::scan::tildify(&p);
            (shown.clone(), shown)
        })
        .collect();
    keys.sort();
    keys
}

fn gpg_secret_keys() -> Vec<(String, String)> {
    std::process::Command::new("gpg")
        .args(["--list-secret-keys", "--with-colons"])
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| parse_gpg(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// The secret keys in `gpg --with-colons` output, as their key ID and the
/// first user ID that follows.
fn parse_gpg(text: &str) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut waiting = false;
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.first() {
            Some(&"sec") if fields.len() > 4 => {
                keys.push((fields[4].to_string(), fields[4].to_string()));
                waiting = true;
            }
            Some(&"uid") if waiting && fields.len() > 9 => {
                if let Some(last) = keys.last_mut() {
                    last.1 = format!("{}  {}", last.0, fields[9]);
                }
                waiting = false;
            }
            _ => {}
        }
    }
    keys
}

/// The key an `ssh -i KEY ...` command offers, unquoting it as a shell
/// would, so the command gitenv writes for a profile reads back as its key.
fn ssh_key_of(command: &str) -> Option<String> {
    let words = shell_words(command);
    let mut words = words.iter();
    if !words
        .next()
        .is_some_and(|w| w == "ssh" || w.ends_with("/ssh"))
    {
        return None;
    }
    while let Some(word) = words.next() {
        if word == "-i" {
            return words.next().cloned();
        }
        if let Some(key) = word.strip_prefix("-i").filter(|k| !k.is_empty()) {
            return Some(key.to_string());
        }
    }
    None
}

fn shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(w) = word.take() {
                    words.push(w);
                }
            }
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    w.push(q);
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => w.extend(chars.next()),
                        _ => w.push(q),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            _ => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    words
}

fn split_paths(s: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut start = 0;
    for (i, _) in s.match_indices(',') {
        if s[i + 1..].trim_start().starts_with(['/', '~']) {
            paths.push(s[start..i].trim().to_string());
            start = i + 1;
        }
    }
    paths.push(s[start..].trim().to_string());
    paths.retain(|p| !p.is_empty());
    paths
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
            gh_account: Some("ada-acme".into()),
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
            form.values[10],
            "pull.rebase=true, url.git@github.com:acme/.insteadOf=https://github.com/acme/"
        );
        assert_eq!(form.values[4], "ssh");
        assert_eq!(form.values[5], "yes");
        assert_eq!(form.values[7], "ada-acme");
        assert_eq!(form.values[8], "github.com/acme/*, gitlab.com/acme/*");
        assert_eq!(form.to_profile().unwrap(), profile);
    }

    #[test]
    fn bad_extra_config_is_reported() {
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[1] = "Ada".into();
        form.values[2] = "ada@acme.example".into();
        form.values[10] = "pull.rebase".into();
        assert!(form.to_profile().unwrap_err().contains("key=value"));
        form.values[10] = "rebase=true".into();
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
        form.values[8] = "[oops".into();
        assert!(form.to_profile().unwrap_err().contains("Bad pattern"));
    }

    #[test]
    fn extra_config_values_may_contain_commas() {
        let mut form = typed("p");
        form.values[1] = "Ada".into();
        form.values[2] = "ada@example.com".into();
        form.values[10] = "alias.lg=log --format=%h,%s, pull.rebase=true".into();
        let profile = form.to_profile().unwrap();
        assert_eq!(profile.git["alias.lg"], "log --format=%h,%s");
        assert_eq!(profile.git["pull.rebase"], "true");
        let again = Form::edit(0, &profile).to_profile().unwrap();
        assert_eq!(again, profile);
    }

    #[test]
    fn path_rules_may_contain_commas() {
        assert_eq!(
            split_paths("~/Clients/Smith, Jones/**, /srv/x"),
            vec!["~/Clients/Smith, Jones/**", "/srv/x"]
        );
    }

    #[test]
    fn the_ssh_key_reads_back_from_the_command_gitenv_writes() {
        let key = Path::new("/home/ada/.ssh/id it's");
        assert_eq!(
            ssh_key_of(&git::ssh_command(key)).as_deref(),
            Some("/home/ada/.ssh/id it's")
        );
        assert_eq!(
            ssh_key_of("ssh -o IdentitiesOnly=yes -i \"/k/a b\"").as_deref(),
            Some("/k/a b")
        );
        assert_eq!(ssh_key_of("/usr/bin/ssh -i/k/x").as_deref(), Some("/k/x"));
        assert_eq!(ssh_key_of("ssh -o IdentitiesOnly=yes"), None);
        assert_eq!(ssh_key_of("plink -i /k/x"), None);
    }

    fn repo(root: &str, remote: Option<&str>) -> Repo {
        Repo {
            root: root.into(),
            remote: remote.map(String::from),
            effective: git::Identity {
                name: Some("Ada".into()),
                email: Some("ada@acme.example".into()),
                signing_key: Some("~/.ssh/id_acme.pub".into()),
                signing_format: Some(SigningFormat::Ssh),
                sign_commits: Some(true),
                ssh_command: Some(git::ssh_command(Path::new("/keys/id_acme"))),
                ..Default::default()
            },
            local: Default::default(),
            authors: Vec::new(),
        }
    }

    #[test]
    fn a_profile_from_a_repo_takes_its_identity_and_owner() {
        let form = Form::from_repo(&repo("/work/acme/api", Some("git@github.com:Acme/api.git")));
        let p = form.to_profile().unwrap();
        assert_eq!(p.name, "acme");
        assert_eq!(
            (p.user_name.as_str(), p.email.as_str()),
            ("Ada", "ada@acme.example")
        );
        assert_eq!(p.signing_key.as_deref(), Some("~/.ssh/id_acme.pub"));
        assert_eq!(p.signing_format, Some(SigningFormat::Ssh));
        assert_eq!(p.sign_commits, Some(true));
        assert_eq!(p.ssh_key, Some(PathBuf::from("/keys/id_acme")));
        assert_eq!(p.remotes, ["github.com/acme/*"]);
        assert!(p.paths.is_empty());
        assert_eq!(form.editing, None);
    }

    #[test]
    fn without_a_remote_the_folder_names_it_and_is_its_rule() {
        let form = Form::from_repo(&repo("/work/scratch", None));
        assert_eq!(form.values[0], "scratch");
        assert_eq!(form.values[8], "");
        assert_eq!(form.values[9], "/work/scratch");
    }

    #[test]
    fn checks_flag_what_saving_would_refuse_or_regret() {
        let mut form = Form::new();
        form.values[2] = "ada.example.com".into();
        assert!(matches!(form.check(2), Some(Check::Bad(_))));
        form.values[SSH_KEY] = "/nonexistent/id_x".into();
        assert!(
            matches!(form.check(SSH_KEY), Some(Check::Bad(m)) if m.starts_with("no such file"))
        );
        form.values[SSH_KEY] = "~/.ssh/id_x.pub".into();
        assert!(matches!(form.check(SSH_KEY), Some(Check::Bad(m)) if m.contains("public key")));
        let exe = std::env::current_exe().unwrap().display().to_string();
        form.values[SSH_KEY] = exe;
        assert!(matches!(form.check(SSH_KEY), Some(Check::Good(_))));
        form.values[SIGN_COMMITS] = "yes".into();
        assert_eq!(
            form.check(SIGN_COMMITS),
            Some(Check::Bad("signing needs a signing key".into()))
        );
        form.values[SIGNING_FORMAT] = "rsa".into();
        assert!(matches!(form.check(SIGNING_FORMAT), Some(Check::Bad(_))));
        form.values[PATHS] = "work/**".into();
        assert!(matches!(form.check(PATHS), Some(Check::Bad(m)) if m.contains("must start with")));
        form.values[10] = "nonsense, pull.rebase=true".into();
        assert!(matches!(form.check(10), Some(Check::Bad(m)) if m.contains("'nonsense'")));
        form.values[REMOTES] = "github.com/acme/*".into();
        assert_eq!(form.check(REMOTES), None);
    }

    #[test]
    fn ssh_keys_lists_private_keys_with_a_public_half() {
        let dir = std::env::temp_dir().join(format!("gitenv-form-ssh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "id_acme",
            "id_acme.pub",
            "id_old.key",
            "id_old.key.pub",
            "config",
            "known_hosts",
        ] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let names = |keys: Vec<(String, String)>| -> Vec<String> {
            keys.into_iter()
                .map(|(v, _)| {
                    Path::new(&v)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(names(ssh_keys(&dir, false)), ["id_acme", "id_old.key"]);
        assert_eq!(
            names(ssh_keys(&dir, true)),
            ["id_acme.pub", "id_old.key.pub"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gpg_keys_come_with_their_user_id() {
        let text = "sec:u:255:22:ABCD1234EF567890:1700000000:::u:::scESC:::+:::23::0:\n\
                    fpr:::::::::0123456789ABCDEF0123ABCD1234EF567890:\n\
                    uid:u::::1700000000::HASH::Ada Lovelace <ada@acme.example>::::::::::0:\n\
                    ssb:u:255:18:1111222233334444:1700000000::::::e:::+:::23:\n\
                    sec:u:255:22:9999888877776666:1700000000:::u:::scESC:::+:::23::0:\n";
        assert_eq!(
            parse_gpg(text),
            [
                (
                    "ABCD1234EF567890".to_string(),
                    "ABCD1234EF567890  Ada Lovelace <ada@acme.example>".to_string()
                ),
                (
                    "9999888877776666".to_string(),
                    "9999888877776666".to_string()
                ),
            ]
        );
    }

    #[test]
    fn picking_fills_the_field() {
        let mut form = Form::new();
        form.field = SIGNING_FORMAT;
        assert!(form.pickable());
        form.open_picker(form.candidates(Path::new("/nonexistent")));
        form.move_picker(1);
        form.move_picker(1);
        form.move_picker(1);
        form.pick();
        assert_eq!(form.values[SIGNING_FORMAT], "gpg");
        assert!(form.picker.is_none());
        form.field = 0;
        assert!(!form.pickable());
    }

    #[test]
    fn the_preview_shows_what_the_typed_rules_would_claim_and_lose() {
        let infra = Profile {
            name: "infra".into(),
            remotes: vec!["github.com/acme/infra*".into()],
            ..Default::default()
        };
        let config = Config {
            profiles: vec![infra],
        };
        let repos: Vec<Repo> = ["api", "web", "infra-tools"]
            .iter()
            .map(|name| {
                let mut r = repo(
                    &format!("/work/acme/{name}"),
                    Some(&format!("git@github.com:acme/{name}.git")),
                );
                r.effective = Default::default();
                r
            })
            .chain([repo("/work/toy", Some("git@github.com:ada/toy.git"))])
            .collect();
        let mut form = Form::new();
        form.values[0] = "acme".into();
        form.values[REMOTES] = "github.com/acme/*, github.com/[broken".into();
        let p = form.preview(&config, &repos);
        assert_eq!(
            p.claimed,
            [
                PathBuf::from("/work/acme/api"),
                PathBuf::from("/work/acme/web")
            ]
        );
        assert_eq!(
            p.lost,
            [(PathBuf::from("/work/acme/infra-tools"), "infra".to_string())]
        );

        let mut edit = Form::edit(0, &config.profiles[0]);
        edit.values[REMOTES] = "github.com/ada/*".into();
        let p = edit.preview(&config, &repos);
        assert_eq!(p.claimed, [PathBuf::from("/work/toy")]);
        assert!(
            p.lost.is_empty(),
            "editing replaces the profile's own old rules"
        );
    }
}
