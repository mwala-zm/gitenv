use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::profile::Profile;

/// Splits a profile's `gh_account` into login and host: `ada-acme` is a
/// github.com login, `ada@ghe.acme.example` one on that host.
pub fn account(profile: &Profile) -> Option<(&str, &str)> {
    let account = profile.gh_account.as_deref()?.trim();
    if account.is_empty() {
        return None;
    }
    Some(match account.rsplit_once('@') {
        Some((login, host)) if !login.is_empty() && !host.is_empty() => (login, host),
        _ => (account, "github.com"),
    })
}

/// Makes the GitHub CLI act as the profile's account, so `gh pr create` and
/// friends open as the same person the commits name. Nothing happens for a
/// profile without `gh_account`. The login must already be known to gh: this
/// switches, it never logs in.
pub fn switch(profile: &Profile) -> Result<Option<String>> {
    let Some((login, host)) = account(profile) else {
        return Ok(None);
    };
    let output = Command::new("gh")
        .args(["auth", "switch", "--hostname", host, "--user", login])
        .stdin(Stdio::null())
        .output()
        .context("running gh; is the GitHub CLI installed and on PATH?")?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "gh could not switch to {login} on {host}: {}",
            if why.is_empty() {
                "it is not logged in there; run `gh auth login`".to_string()
            } else {
                why
            }
        );
    }
    Ok(Some(format!("{login} on {host}")))
}

/// Whether gh knows the account at all, for `doctor`. `gh auth status` lists
/// every login it holds for the host, active or not.
pub fn known(profile: &Profile) -> Option<Result<String, String>> {
    let (login, host) = account(profile)?;
    let output = Command::new("gh")
        .args(["auth", "status", "--hostname", host])
        .stdin(Stdio::null())
        .output();
    Some(match output {
        Err(_) => Err(format!(
            "gh_account {login} needs the GitHub CLI, which is not on PATH"
        )),
        Ok(output) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if lists_login(&text, login) {
                Ok(format!("gh is logged in to {host} as {login}"))
            } else {
                Err(format!(
                    "gh is not logged in to {host} as {login}; run `gh auth login --hostname {host}`"
                ))
            }
        }
    })
}

fn lists_login(status_text: &str, login: &str) -> bool {
    status_text.lines().any(|line| {
        let line = line.trim();
        line.contains("account ")
            && line
                .split_whitespace()
                .skip_while(|w| *w != "account")
                .nth(1)
                .is_some_and(|w| w.eq_ignore_ascii_case(login))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(account: &str) -> Profile {
        Profile {
            name: "acme".into(),
            user_name: "Ada".into(),
            email: "ada@acme.example".into(),
            gh_account: Some(account.into()),
            ..Default::default()
        }
    }

    #[test]
    fn account_splits_login_and_host() {
        assert_eq!(account(&with("ada-acme")), Some(("ada-acme", "github.com")));
        assert_eq!(
            account(&with("ada@ghe.acme.example")),
            Some(("ada", "ghe.acme.example"))
        );
        assert_eq!(account(&with("  ")), None);
        assert_eq!(account(&Profile::default()), None);
    }

    #[test]
    fn status_output_names_the_login() {
        let text = "github.com\n  ✓ Logged in to github.com account ada-acme (keyring)\n  - Active account: true\n  ✓ Logged in to github.com account ada (keyring)\n";
        assert!(lists_login(text, "ada-acme"));
        assert!(lists_login(text, "ADA"));
        assert!(!lists_login(text, "nobody"));
    }
}
