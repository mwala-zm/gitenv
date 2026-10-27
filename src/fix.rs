//! The `fix-authors` subcommand: rewrite commits that have not been pushed
//! anywhere and were made with another identity than the repo should use.
//!
//! Commits are rebuilt with `git commit-tree` from their own trees and
//! messages, and the branch is moved once with `git update-ref`, so the
//! work tree, the index, ignored files and hooks are never touched.
//! Commits a remote, a tag or another local branch has are left alone, so
//! nothing published or shared is rewritten.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};

use crate::git::{self, Repo};
use crate::profile::{Config, Profile};
use crate::rules;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub committer: String,
    pub committer_email: String,
    pub subject: String,
    pub parents: usize,
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

fn git_ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
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

/// The current branch, or `None` with a detached HEAD.
fn current_branch(root: &Path) -> Option<String> {
    git_out(root, &["symbolic-ref", "-q", "--short", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
}

/// `rev-list` arguments that leave out every commit something else has:
/// remote-tracking branches, tags, and local branches other than this one.
fn published(root: &Path) -> Vec<String> {
    let mut args = vec!["--not".to_string(), "--remotes".into(), "--tags".into()];
    if let Some(branch) = current_branch(root) {
        args.push(format!("--exclude={branch}"));
    }
    args.push("--branches".into());
    args
}

/// Commits on HEAD that nothing else has, oldest first.
fn unpushed(root: &Path) -> Result<Vec<Commit>> {
    if !git_ok(root, &["rev-parse", "--verify", "-q", "HEAD"]) {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "log".to_string(),
        "--reverse".into(),
        "--format=%H%x00%P%x00%an%x00%ae%x00%cn%x00%ce%x00%s".into(),
        "HEAD".into(),
    ];
    args.extend(published(root));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let text = git_out(root, &args)?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\0');
            let hash = f.next()?.to_string();
            let parents = f.next()?.split_whitespace().count();
            Some(Commit {
                hash,
                parents,
                author: f.next()?.to_string(),
                email: f.next()?.to_string(),
                committer: f.next()?.to_string(),
                committer_email: f.next()?.to_string(),
                subject: f.next().unwrap_or("").to_string(),
            })
        })
        .collect())
}

pub struct Plan {
    /// Every commit only HEAD has.
    pub unpushed: Vec<Commit>,
    /// The ones whose author or committer is not the profile.
    pub wrong: Vec<Commit>,
}

fn is_profile(name: &str, email: &str, profile: &Profile) -> bool {
    name == profile.user_name && email.eq_ignore_ascii_case(&profile.email)
}

pub fn plan(root: &Path, profile: &Profile) -> Result<Plan> {
    let unpushed = unpushed(root)?;
    let wrong = unpushed
        .iter()
        .filter(|c| {
            !is_profile(&c.author, &c.email, profile)
                || !is_profile(&c.committer, &c.committer_email, profile)
        })
        .cloned()
        .collect();
    Ok(Plan { unpushed, wrong })
}

