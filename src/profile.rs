use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

pub const CONFIG_ENV: &str = "GITENV_CONFIG";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigningFormat {
    Openpgp,
    Ssh,
    X509,
}

impl SigningFormat {
    pub fn as_git(self) -> &'static str {
        match self {
            SigningFormat::Openpgp => "openpgp",
            SigningFormat::Ssh => "ssh",
            SigningFormat::X509 => "x509",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openpgp" | "gpg" | "pgp" => Some(SigningFormat::Openpgp),
            "ssh" => Some(SigningFormat::Ssh),
            "x509" | "x.509" | "smime" => Some(SigningFormat::X509),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub user_name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key: Option<PathBuf>,
    /// With `ssh`, `signing_key` is the path to a public key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_format: Option<SigningFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_commits: Option<bool>,
    /// Checked when matching, but never removed by applying another profile.
    #[serde(
        default,
        deserialize_with = "git_settings",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub git: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// `[profiles.git]` keys may be written as TOML dotted keys
/// (`pull.rebase = true`), which arrive as nested tables; join them back
/// into git's `section.key` and accept plain booleans and numbers.
fn git_settings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    fn flatten(
        prefix: &str,
        table: toml::Table,
        out: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        for (key, value) in table {
            let key = if prefix.is_empty() {
                key
            } else {
                format!("{prefix}.{key}")
            };
            let text = match value {
                toml::Value::Table(t) => {
                    flatten(&key, t, out)?;
                    continue;
                }
                toml::Value::String(s) => s,
                toml::Value::Boolean(b) => b.to_string(),
                toml::Value::Integer(i) => i.to_string(),
                other => return Err(format!("git setting '{key}' must be a string, not {other}")),
            };
            out.insert(key, text);
        }
        Ok(())
    }
    let table = toml::Table::deserialize(deserializer)?;
    let mut out = BTreeMap::new();
    flatten("", table, &mut out).map_err(serde::de::Error::custom)?;
    Ok(out)
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Profile {
    /// How many settings beyond name and email the profile fixes.
    fn pinned(&self) -> usize {
        usize::from(self.signing_key.is_some())
            + usize::from(self.ssh_key.is_some())
            + usize::from(self.signing_format.is_some())
            + usize::from(self.sign_commits.is_some())
            + self.git.len()
    }
}

impl Config {
    /// `~/Library/Application Support/gitenv/config.toml` on macOS,
    /// `~/.config/gitenv/config.toml` on Linux, unless `GITENV_CONFIG` is set.
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(path));
        }
        let dirs = ProjectDirs::from("", "", "gitenv")
            .context("could not determine a config directory for this user")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// A missing file is not an error: the UI tells you where to create one.
    pub fn load(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(config)
    }

    /// Edits the existing file in place where it can, so comments and
    /// layout around profiles that did not change survive a save. The file
    /// is replaced atomically, and a symlinked config is written through.
    pub fn save(&self, path: &PathBuf) -> Result<()> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let fresh: DocumentMut = toml::to_string_pretty(self)
            .context("serialising config")?
            .parse()
            .context("re-reading serialised config")?;
        let text = match fs::read_to_string(&path)
            .ok()
            .and_then(|old| old.parse::<DocumentMut>().ok())
        {
            Some(mut doc) => {
                merge_document(&mut doc, &fresh);
                doc.to_string()
            }
            None => fresh.to_string(),
        };
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// The profile the identity is on. When several match, the one that
    /// pins the most settings wins, so a repo set up with a signing profile
    /// is not reported as a plainer profile with the same name and email;
    /// ties go to file order.
    pub fn matching(&self, identity: &crate::git::Identity) -> Option<&Profile> {
        self.profiles
            .iter()
            .rev()
            .filter(|p| identity.matches(p))
            .max_by_key(|p| p.pinned())
    }
}

