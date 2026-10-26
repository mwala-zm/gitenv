//! The `doctor` subcommand: check that each profile would actually work,
//! not just that a repo carries its name and email. Key files exist, the
//! signing key is one gpg or ssh can use, and, unless offline, the SSH key
//! logs in to each host the profile's remote rules name, as the account
//! the host reports.

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use crate::git;
use crate::profile::{Config, Profile, SigningFormat};
use crate::rules;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub message: String,
}

fn ok(message: impl Into<String>) -> Finding {
    Finding {
        level: Level::Ok,
        message: message.into(),
    }
}

fn warn(message: impl Into<String>) -> Finding {
    Finding {
        level: Level::Warn,
        message: message.into(),
    }
}

fn fail(message: impl Into<String>) -> Finding {
    Finding {
        level: Level::Fail,
        message: message.into(),
    }
}

/// Problems across profiles: ones gitenv would trip over when matching.
pub fn config_findings(config: &Config) -> Vec<Finding> {
    let mut out = Vec::new();
    let profiles = &config.profiles;
    for (i, a) in profiles.iter().enumerate() {
        for b in &profiles[i + 1..] {
            if a.name == b.name {
                out.push(fail(format!("two profiles are named '{}'", a.name)));
            } else if a.user_name == b.user_name
                && a.email == b.email
                && a.signing_key == b.signing_key
                && a.ssh_key == b.ssh_key
                && a.signing_format == b.signing_format
                && a.sign_commits == b.sign_commits
                && a.git == b.git
            {
                out.push(warn(format!(
                    "'{}' and '{}' set the same identity, so a repo on either \
                     shows as '{}'",
                    a.name, b.name, a.name
                )));
            }
        }
    }
    for p in profiles {
        let checks = [rules::validate(&p.remotes), rules::validate_paths(&p.paths)];
        for e in checks.into_iter().filter_map(Result::err) {
            out.push(fail(format!("'{}': {e}", p.name)));
        }
    }
    out
}

/// What `probe` reports for `ssh -T` against a host: the first line the
/// host printed, or why it could not log in.
pub type Probe<'a> = &'a dyn Fn(&Path, &str) -> Result<String, String>;

/// Checks for one profile. `probe` is `None` offline.
pub fn profile_findings(profile: &Profile, probe: Option<Probe>) -> Vec<Finding> {
    let mut out = Vec::new();
    let ssh_key = profile.ssh_key.as_deref().map(git::expand_tilde);
    if let Some(key) = &ssh_key {
        if key.is_file() {
            out.push(ok(format!("ssh key {} exists", key.display())));
        } else {
            out.push(fail(format!("ssh key {} does not exist", key.display())));
        }
    }

    match (&profile.signing_key, profile.signing_format) {
        (None, _) if profile.sign_commits == Some(true) => {
            out.push(fail("sign_commits is on but there is no signing_key"));
        }
        (None, _) => {}
        (Some(key), Some(SigningFormat::Ssh)) => {
            if key.starts_with("key::") || key.starts_with("ssh-") {
                out.push(ok("ssh signing key is given inline"));
            } else {
                let path = git::expand_tilde(Path::new(key));
                if path.is_file() {
                    out.push(ok(format!("ssh signing key {} exists", path.display())));
                } else {
                    out.push(fail(format!(
                        "ssh signing key {} does not exist",
                        path.display()
                    )));
                }
            }
        }
        (Some(_), Some(SigningFormat::X509)) => {
            out.push(warn("x509 signing key not checked"));
        }
        (Some(key), _) => out.push(gpg_key(key)),
    }

    if let (Some(key), Some(probe)) = (&ssh_key, probe)
        && key.is_file()
    {
        for host in hosts(profile) {
            out.push(match probe(key, &host) {
                Ok(greeting) => ok(format!("{host}: {greeting}")),
                Err(why) => fail(format!("{host}: {why}")),
            });
        }
    }

    if out.is_empty() {
        out.push(ok("nothing to check beyond name and email"));
    }
    out
}

fn gpg_key(key: &str) -> Finding {
    let status = Command::new("gpg")
        .args(["--list-secret-keys", key])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => ok(format!("gpg has the secret key {key}")),
        Ok(_) => fail(format!("gpg has no secret key {key}")),
        Err(_) => fail(format!("signing key {key} needs gpg, which is not on PATH")),
    }
}

