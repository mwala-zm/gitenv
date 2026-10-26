//! The `fix-authors` subcommand: rewrite commits that have not been pushed
//! anywhere and were authored with another identity than the repo should
//! use. Pushed history is never touched, so nobody else's clone breaks.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::git::{self, Repo};
use crate::profile::{Config, Profile};
use crate::rules;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub subject: String,
}

fn git_out(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// The profile commits here should carry: the one a rule claims the repo
/// for, else the one it is on now.
pub fn target<'a>(config: &'a Config, repo: &Repo) -> Result<&'a Profile> {
    if let Some((profile, _)) = rules::expected(config, repo) {
        return Ok(profile);
    }
    config.matching(&repo.effective).with_context(|| {
        format!(
            "no rule claims {} and its identity matches no profile; \
             apply one first: gitenv apply <PROFILE>",
            repo.root.display()
        )
    })
}

/// Commits on HEAD that no remote branch has, oldest first.
fn unpushed(root: &Path) -> Result<Vec<Commit>> {
    let has_head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_head {
        return Ok(Vec::new());
    }
    let text = git_out(
        root,
        &[
            "log",
            "--reverse",
            "--format=%H%x00%an%x00%ae%x00%s",
            "HEAD",
            "--not",
            "--remotes",
        ],
    )?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\0');
            Some(Commit {
                hash: f.next()?.to_string(),
                author: f.next()?.to_string(),
                email: f.next()?.to_string(),
                subject: f.next().unwrap_or("").to_string(),
            })
        })
        .collect())
}

pub struct Plan {
    /// Every unpushed commit; all of them are replayed.
    pub unpushed: Vec<Commit>,
    /// The ones whose author changes.
    pub wrong: Vec<Commit>,
}

pub fn plan(root: &Path, profile: &Profile) -> Result<Plan> {
    let unpushed = unpushed(root)?;
    let wrong = unpushed
        .iter()
        .filter(|c| c.email != profile.email || c.author != profile.user_name)
        .cloned()
        .collect();
    Ok(Plan { unpushed, wrong })
}