/// Bring `doc` in line with `fresh`, keeping `doc`'s formatting wherever
/// the content is unchanged. Profiles are paired by name, then a renamed
/// one by position.
fn merge_document(doc: &mut DocumentMut, fresh: &DocumentMut) {
    let Some(new) = fresh.get("profiles").and_then(Item::as_array_of_tables) else {
        doc.remove("profiles");
        return;
    };
    let old = doc
        .get("profiles")
        .and_then(Item::as_array_of_tables)
        .cloned()
        .unwrap_or_default();
    let name = |t: &Table| t.get("name").and_then(Item::as_str).map(str::to_owned);
    let new_names: Vec<Option<String>> = new.iter().map(name).collect();
    let mut merged = ArrayOfTables::new();
    for (i, table) in new.iter().enumerate() {
        let paired = old
            .iter()
            .find(|t| name(t).is_some() && name(t) == new_names[i])
            .or_else(|| old.get(i).filter(|t| !new_names.contains(&name(t))));
        match paired {
            Some(old_table) if same_profile(old_table, table) => merged.push(old_table.clone()),
            Some(old_table) => {
                let mut kept = old_table.clone();
                merge_table(&mut kept, table);
                merged.push(kept);
            }
            None => merged.push(table.clone()),
        }
    }
    doc.insert("profiles", Item::ArrayOfTables(merged));
}

fn merge_table(old: &mut Table, new: &Table) {
    let gone: Vec<String> = old
        .iter()
        .map(|(k, _)| k.to_owned())
        .filter(|k| !new.contains_key(k))
        .collect();
    for key in gone {
        old.remove(&key);
    }
    for (key, item) in new.iter() {
        match (old.get_mut(key), item) {
            (Some(Item::Table(o)), Item::Table(n)) => merge_table(o, n),
            (Some(Item::Value(o)), Item::Value(n)) if same_value(o, n) => {}
            (Some(Item::Value(o)), Item::Value(n)) => {
                let decor = o.decor().clone();
                *o = n.clone();
                *o.decor_mut() = decor;
            }
            _ => {
                old.insert(key, item.clone());
            }
        }
    }
}

/// Equal once read as profiles, so `pull.rebase = true` and
/// `"pull.rebase" = "true"` are the same.
fn same_profile(a: &Table, b: &Table) -> bool {
    let read = |t: &Table| {
        let mut doc = DocumentMut::new();
        doc.insert("p", Item::Table(t.clone()));
        toml::from_str::<BTreeMap<String, Profile>>(&doc.to_string()).ok()
    };
    read(a).is_some() && read(a) == read(b)
}

