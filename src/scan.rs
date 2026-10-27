use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use crate::check::{self, Verdict};
use crate::cmd::short_status;
use crate::git::{self, Repo};
use crate::profile::Config;
use crate::rules;

pub struct Options {
    pub problems_only: bool,
    pub fix: bool,
}

const SKIP: &[&str] = &["node_modules", "target", ".git"];

static FIX: Mutex<()> = Mutex::new(());

pub fn find_repos(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    if dir.join(".git").exists() {
        found.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
            continue;
        }
        walk(&entry.path(), found);
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub repo: Repo,
    pub verdict: Verdict,
    pub fixed: Option<String>,
}

impl Row {
    pub fn is_ok(&self) -> bool {
        self.verdict.is_ok() || self.fixed.is_some()
    }

    pub fn status_word(&self) -> &'static str {
        match (&self.fixed, &self.verdict) {
            (Some(_), _) => "fixed",
            (None, Verdict::Match(_)) => "ok",
            (None, Verdict::WrongProfile { .. }) => "wrong",
            (None, Verdict::Mismatch { .. }) => "none",
        }
    }

    pub fn profile_column(&self) -> String {
        short_status(&self.verdict)
    }
}

/// Inspects every repository under `root` on a few threads. `root` is resolved
/// first, since git reports resolved paths and path rules need absolute ones.
/// Returns the rows and a warning per repository git cannot read.
pub fn scan(config: &Config, root: &Path, opts: &Options) -> Result<(Vec<Row>, Vec<String>)> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let paths = find_repos(&root);
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(Vec::with_capacity(paths.len()));
    let warnings = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
        .min(paths.len().max(1));

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(i) else { break };
                    match inspect(config, path, opts) {
                        Ok(row) => rows.lock().unwrap().push(row),
                        Err(warning) => warnings.lock().unwrap().push(warning),
                    }
                }
            });
        }
    });

    let mut rows = rows.into_inner().unwrap();
    rows.sort_by(|a, b| a.repo.root.cmp(&b.repo.root));
    let mut warnings = warnings.into_inner().unwrap();
    warnings.sort();
    Ok((rows, warnings))
}

/// One `git` process per repository: the walk found its top level already and
/// the table shows no history. With `fix`, a wrong repository a rule claims
/// gets that profile and is read again; a fix that worktree config or the
/// environment overrides is reported and the row stays wrong. Fixes run one
/// at a time, since a repository and its worktrees share a config file.
fn inspect(config: &Config, path: &Path, opts: &Options) -> Result<Row, String> {
    let Some(repo) = git::open(path.to_path_buf(), 0) else {
        return Err(format!("skipped {}: git cannot read it", tildify(path)));
    };
    let verdict = check::verdict(config, &repo);
    let mut fixed = None;
    if opts.fix
        && let Verdict::WrongProfile { .. } = &verdict
        && let Some((profile, _)) = rules::expected(config, &repo)
    {
        let _one_at_a_time = FIX.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = git::apply(&repo, profile) {
            eprintln!("gitenv: could not fix {}: {err:#}", tildify(path));
        } else if git::open(path.to_path_buf(), 0)
            .is_some_and(|fresh| check::verdict(config, &fresh).is_ok())
        {
            fixed = Some(profile.name.clone());
        } else {
            eprintln!(
                "gitenv: applied '{}' to {} but git still uses another identity there",
                profile.name,
                tildify(path)
            );
        }
    }
    Ok(Row {
        repo,
        verdict,
        fixed,
    })
}

pub fn render(rows: &[Row], opts: &Options) -> String {
    let shown: Vec<&Row> = rows
        .iter()
        .filter(|r| !opts.problems_only || !r.verdict.is_ok())
        .collect();
    let mut out = Vec::new();
    if !shown.is_empty() {
        let width = shown
            .iter()
            .map(|r| r.profile_column().chars().count())
            .max()
            .unwrap_or(7)
            .max("PROFILE".len());
        out.push(format!("{:<6} {:<width$} REPO", "STATUS", "PROFILE"));
        for row in &shown {
            out.push(format!(
                "{:<6} {:<width$} {}",
                row.status_word(),
                row.profile_column(),
                tildify(&row.repo.root)
            ));
        }
        out.push(String::new());
    }
    let total = rows.len();
    let ok = rows.iter().filter(|r| r.is_ok()).count();
    let fixed = rows.iter().filter(|r| r.fixed.is_some()).count();
    let noun = if total == 1 {
        "repository"
    } else {
        "repositories"
    };
    let mut summary = format!("{total} {noun}, {ok} on the right profile");
    if fixed > 0 {
        summary.push_str(&format!(" ({fixed} just fixed)"));
    }
    if total - ok > 0 {
        summary.push_str(&format!(", {} to look at", total - ok));
    }
    out.push(summary);
    out.join("\n")
}

