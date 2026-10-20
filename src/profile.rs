use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Environment variable that overrides the config file location.
pub const CONFIG_ENV: &str = "GITENV_CONFIG";

/// One git identity. Serialised as a `[[profiles]]` table in the config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Short label shown in the UI, e.g. `personal` or `acme-corp`.
    pub name: String,
    /// Value written to `user.name`.
    pub user_name: String,
    /// Value written to `user.email`.
    pub email: String,
    /// Optional value for `user.signingkey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,
    /// Optional private key path, written as `core.sshCommand = ssh -i <path>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key: Option<PathBuf>,
}

/// The whole config file.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Config {
    /// Where the config lives. `GITENV_CONFIG` wins when set, so dotfile
    /// managers and tests can point at any file. Otherwise on macOS this is
    /// `~/Library/Application Support/gitenv/config.toml`, on Linux
    /// `~/.config/gitenv/config.toml`.
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(path));
        }
        let dirs = ProjectDirs::from("", "", "gitenv")
            .context("could not determine a config directory for this user")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Load the config file. A missing file is not an error: you get an
    /// empty config and the UI tells you where to create one.
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

    /// Find the profile whose name and email match the given identity.
    /// If the profile declares a signing key, that must match too.
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
                    signing_key: None,
                    ssh_key: None,
                },
                Profile {
                    name: "acme".into(),
                    user_name: "Ada Lovelace".into(),
                    email: "ada@acme.example".into(),
                    signing_key: Some("ABCDEF".into()),
                    ssh_key: Some("/home/ada/.ssh/id_acme".into()),
                },
            ],
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, back);
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