/// Equal as data, however each side is laid out.
fn same_value(a: &Value, b: &Value) -> bool {
    let data = |v: &Value| {
        let mut v = v.clone();
        v.decor_mut().clear();
        toml::from_str::<toml::Table>(&format!("v = {v}")).ok()
    };
    data(a).is_some() && data(a) == data(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_through_toml() {
        let config = Config {
            profiles: vec![
                Profile {
                    name: "personal".into(),
                    user_name: "Ada Lovelace".into(),
                    email: "ada@example.com".into(),
                    ..Default::default()
                },
                Profile {
                    name: "acme".into(),
                    user_name: "Ada Lovelace".into(),
                    email: "ada@acme.example".into(),
                    signing_key: Some("~/.ssh/id_acme.pub".into()),
                    ssh_key: Some("/home/ada/.ssh/id_acme".into()),
                    signing_format: Some(SigningFormat::Ssh),
                    sign_commits: Some(true),
                    git: BTreeMap::from([(
                        "url.git@github.com:acme/.insteadOf".to_string(),
                        "https://github.com/acme/".to_string(),
                    )]),
                    remotes: vec!["github.com/acme/*".into()],
                    paths: vec!["~/work/acme/**".into()],
                },
            ],
        };
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("signing_format = \"ssh\""), "{text}");
        assert!(text.contains("sign_commits = true"), "{text}");
        assert!(text.contains("[profiles.git]"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn signing_format_parses_what_people_type() {
        assert_eq!(SigningFormat::parse("SSH"), Some(SigningFormat::Ssh));
        assert_eq!(SigningFormat::parse("gpg"), Some(SigningFormat::Openpgp));
        assert_eq!(SigningFormat::parse("x509"), Some(SigningFormat::X509));
        assert_eq!(SigningFormat::parse("pigeon"), None);
    }

    #[test]
    fn missing_optional_fields_are_fine() {
        let text = r#"
            [[profiles]]
            name = "p"
            user_name = "n"
            email = "e"
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.profiles.len(), 1);
        assert!(config.profiles[0].signing_key.is_none());
    }

    fn temp_config(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("gitenv-save-{}-{tag}.toml", std::process::id()))
    }

    #[test]
    fn saving_keeps_comments_and_layout_of_untouched_parts() {
        let path = temp_config("layout");
        let text = r#"# My identities, kept in dotfiles.

# Day job.
[[profiles]]
name = "acme"
user_name = "Ada Lovelace"
email = "ada@acme.example" # the work address
remotes = [
    "github.com/acme/*", # main org
    "gitlab.acme.example/**",
]

# Weekend projects.
[[profiles]]
name = "personal"
user_name = "Ada Lovelace"
email = "ada@example.com"

# Old client, keep for archives.
[[profiles]]
name = "old"
user_name = "Ada Lovelace"
email = "ada@old.example"
"#;
        fs::write(&path, text).unwrap();
        let mut config = Config::load(&path).unwrap();
        config.profiles[0].signing_key = Some("ABCD".into());
        config.profiles[1].name = "home".into();
        config.profiles.remove(2);
        config.save(&path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(
            saved.starts_with("# My identities, kept in dotfiles.\n"),
            "{saved}"
        );
        assert!(saved.contains("# Day job.\n[[profiles]]"), "{saved}");
        assert!(
            saved.contains("email = \"ada@acme.example\" # the work address"),
            "{saved}"
        );
        assert!(
            saved.contains("\"github.com/acme/*\", # main org"),
            "{saved}"
        );
        assert!(saved.contains("signing_key = \"ABCD\""), "{saved}");
        assert!(
            saved.contains("# Weekend projects.\n[[profiles]]\nname = \"home\""),
            "{saved}"
        );
        assert!(!saved.contains("old"), "{saved}");
        assert_eq!(Config::load(&path).unwrap(), config);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn saving_a_new_file_and_through_a_symlink() {
        let path = temp_config("fresh");
        let _ = fs::remove_file(&path);
        let config = Config {
            profiles: vec![Profile {
                name: "p".into(),
                user_name: "n".into(),
                email: "e".into(),
                ..Default::default()
            }],
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);

        // A config symlinked from a dotfiles repo stays a symlink.
        #[cfg(unix)]
        {
            let link = temp_config("link");
            let _ = fs::remove_file(&link);
            std::os::unix::fs::symlink(&path, &link).unwrap();
            let mut changed = config.clone();
            changed.profiles[0].email = "e2".into();
            changed.save(&link).unwrap();
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(Config::load(&path).unwrap(), changed);
            fs::remove_file(&link).unwrap();
        }
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn git_settings_accept_dotted_keys_and_plain_values() {
        let text = r#"
            [[profiles]]
            name = "p"
            user_name = "n"
            email = "e"

            [profiles.git]
            pull.rebase = true
            "core.editor" = "vim"
            url."git@github.com:acme/".insteadOf = "https://github.com/acme/"
            gc.auto = 0
        "#;
        let config: Config = toml::from_str(text).unwrap();
        let git = &config.profiles[0].git;
        assert_eq!(git["pull.rebase"], "true");
        assert_eq!(git["core.editor"], "vim");
        assert_eq!(
            git["url.git@github.com:acme/.insteadOf"],
            "https://github.com/acme/"
        );
        assert_eq!(git["gc.auto"], "0");
        let back: Config = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(config, back);

        let path = temp_config("dotted");
        fs::write(&path, text).unwrap();
        config.save(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            text,
            "untouched profile"
        );
        fs::remove_file(&path).unwrap();
    }
}