pub fn tildify(path: &Path) -> String {
    if let Some(dirs) = directories::UserDirs::new()
        && let Ok(rest) = path.strip_prefix(dirs.home_dir())
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

pub fn run(root: &Path, opts: &Options) -> ExitCode {
    let config = match Config::path().and_then(|p| Config::load(&p)) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            return ExitCode::from(2);
        }
    };
    match scan(&config, root, opts) {
        Ok((rows, warnings)) => {
            for warning in &warnings {
                eprintln!("gitenv: {warning}");
            }
            println!("{}", render(&rows, opts));
            if rows.iter().all(Row::is_ok) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    fn init(dir: &Path, remote: &str) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["remote", "add", "origin", remote]);
        git(dir, &["config", "--local", "user.name", "Ada"]);
        git(dir, &["config", "--local", "user.email", "ada@example.com"]);
    }

    #[test]
    fn finds_repos_and_reports_and_fixes_them() {
        let base = std::env::temp_dir().join(format!("gitenv-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        init(&base.join("play/toy"), "git@github.com:ada/toy.git");
        init(&base.join("work/api"), "git@github.com:acme/api.git");
        init(
            &base.join("work/api/vendor/lib"),
            "git@github.com:acme/lib.git",
        );
        std::fs::create_dir_all(base.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(base.join("empty")).unwrap();

        let found = find_repos(&base);
        assert_eq!(
            found,
            vec![base.join("play/toy"), base.join("work/api")],
            "{found:?}"
        );

        let config = Config {
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
                    remotes: vec!["github.com/acme/*".into()],
                    ..Default::default()
                },
            ],
        };

        let opts = Options {
            problems_only: false,
            fix: false,
        };
        let (rows, _) = scan(&config, &base, &opts).unwrap();
        assert_eq!(rows.len(), 2);
        let text = render(&rows, &opts);
        assert!(text.contains("ok     personal"), "{text}");
        assert!(text.contains("wrong  personal -> acme"), "{text}");
        assert!(text.contains("2 repositories, 1 on the right profile, 1 to look at"));

        let only = Options {
            problems_only: true,
            fix: false,
        };
        let text = render(&rows, &only);
        assert!(!text.contains("ok     personal"), "{text}");
        assert!(text.contains("wrong  personal -> acme"), "{text}");

        let fix = Options {
            problems_only: false,
            fix: true,
        };
        let (rows, _) = scan(&config, &base, &fix).unwrap();
        let text = render(&rows, &fix);
        assert!(text.contains("fixed  personal -> acme"), "{text}");
        assert!(text.contains("(1 just fixed)"), "{text}");
        assert!(rows.iter().all(Row::is_ok));

        let (rows, _) = scan(&config, &base, &opts).unwrap();
        assert!(rows.iter().all(|r| r.verdict.is_ok()));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_repo_and_its_worktrees_are_fixed_together() {
        let base = std::env::temp_dir().join(format!("gitenv-scan-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let main = base.join("main");
        init(&main, "git@example.com:x/main.git");
        git(
            &main,
            &[
                "-c",
                "user.name=A",
                "-c",
                "user.email=a@b",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        );
        for n in 0..6 {
            let wt = base.join(format!("wt{n}"));
            git(
                &main,
                &[
                    "worktree",
                    "add",
                    "-q",
                    wt.to_str().unwrap(),
                    "-b",
                    &format!("b{n}"),
                ],
            );
        }
        let config = Config {
            profiles: vec![Profile {
                name: "acme".into(),
                user_name: "Ada".into(),
                email: "ada@acme.example".into(),
                paths: vec![format!(
                    "{}/**",
                    std::fs::canonicalize(&base).unwrap().display()
                )],
                ..Default::default()
            }],
        };
        let fix = Options {
            problems_only: false,
            fix: true,
        };
        let (rows, warnings) = scan(&config, &base, &fix).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|r| r.fixed.is_some()), "{rows:?}");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