fn git_path(root: &Path, name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git_out(root, &["rev-parse", "--git-path", name])?.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Reasons not to touch the branch at all.
fn preflight(root: &Path, plan: &Plan) -> Result<()> {
    for (name, what) in [
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase or git am"),
        ("MERGE_HEAD", "a merge"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
    ] {
        if git_path(root, name)?.exists() {
            bail!("git is in the middle of {what}; finish or abort it first");
        }
    }
    // A branch set to track one that has no remote-tracking ref (a
    // single-branch clone, say) may well be pushed without gitenv seeing it.
    if let Some(branch) = current_branch(root)
        && let Ok(remote) = git_out(root, &["config", &format!("branch.{branch}.remote")])
        && remote.trim() != "."
        && !git_ok(root, &["rev-parse", "--verify", "-q", "@{upstream}"])
    {
        bail!(
            "{branch} tracks a branch on '{}' that this clone has no copy of, so gitenv \
             cannot tell which commits are pushed; run `git fetch` and try again",
            remote.trim()
        );
    }
    if plan.unpushed.iter().any(|c| c.parents > 1) {
        bail!("the unpushed commits include a merge; rewrite them by hand with git rebase");
    }
    let shallow = std::fs::read_to_string(git_path(root, "shallow")?).unwrap_or_default();
    if plan
        .unpushed
        .iter()
        .any(|c| shallow.lines().any(|l| l == c.hash))
    {
        bail!(
            "this is a shallow clone and the commits reach its cut-off; run `git fetch --unshallow` first"
        );
    }
    Ok(())
}

/// The parts of a raw commit object `commit-tree` needs again.
struct Raw {
    tree: String,
    parents: Vec<String>,
    /// `Name <email> <timestamp> <zone>`
    author: String,
    encoding: Option<String>,
    message: Vec<u8>,
}

/// Read many commits with one `git cat-file --batch`.
fn read_commits(root: &Path, hashes: &[&str]) -> Result<Vec<Raw>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running git cat-file")?;
    let mut stdin = child.stdin.take().context("writing to git cat-file")?;
    let input: String = hashes.iter().map(|h| format!("{h}\n")).collect();
    // Written from a thread so a large batch cannot fill both pipes.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("writing to git cat-file"))??;
    let mut rest = output.stdout.as_slice();
    let mut commits = Vec::with_capacity(hashes.len());
    for hash in hashes {
        // `<hash> commit <size>\n<content>\n`
        let line_end = rest
            .iter()
            .position(|&b| b == b'\n')
            .with_context(|| format!("could not read commit {hash}"))?;
        let header = String::from_utf8_lossy(&rest[..line_end]).to_string();
        let mut fields = header.split_whitespace();
        let size: usize = match (fields.next(), fields.next(), fields.next()) {
            (Some(_), Some("commit"), Some(size)) => size.parse()?,
            _ => bail!("could not read commit {hash}: {header}"),
        };
        let body = rest
            .get(line_end + 1..line_end + 1 + size)
            .with_context(|| format!("could not read commit {hash}"))?;
        commits.push(parse_commit(hash, body)?);
        rest = rest.get(line_end + 2 + size..).unwrap_or_default();
    }
    Ok(commits)
}

fn parse_commit(hash: &str, bytes: &[u8]) -> Result<Raw> {
    let split = bytes
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(bytes.len(), |i| i + 2);
    let headers = String::from_utf8_lossy(&bytes[..split]).to_string();
    let mut raw = Raw {
        tree: String::new(),
        parents: Vec::new(),
        author: String::new(),
        encoding: None,
        message: bytes[split..].to_vec(),
    };
    // Continuation lines of gpgsig and mergetag start with a space and are
    // dropped: the commit changes, so its old signature would be invalid.
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("tree ") {
            raw.tree = v.to_string();
        } else if let Some(v) = line.strip_prefix("parent ") {
            raw.parents.push(v.to_string());
        } else if let Some(v) = line.strip_prefix("author ") {
            raw.author = v.to_string();
        } else if let Some(v) = line.strip_prefix("encoding ") {
            raw.encoding = Some(v.to_string());
        }
    }
    if raw.tree.is_empty() || raw.author.is_empty() {
        bail!("could not read commit {hash}");
    }
    Ok(raw)
}

/// Split `Name <email> 1700000000 +0200` into its three parts.
fn split_ident(ident: &str) -> Option<(&str, &str, &str)> {
    let open = ident.rfind('<')?;
    let close = ident.rfind('>')?;
    Some((
        ident[..open].trim_end(),
        &ident[open + 1..close],
        ident[close + 1..].trim(),
    ))
}