/// Hosts named without wildcards in the profile's remote rules.
fn hosts(profile: &Profile) -> Vec<String> {
    let mut hosts: Vec<String> = profile
        .remotes
        .iter()
        .filter_map(|r| {
            let host_path = rules::remote_host_path(r).to_ascii_lowercase();
            let host = host_path.split('/').next()?.to_string();
            (!host.is_empty() && !host.contains(['*', '?', '['])).then_some(host)
        })
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// `ssh -T git@host` with only `key`, never prompting. Hosting services
/// refuse a shell but greet the account the key belongs to first.
pub fn ssh_probe(key: &Path, host: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .arg("-i")
        .arg(key)
        .args([
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-T",
        ])
        .arg(format!("git@{host}"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run ssh: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let line = |pred: &dyn Fn(&str) -> bool| {
        text.lines()
            .map(str::trim)
            .find(|l| pred(l))
            .map(str::to_owned)
    };
    if let Some(denied) = line(&|l| l.contains("Permission denied")) {
        return Err(format!(
            "{denied} (the host does not take this key, or it has a passphrase and is not in ssh-agent)"
        ));
    }
    line(&|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("hi ")
            || l.starts_with("welcome")
            || l.contains("logged in as")
            || l.contains("authenticated")
    })
    .ok_or_else(|| match line(&|l| !l.is_empty()) {
        Some(first) => format!("unexpected reply: {first}"),
        None => "no reply".into(),
    })
}

pub fn render(sections: &[(String, Vec<Finding>)]) -> String {
    let mut out = Vec::new();
    for (title, findings) in sections {
        out.push(title.clone());
        for f in findings {
            let word = match f.level {
                Level::Ok => "ok  ",
                Level::Warn => "warn",
                Level::Fail => "fail",
            };
            out.push(format!("  {word}  {}", f.message));
        }
    }
    out.join("\n")
}

pub fn run(offline: bool) -> ExitCode {
    let config = match Config::path().and_then(|p| Config::load(&p)) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            return ExitCode::from(2);
        }
    };
    let mut sections = Vec::new();
    let shared = config_findings(&config);
    if !shared.is_empty() {
        sections.push(("profiles".to_string(), shared));
    }
    let probe: Probe = &ssh_probe;
    for p in &config.profiles {
        let findings = profile_findings(p, (!offline).then_some(probe));
        sections.push((format!("{} <{}>", p.name, p.email), findings));
    }
    if sections.is_empty() {
        println!("gitenv: no profiles saved yet; run `gitenv` to create one");
        return ExitCode::from(1);
    }
    println!("{}", render(&sections));
    let failed = sections
        .iter()
        .flat_map(|(_, f)| f)
        .any(|f| f.level == Level::Fail);
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.into(),
            user_name: "Ada".into(),
            email: format!("ada@{name}.example"),
            ..Default::default()
        }
    }

    fn levels(findings: &[Finding]) -> Vec<Level> {
        findings.iter().map(|f| f.level).collect()
    }

    #[test]
    fn missing_key_files_fail() {
        let mut p = profile("acme");
        p.ssh_key = Some("/nowhere/id_acme".into());
        p.signing_key = Some("/nowhere/id_acme.pub".into());
        p.signing_format = Some(SigningFormat::Ssh);
        let f = profile_findings(&p, None);
        assert_eq!(levels(&f), vec![Level::Fail, Level::Fail], "{f:?}");
        assert!(f[0].message.contains("/nowhere/id_acme does not exist"));
    }

    #[test]
    fn signing_without_a_key_fails_and_plain_profiles_pass() {
        let mut p = profile("acme");
        p.sign_commits = Some(true);
        assert_eq!(levels(&profile_findings(&p, None)), vec![Level::Fail]);
        let plain = profile_findings(&profile("me"), None);
        assert_eq!(levels(&plain), vec![Level::Ok]);
    }

    #[test]
    fn probes_each_named_host_once_with_the_profile_key() {
        let key = std::env::temp_dir().join(format!("gitenv-doctor-{}", std::process::id()));
        std::fs::write(&key, "not really a key").unwrap();
        let mut p = profile("acme");
        p.ssh_key = Some(key.clone());
        p.remotes = vec![
            "github.com/acme/*".into(),
            "git@github.com:acme-labs/*".into(),
            "gitlab.acme.example/**".into(),
            "*/acme/*".into(),
        ];
        let probe = |k: &Path, host: &str| {
            assert_eq!(k, key.as_path());
            if host == "github.com" {
                Ok("Hi ada-acme! You've successfully authenticated".to_string())
            } else {
                Err("Permission denied (publickey)".to_string())
            }
        };
        let f = profile_findings(&p, Some(&probe));
        assert_eq!(levels(&f), vec![Level::Ok, Level::Ok, Level::Fail], "{f:?}");
        assert!(f[1].message.starts_with("github.com: Hi ada-acme!"));
        assert!(f[2].message.starts_with("gitlab.acme.example:"));
        std::fs::remove_file(&key).unwrap();
    }

    #[test]
    fn duplicate_names_identities_and_bad_globs_are_reported() {
        let mut twin = profile("acme");
        twin.name = "acme-old".into();
        let mut bad = profile("bad");
        bad.paths = vec!["[oops".into()];
        let config = Config {
            profiles: vec![profile("acme"), twin, profile("acme"), bad],
        };
        let f = config_findings(&config);
        let text = render(&[("profiles".into(), f.clone())]);
        assert!(
            text.contains("fail  two profiles are named 'acme'"),
            "{text}"
        );
        assert!(
            text.contains("warn  'acme' and 'acme-old' set the same identity"),
            "{text}"
        );
        assert!(text.contains("fail  'bad': Bad pattern '[oops'"), "{text}");
    }
}