/// Replay the unpushed commits, amending the author of each wrong one and
/// keeping its author date. The repo gets the profile applied first so
/// the replayed commits are committed, and signed, as that profile.
pub fn rewrite(repo: &Repo, profile: &Profile, plan: &Plan) -> Result<()> {
    let root = &repo.root;
    let Some(oldest) = plan.unpushed.first() else {
        return Ok(());
    };
    let dirty = git_out(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty.trim().is_empty() {
        bail!("commit or stash your changes first; rewriting needs a clean work tree");
    }
    let merges = git_out(
        root,
        &["rev-list", "--merges", "HEAD", "--not", "--remotes"],
    )?;
    if !merges.trim().is_empty() {
        bail!("the unpushed commits include a merge; rewrite them by hand with git rebase");
    }
    git::apply(repo, profile)?;

    let author = format!("{} <{}>", profile.user_name, profile.email);
    let exec = format!(
        "test \"$(git log -1 --format='%an <%ae>')\" = {author} || \
         git commit --quiet --amend --no-edit --no-verify --allow-empty --author={author}",
        author = git::shell_quote(&author)
    );
    let is_root = git_out(root, &["rev-list", "--max-parents=0", "HEAD"])?
        .lines()
        .any(|h| h == oldest.hash);
    let base = format!("{}^", oldest.hash);
    let mut args = vec!["rebase", "--quiet", "--exec", exec.as_str()];
    args.push(if is_root { "--root" } else { base.as_str() });
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(&args)
        .output()
        .context("running git rebase")?;
    if !output.status.success() {
        let _ = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rebase", "--abort"])
            .output();
        bail!(
            "git rebase failed and was undone: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub fn run(dir: &Path, yes: bool) -> ExitCode {
    match try_run(dir, yes) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("gitenv: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn try_run(dir: &Path, yes: bool) -> Result<ExitCode> {
    let (config, repo) = crate::cmd::load(dir)?;
    let profile = target(&config, &repo)?;
    let plan = plan(&repo.root, profile)?;
    if plan.wrong.is_empty() {
        println!(
            "gitenv: all {} unpushed commit(s) are by {} <{}>",
            plan.unpushed.len(),
            profile.user_name,
            profile.email
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "gitenv: {} of {} unpushed commit(s) will be re-authored as profile '{}' ({} <{}>):",
        plan.wrong.len(),
        plan.unpushed.len(),
        profile.name,
        profile.user_name,
        profile.email
    );
    for c in &plan.wrong {
        println!(
            "  {} {} <{}>  {}",
            &c.hash[..10.min(c.hash.len())],
            c.author,
            c.email,
            c.subject
        );
    }
    if !yes {
        if !io::stdin().is_terminal() {
            println!("rerun with --yes to rewrite them");
            return Ok(ExitCode::from(1));
        }
        if !confirm("Rewrite them?") {
            println!("nothing changed");
            return Ok(ExitCode::from(1));
        }
    }
    rewrite(&repo, profile, &plan)?;
    println!(
        "gitenv: rewrote {} commit(s) and applied profile '{}'; the old ones are in `git reflog`",
        plan.unpushed.len(),
        profile.name
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit(dir: &Path, email: &str, msg: &str) {
        git(
            dir,
            &[
                "-c",
                "user.name=Ada",
                "-c",
                &format!("user.email={email}"),
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                msg,
            ],
        );
    }

    fn acme() -> Profile {
        Profile {
            name: "acme".into(),
            user_name: "Ada".into(),
            email: "ada@acme.example".into(),
            ..Default::default()
        }
    }

    fn base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitenv-fix-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rewrites_only_unpushed_commits_with_the_wrong_author() {
        let dir = base("pushed");
        let remote = dir.join("remote.git");
        let work = dir.join("work");
        git(&dir, &["init", "-q", "--bare", remote.to_str().unwrap()]);
        git(&dir, &["init", "-q", work.to_str().unwrap()]);
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        commit(&work, "ada@example.com", "pushed with the wrong email");
        git(&work, &["push", "-q", "origin", "HEAD:main"]);
        git(&work, &["fetch", "-q"]);
        let pushed = git(&work, &["rev-parse", "HEAD"]);
        // An old author date, so keeping it is not a coincidence.
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
                .args(["commit", "-q", "--allow-empty", "-m", "two"])
                .env("GIT_AUTHOR_DATE", "2001-02-03T04:05:06Z")
                .status()
                .unwrap()
                .success()
        );
        commit(&work, "ada@acme.example", "three");
        commit(&work, "ada@example.com", "four");

        let repo = git::discover(&work).unwrap().unwrap();
        let plan = plan(&work, &acme()).unwrap();
        assert_eq!(plan.unpushed.len(), 3);
        let subjects: Vec<&str> = plan.wrong.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["two", "four"]);

        rewrite(&repo, &acme(), &plan).unwrap();
        let emails = git(&work, &["log", "--format=%ae %s"]);
        assert_eq!(
            emails,
            "ada@acme.example four\nada@acme.example three\nada@acme.example two\n\
             ada@example.com pushed with the wrong email"
        );
        assert_eq!(
            git(&work, &["rev-parse", "HEAD~3"]),
            pushed,
            "pushed commit untouched"
        );
        assert_eq!(
            git(&work, &["log", "-1", "--format=%aI", "HEAD~2"]),
            "2001-02-03T04:05:06Z"
        );
        assert_eq!(
            git(&work, &["config", "--local", "user.email"]),
            "ada@acme.example"
        );
        assert!(plan_is_clean(&work));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn plan_is_clean(dir: &Path) -> bool {
        plan(dir, &acme()).unwrap().wrong.is_empty()
    }

    #[test]
    fn rewrites_from_the_root_when_nothing_is_pushed() {
        let dir = base("root");
        git(&dir, &["init", "-q"]);
        commit(&dir, "ada@example.com", "one");
        commit(&dir, "ada@example.com", "two");
        let repo = git::discover(&dir).unwrap().unwrap();
        let plan = plan(&dir, &acme()).unwrap();
        assert_eq!(plan.wrong.len(), 2);
        rewrite(&repo, &acme(), &plan).unwrap();
        assert!(plan_is_clean(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_a_dirty_tree() {
        let dir = base("dirty");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f"), "a").unwrap();
        git(&dir, &["add", "f"]);
        commit(&dir, "ada@example.com", "one");
        std::fs::write(dir.join("f"), "b").unwrap();
        let repo = git::discover(&dir).unwrap().unwrap();
        let plan = plan(&dir, &acme()).unwrap();
        let err = rewrite(&repo, &acme(), &plan).unwrap_err().to_string();
        assert!(err.contains("clean work tree"), "{err}");
        assert!(!plan_is_clean(&dir), "nothing rewritten");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
