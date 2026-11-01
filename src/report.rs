use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::git;
use crate::profile::Config;
use crate::rules;
use crate::scan::{find_repos, tildify};

pub struct Options {
    pub since: String,
    pub until: Option<String>,
    pub csv: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub date: String,
    pub hash: String,
    pub email: String,
    pub subject: String,
}

#[derive(Debug, Clone)]
pub struct RepoReport {
    pub root: PathBuf,
    /// The profile the repository belongs to: the one a rule claims it for,
    /// else the one its identity matches.
    pub profile: Option<String>,
    pub commits: Vec<Commit>,
}

pub fn run(root: &Path, opts: &Options) -> ExitCode {
    let config = match Config::path().and_then(|p| Config::load(&p)) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            return ExitCode::from(2);
        }
    };
    if config.profiles.is_empty() {
        eprintln!("gitenv: no profiles saved yet; run `gitenv` to create one");
        return ExitCode::from(2);
    }
    let (reports, warnings) = collect(&config, root, opts);
    for warning in &warnings {
        eprintln!("gitenv: {warning}");
    }
    if opts.csv {
        print!("{}", csv(&reports));
    } else {
        println!("{}", render(&config, &reports, opts));
    }
    ExitCode::SUCCESS
}

/// Every commit you made under `root` in the period: one `git log` per
/// repository, over all its branches, keeping the commits whose author email
/// belongs to any profile. Teammates' commits are left out, so the report is
/// about your work, grouped by the profile each repository belongs to.
pub fn collect(config: &Config, root: &Path, opts: &Options) -> (Vec<RepoReport>, Vec<String>) {
    let mine: BTreeSet<String> = config
        .profiles
        .iter()
        .map(|p| p.email.to_ascii_lowercase())
        .collect();
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut reports = Vec::new();
    let mut warnings = Vec::new();
    for path in find_repos(&root) {
        let Some(repo) = git::open(path.clone(), 0) else {
            warnings.push(format!("skipped {}: git cannot read it", tildify(&path)));
            continue;
        };
        let profile = rules::expected(config, &repo)
            .map(|(p, _)| p)
            .or_else(|| config.matching(&repo.effective))
            .map(|p| p.name.clone());
        let commits = match log(&repo.root, opts) {
            Ok(all) => all
                .into_iter()
                .filter(|c| mine.contains(&c.email.to_ascii_lowercase()))
                .collect::<Vec<_>>(),
            Err(why) => {
                warnings.push(format!("skipped {}: {why}", tildify(&repo.root)));
                continue;
            }
        };
        if !commits.is_empty() {
            reports.push(RepoReport {
                root: repo.root,
                profile,
                commits,
            });
        }
    }
    (reports, warnings)
}

fn log(root: &Path, opts: &Options) -> Result<Vec<Commit>, String> {
    let mut git = Command::new("git");
    git.arg("-C")
        .arg(root)
        .args([
            "log",
            "--all",
            "--date=short",
            "--format=%h%x09%ad%x09%ae%x09%s",
        ])
        .arg(format!("--since={}", opts.since));
    if let Some(until) = &opts.until {
        git.arg(format!("--until={until}"));
    }
    let output = git
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stderr);
        if text.contains("does not have any commits") || text.contains("bad revision") {
            return Ok(Vec::new());
        }
        return Err(text.trim().to_string());
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

