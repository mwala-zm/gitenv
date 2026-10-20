use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub git: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
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

    pub fn save(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialising config")?;
        fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn matching(&self, identity: &crate::git::Identity) -> Option<&Profile> {
        self.profiles.iter().find(|p| identity.matches(p))
    }
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
}