/// Rebuild the unpushed commits from the first wrong one on, with the
/// profile as author of each wrong one (keeping its author date) and as
/// committer of all of them, then move HEAD once. The profile is applied
/// to the repo only after that has worked.
pub fn rewrite(repo: &Repo, profile: &Profile, plan: &Plan) -> Result<()> {
    let root = &repo.root;
    preflight(root, plan)?;
    let Some(first) = plan.wrong.first() else {
        return Ok(());
    };
    let old_head = git_out(root, &["rev-parse", "HEAD"])?.trim().to_string();
    let start = plan
        .unpushed
        .iter()
        .position(|c| c.hash == first.hash)
        .context("planned commit is not on HEAD")?;

    let sign = profile
        .sign_commits
        .or(repo.effective.sign_commits)
        .unwrap_or(false);
    let mut settings = Vec::new();
    if let Some(key) = &profile.signing_key {
        settings.push(format!("user.signingkey={key}"));
    }
    if let Some(format) = profile.signing_format {
        settings.push(format!("gpg.format={}", format.as_git()));
    }

    let todo = &plan.unpushed[start..];
    let hashes: Vec<&str> = todo.iter().map(|c| c.hash.as_str()).collect();
    let raws = read_commits(root, &hashes)?;
    let progress = todo.len() >= 200 && io::stderr().is_terminal();
    let mut replaced: Option<(String, String)> = None;
    for (i, (commit, raw)) in todo.iter().zip(raws).enumerate() {
        if progress && i % 100 == 0 {
            eprint!("\rgitenv: rewriting commit {} of {}", i + 1, todo.len());
        }
        let (name, email, date) =
            split_ident(&raw.author).context("could not read a commit's author")?;
        let (name, email) = if is_profile(name, email, profile) {
            (name.to_string(), email.to_string())
        } else {
            (profile.user_name.clone(), profile.email.clone())
        };
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(root);
        for setting in &settings {
            cmd.args(["-c", setting]);
        }
        if let Some(encoding) = &raw.encoding {
            cmd.args(["-c", &format!("i18n.commitEncoding={encoding}")]);
        }
        cmd.args(["commit-tree", &raw.tree]);
        for parent in &raw.parents {
            let parent = match &replaced {
                Some((old, new)) if old == parent => new,
                _ => parent,
            };
            cmd.args(["-p", parent]);
        }
        cmd.arg(if sign { "-S" } else { "--no-gpg-sign" });
        cmd.env("GIT_AUTHOR_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_AUTHOR_DATE", format!("@{date}"))
            .env("GIT_COMMITTER_NAME", &profile.user_name)
            .env("GIT_COMMITTER_EMAIL", &profile.email)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("running git commit-tree")?;
        child
            .stdin
            .take()
            .context("writing to git commit-tree")?
            .write_all(&raw.message)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "git commit-tree failed, nothing was changed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let new = String::from_utf8_lossy(&output.stdout).trim().to_string();
        replaced = Some((commit.hash.clone(), new));
    }

    if progress {
        eprintln!("\rgitenv: rewrote {} commits{:10}", todo.len(), "");
    }
    let (_, new_head) = replaced.context("nothing was rebuilt")?;
    git_out(
        root,
        &[
            "update-ref",
            "-m",
            "gitenv fix-authors: re-author unpushed commits",
            "HEAD",
            &new_head,
            &old_head,
        ],
    )
    .context("moving HEAD; it changed while gitenv was working, so nothing was changed")?;
    git::apply(repo, profile).context("commits rewritten, but applying the profile failed")?;
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
    // Refuse before asking, so a "yes" is never wasted on a no.
    preflight(&repo.root, &plan)?;
    println!(
        "gitenv: {} of {} unpushed commit(s) will be re-authored as profile '{}' ({} <{}>):",
        plan.wrong.len(),
        plan.unpushed.len(),
        profile.name,
        profile.user_name,
        profile.email
    );
    for c in &plan.wrong {
        let who = if is_profile(&c.author, &c.email, profile) {
            format!("committed by {} <{}>", c.committer, c.committer_email)
        } else {
            format!("{} <{}>", c.author, c.email)
        };
        println!("  {} {who}  {}", &c.hash[..10.min(c.hash.len())], c.subject);
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
        "gitenv: rewrote the unpushed commits from {} on and applied profile '{}'; \
         the old ones are in `git reflog`",
        &plan.wrong[0].hash[..10.min(plan.wrong[0].hash.len())],
        profile.name
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git with a blank global config, so the tests do not depend on the
    /// machine's settings.
    fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        git_env(dir, args, &[])
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

    fn fix(dir: &Path) -> Result<()> {
        let repo = git::discover(dir).unwrap().unwrap();
        let plan = plan(dir, &acme()).unwrap();
        rewrite(&repo, &acme(), &plan)
    }

    fn plan_is_clean(dir: &Path) -> bool {
        plan(dir, &acme()).unwrap().wrong.is_empty()
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
        git_env(
            &work,
            &[
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "two",
            ],
            &[("GIT_AUTHOR_DATE", "2001-02-03T04:05:06Z")],
        );
        commit(&work, "ada@acme.example", "three");
        commit(&work, "ada@example.com", "four");

        let plan = plan(&work, &acme()).unwrap();
        assert_eq!(plan.unpushed.len(), 3);
        let subjects: Vec<&str> = plan.wrong.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["two", "four"]);

        fix(&work).unwrap();
        assert_eq!(
            git(&work, &["log", "--format=%ae %ce %s"]),
            "ada@acme.example ada@acme.example four\n\
             ada@acme.example ada@acme.example three\n\
             ada@acme.example ada@acme.example two\n\
             ada@example.com ada@example.com pushed with the wrong email"
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

    #[test]
    fn rewrites_from_the_root_when_nothing_is_pushed() {
        let dir = base("root");
        git(&dir, &["init", "-q"]);
        commit(&dir, "ada@example.com", "one");
        commit(&dir, "ada@example.com", "two");
        let plan = plan(&dir, &acme()).unwrap();
        assert_eq!(plan.wrong.len(), 2);
        fix(&dir).unwrap();
        assert!(plan_is_clean(&dir));
        assert_eq!(git(&dir, &["rev-list", "--count", "HEAD"]), "2");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The old rebase-based rewrite checked out every commit, which deleted
    /// ignored files a past commit had tracked.
    #[test]
    fn leaves_the_work_tree_index_and_ignored_files_alone() {
        let dir = base("tree");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join(".env"), "SECRET=committed-by-mistake\n").unwrap();
        std::fs::write(dir.join("f"), "a\n").unwrap();
        git(&dir, &["add", ".env", "f"]);
        commit(&dir, "ada@example.com", "add config");
        git(&dir, &["rm", "-q", "--cached", ".env"]);
        std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
        git(&dir, &["add", ".gitignore"]);
        commit(&dir, "ada@example.com", "stop tracking .env");
        let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);

        std::fs::write(dir.join(".env"), "SECRET=my-real-local-secret\n").unwrap();
        std::fs::write(dir.join("f"), "staged\n").unwrap();
        git(&dir, &["add", "f"]);
        std::fs::write(dir.join("f"), "staged then edited\n").unwrap();
        std::fs::write(dir.join("untracked"), "u\n").unwrap();

        fix(&dir).unwrap();
        assert!(plan_is_clean(&dir));
        assert_eq!(git(&dir, &["rev-parse", "HEAD^{tree}"]), tree);
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read(".env"), "SECRET=my-real-local-secret\n");
        assert_eq!(read("f"), "staged then edited\n");
        assert_eq!(read("untracked"), "u\n");
        assert_eq!(git(&dir, &["show", ":f"]), "staged", "index kept");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_messages_byte_for_byte_and_runs_no_hooks() {
        let dir = base("message");
        git(&dir, &["init", "-q"]);
        let hook = dir.join(".git/hooks/prepare-commit-msg");
        std::fs::write(&hook, "#!/bin/sh\nsed -i.bak '1s/^/[TICKET-1] /' \"$1\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let message = "#42 handle nulls\n\n\nbody  \n# not a comment\n\n\n";
        git(
            &dir,
            &[
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@example.com",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-q",
                "--allow-empty",
                "--cleanup=verbatim",
                "-m",
                message,
            ],
        );
        let body = |dir: &Path| {
            let raw = git(dir, &["cat-file", "commit", "HEAD"]);
            raw.split_once("\n\n").unwrap().1.to_string()
        };
        let before = body(&dir);
        fix(&dir).unwrap();
        assert!(plan_is_clean(&dir));
        assert_eq!(body(&dir), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_while_a_rebase_is_in_progress() {
        let dir = base("rebasing");
        git(&dir, &["init", "-q"]);
        for n in ["one", "two", "three"] {
            commit(&dir, "ada@example.com", n);
        }
        git_env(
            &dir,
            &["rebase", "-q", "-i", "HEAD~2"],
            &[("GIT_SEQUENCE_EDITOR", "sed -i.bak '1s/^pick/edit/'")],
        );
        // Work done while stopped is on HEAD and nowhere else.
        commit(&dir, "ada@example.com", "user work during the rebase");
        let head = git(&dir, &["rev-parse", "HEAD"]);
        let err = fix(&dir).unwrap_err().to_string();
        assert!(err.contains("middle of a rebase"), "{err}");
        assert!(
            git_path(&dir, "rebase-merge").unwrap().exists(),
            "user's rebase kept"
        );
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_when_the_upstream_has_no_tracking_ref() {
        let dir = base("upstream");
        git(&dir, &["init", "-q", "-b", "feature"]);
        commit(
            &dir,
            "ada@example.com",
            "pushed, but this clone cannot see it",
        );
        git(&dir, &["config", "branch.feature.remote", "origin"]);
        git(
            &dir,
            &["config", "branch.feature.merge", "refs/heads/feature"],
        );
        let err = fix(&dir).unwrap_err().to_string();
        assert!(
            err.contains("cannot tell which commits are pushed"),
            "{err}"
        );
        assert!(!plan_is_clean(&dir), "nothing rewritten");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn commits_on_tags_and_other_branches_are_left_alone() {
        let dir = base("shared");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit(&dir, "ada@example.com", "tagged");
        git(&dir, &["tag", "v1"]);
        commit(&dir, "ada@example.com", "also on side");
        git(&dir, &["branch", "side"]);
        commit(&dir, "ada@example.com", "only here");
        let plan = plan(&dir, &acme()).unwrap();
        let subjects: Vec<&str> = plan.unpushed.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["only here"]);
        let side = git(&dir, &["rev-parse", "side"]);
        fix(&dir).unwrap();
        assert_eq!(git(&dir, &["rev-parse", "HEAD~1"]), side);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_wrong_committer_alone_is_fixed_and_the_author_kept() {
        let dir = base("committer");
        git(&dir, &["init", "-q"]);
        git_env(
            &dir,
            &[
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@acme.example",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
            &[("GIT_COMMITTER_EMAIL", "ada@example.com")],
        );
        assert_eq!(plan(&dir, &acme()).unwrap().wrong.len(), 1);
        fix(&dir).unwrap();
        assert_eq!(
            git(&dir, &["log", "-1", "--format=%ae %ce"]),
            "ada@acme.example ada@acme.example"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merges_block_only_when_unpushed() {
        let dir = base("merges");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit(&dir, "ada@acme.example", "base");
        git(&dir, &["switch", "-q", "-c", "topic"]);
        commit(&dir, "ada@acme.example", "topic");
        git(&dir, &["switch", "-q", "main"]);
        commit(&dir, "ada@acme.example", "main");
        git(
            &dir,
            &[
                "-c",
                "user.name=Ada",
                "-c",
                "user.email=ada@acme.example",
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "merge",
                "topic",
            ],
        );
        git(&dir, &["branch", "-q", "-D", "topic"]);
        let err = fix_wrong_after(&dir).unwrap_err().to_string();
        assert!(err.contains("include a merge"), "{err}");

        // Once the merge is published (tagged here), it no longer matters.
        git(&dir, &["tag", "v1", "HEAD~1"]);
        fix(&dir).unwrap();
        assert!(plan_is_clean(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Add a wrong commit on top and try to fix it.
    fn fix_wrong_after(dir: &Path) -> Result<()> {
        commit(dir, "ada@example.com", "wrong");
        fix(dir)
    }
}