fn parse(text: &str) -> Vec<Commit> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            Some(Commit {
                hash: parts.next()?.to_string(),
                date: parts.next()?.to_string(),
                email: parts.next()?.to_string(),
                subject: parts.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn groups<'a>(
    config: &Config,
    reports: &'a [RepoReport],
) -> Vec<(Option<String>, Vec<&'a RepoReport>)> {
    let mut out: Vec<(Option<String>, Vec<&RepoReport>)> = config
        .profiles
        .iter()
        .map(|p| (Some(p.name.clone()), Vec::new()))
        .collect();
    out.push((None, Vec::new()));
    for report in reports {
        if let Some((_, members)) = out.iter_mut().find(|(name, _)| *name == report.profile) {
            members.push(report);
        }
    }
    out.retain(|(_, members)| !members.is_empty());
    out
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

pub fn render(config: &Config, reports: &[RepoReport], opts: &Options) -> String {
    let mut out = Vec::new();
    let mut total = 0;
    let mut foreign = 0;
    for (name, members) in groups(config, reports) {
        let profile = name
            .as_deref()
            .and_then(|n| config.profiles.iter().find(|p| p.name == n));
        let count: usize = members.iter().map(|r| r.commits.len()).sum();
        total += count;
        let heading = match profile {
            Some(p) => format!("{} <{}>", p.name, p.email),
            None => "(no profile)".to_string(),
        };
        out.push(format!("{heading:<44} {}", plural(count, "commit")));
        for report in members {
            out.push(format!(
                "  {:<42} {}",
                tildify(&report.root),
                report.commits.len()
            ));
            for c in &report.commits {
                let mut line = format!("    {}  {:<9} {}", c.date, c.hash, c.subject);
                if profile.is_some_and(|p| !p.email.eq_ignore_ascii_case(&c.email)) {
                    foreign += 1;
                    line.push_str(&format!("  (as {})", c.email));
                }
                out.push(line);
            }
        }
        out.push(String::new());
    }
    let period = match &opts.until {
        Some(until) => format!("from {} until {until}", opts.since),
        None => format!("since {}", opts.since),
    };
    let mut summary = format!(
        "{} in {} {period}",
        plural(total, "commit"),
        plural(reports.len(), "repository").replace("repositorys", "repositories")
    );
    if foreign > 0 {
        summary.push_str(&format!("; {foreign} made as another identity"));
    }
    out.push(summary);
    out.join("\n")
}

/// One row per commit, for a spreadsheet or an invoicing script. Fields that
/// hold a comma, quote or line break are quoted the RFC 4180 way.
pub fn csv(reports: &[RepoReport]) -> String {
    let mut out = String::from("date,profile,repo,commit,author_email,subject\n");
    for report in reports {
        for c in &report.commits {
            let row = [
                c.date.as_str(),
                report.profile.as_deref().unwrap_or(""),
                &report.root.display().to_string(),
                &c.hash,
                &c.email,
                &c.subject,
            ];
            let row: Vec<String> = row.iter().map(|f| csv_field(f)).collect();
            out.push_str(&row.join(","));
            out.push('\n');
        }
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;

    fn git(dir: &Path, date: &str, email: &str, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .env("GIT_AUTHOR_DATE", format!("{date}T12:00:00"))
            .env("GIT_COMMITTER_DATE", format!("{date}T12:00:00"))
            .args(["-c", "user.name=Ada", "-c", &format!("user.email={email}")])
            .args(args)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn commit(dir: &Path, date: &str, email: &str, subject: &str) {
        git(
            dir,
            date,
            email,
            &["commit", "-q", "--allow-empty", "-m", subject],
        );
    }

    fn config(base: &Path) -> Config {
        Config {
            profiles: vec![
                Profile {
                    name: "personal".into(),
                    user_name: "Ada".into(),
                    email: "ada@example.com".into(),
                    ..Default::default()
                },
                Profile {
                    name: "acme".into(),
                    user_name: "Ada".into(),
                    email: "ada@acme.example".into(),
                    paths: vec![format!(
                        "{}/acme/**",
                        std::fs::canonicalize(base).unwrap().display()
                    )],
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn groups_your_commits_by_the_repos_profile() {
        let base = std::env::temp_dir().join(format!("gitenv-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let api = base.join("acme/api");
        let play = base.join("play");
        for dir in [&api, &play] {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, "2026-10-01", "ada@example.com", &["init", "-q"]);
        }
        commit(&api, "2020-01-01", "ada@acme.example", "Ancient");
        commit(&api, "2026-10-02", "ada@acme.example", "Add login");
        commit(&api, "2026-10-03", "ada@example.com", "Oops, wrong hat");
        commit(&api, "2026-10-04", "bob@acme.example", "Bob's work");
        commit(
            &play,
            "2026-10-05",
            "ada@example.com",
            "Tinker, with a comma",
        );
        git(
            &play,
            "2026-10-05",
            "ada@example.com",
            &["config", "user.email", "ada@example.com"],
        );
        git(
            &play,
            "2026-10-05",
            "ada@example.com",
            &["config", "user.name", "Ada"],
        );

        let config = config(&base);
        let opts = Options {
            since: "2026-01-01".into(),
            until: Some("2026-12-31".into()),
            csv: false,
        };
        let (reports, warnings) = collect(&config, &base, &opts);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reports.len(), 2, "{reports:?}");
        let api_report = reports
            .iter()
            .find(|r| r.root.ends_with("acme/api"))
            .unwrap();
        assert_eq!(api_report.profile.as_deref(), Some("acme"));
        let subjects: Vec<&str> = api_report
            .commits
            .iter()
            .map(|c| c.subject.as_str())
            .collect();
        assert_eq!(subjects, ["Oops, wrong hat", "Add login"]);
        let play_report = reports.iter().find(|r| r.root.ends_with("play")).unwrap();
        assert_eq!(play_report.profile.as_deref(), Some("personal"));

        let text = render(&config, &reports, &opts);
        assert!(text.starts_with("personal <ada@example.com>"), "{text}");
        assert!(text.contains("acme <ada@acme.example>"), "{text}");
        assert!(
            text.contains("Oops, wrong hat  (as ada@example.com)"),
            "{text}"
        );
        assert!(!text.contains("Bob"), "{text}");
        assert!(
            text.ends_with(
                "3 commits in 2 repositories from 2026-01-01 until 2026-12-31; 1 made as another identity"
            ),
            "{text}"
        );

        let csv = csv(&reports);
        assert!(csv.starts_with("date,profile,repo,commit,author_email,subject\n"));
        assert!(csv.contains(",\"Tinker, with a comma\"\n"), "{csv}");
        assert_eq!(csv.lines().count(), 4, "{csv}");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn csv_fields_are_quoted_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn log_lines_parse_with_tabs_in_subjects() {
        let commits = parse("abc1234\t2026-10-02\tada@acme.example\tAdd\tlogin\n");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "Add\tlogin");
        assert_eq!(commits[0].hash, "abc1234");
    }
}
