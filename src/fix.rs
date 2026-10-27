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

fn git(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).env("GIT_NO_REPLACE_OBJECTS", "1");
    cmd
}

fn git_out(root: &Path, args: &[&str]) -> Result<String> {
    let output = git(root).args(args).output().context("running git")?;
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
    git(root)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

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

fn current_branch(root: &Path) -> Option<String> {
    git_out(root, &["symbolic-ref", "-q", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| b.starts_with("refs/heads/"))
}

fn short(branch: &str) -> &str {
    branch.trim_start_matches("refs/heads/")
}

/// `rev-list` arguments that leave out every commit something else has:
/// remote-tracking branches, tags, and local branches other than this one, so
/// nothing published or shared is rewritten.
fn published(branch: Option<&str>) -> Vec<String> {
    let mut args = vec!["--not".to_string(), "--remotes".into(), "--tags".into()];
    if let Some(branch) = branch {
        args.push(format!("--exclude={}", short(branch)));
    }
    args.push("--branches".into());
    args
}

fn unpushed(root: &Path, branch: Option<&str>) -> Result<Vec<Commit>> {
    let mut args = vec![
        "log".to_string(),
        "--reverse".into(),
        "--format=%H%x00%P%x00%an%x00%ae%x00%cn%x00%ce%x00%s".into(),
        "HEAD".into(),
    ];
    args.extend(published(branch));
    args.push("--".into());
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
    pub unpushed: Vec<Commit>,
    pub wrong: Vec<Commit>,
    head: String,
    branch: Option<String>,
}

fn is_profile(name: &str, email: &str, profile: &Profile) -> bool {
    name == git::as_git_writes(&profile.user_name) && email.eq_ignore_ascii_case(&profile.email)
}

pub fn plan(root: &Path, profile: &Profile) -> Result<Plan> {
    let head = match git_out(root, &["rev-parse", "--verify", "-q", "HEAD"]) {
        Ok(head) => head.trim().to_string(),
        Err(_) => {
            return Ok(Plan {
                unpushed: Vec::new(),
                wrong: Vec::new(),
                head: String::new(),
                branch: None,
            });
        }
    };
    let branch = current_branch(root);
    let unpushed = unpushed(root, branch.as_deref())?;
    let wrong = unpushed
        .iter()
        .filter(|c| {
            !is_profile(&c.author, &c.email, profile)
                || !is_profile(&c.committer, &c.committer_email, profile)
        })
        .cloned()
        .collect();
    Ok(Plan {
        unpushed,
        wrong,
        head,
        branch,
    })
}

fn git_path(root: &Path, name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git_out(root, &["rev-parse", "--git-path", name])?.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Refuses while git is in the middle of a rebase, merge, cherry-pick, revert
/// or bisect; with old-style grafts, which git log follows but the rebuilt
/// commits could not; when the branch tracks a remote branch this clone has no
/// copy of, as in a single-branch clone, since it may be pushed without gitenv
/// seeing it; when the commits include a merge; and when they reach a shallow
/// clone's cut-off.
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
    if git_path(root, "info/grafts")?.exists() {
        bail!(
            "this repository uses .git/info/grafts; convert them with `git replace --convert-graft-file` first"
        );
    }
    if let Some(branch) = plan.branch.as_deref().map(short)
        && let Ok(remote) = git_out(root, &["config", &format!("branch.{branch}.remote")])
        && remote.trim() != "."
        && !git_ok(root, &["rev-parse", "--verify", "-q", "@{upstream}"])
    {
        let remote = remote.trim();
        let merge =
            git_out(root, &["config", &format!("branch.{branch}.merge")]).unwrap_or_default();
        let upstream = merge.trim().trim_start_matches("refs/heads/");
        bail!(
            "{branch} tracks {upstream} on '{remote}', which this clone keeps no copy of, so \
             gitenv cannot tell which commits are pushed; fetch it with \
             `git fetch {remote} {upstream}:refs/remotes/{remote}/{upstream}` and try again"
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

struct Raw {
    tree: String,
    parents: Vec<String>,
    author: Vec<u8>,
    encoding: Option<String>,
    others: Vec<u8>,
    message: Vec<u8>,
}

/// Reads the commits with one `git cat-file --batch`, parsing its `<hash>
/// commit <size>` headers. The hashes are written from a thread so a large
/// batch cannot fill both pipes.
fn read_commits(root: &Path, hashes: &[&str]) -> Result<Vec<Raw>> {
    let mut child = git(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running git cat-file")?;
    let mut stdin = child.stdin.take().context("writing to git cat-file")?;
    let input: String = hashes.iter().map(|h| format!("{h}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("writing to git cat-file"))??;
    let mut rest = output.stdout.as_slice();
    let mut commits = Vec::with_capacity(hashes.len());
    for hash in hashes {
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

/// Splits a raw commit into the headers gitenv rewrites (tree, parents,
/// author, committer), the encoding, and every other header, kept as bytes
/// with its continuation lines. Signature headers (gpgsig, gpgsig-sha256,
/// mergetag) are dropped: the rebuilt commit is a different one, so they
/// would no longer be valid.
fn parse_commit(hash: &str, bytes: &[u8]) -> Result<Raw> {
    let split = bytes
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(bytes.len(), |i| i + 1);
    let mut raw = Raw {
        tree: String::new(),
        parents: Vec::new(),
        author: Vec::new(),
        encoding: None,
        others: Vec::new(),
        message: bytes.get(split + 1..).unwrap_or_default().to_vec(),
    };
    let mut skipping = false;
    for line in bytes[..split].split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b" ") {
            if !skipping {
                raw.others.extend_from_slice(line);
            }
            continue;
        }
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\n');
        let (key, value) = text.split_once(' ').unwrap_or((text, ""));
        skipping = false;
        match key {
            "tree" => raw.tree = value.to_string(),
            "parent" => raw.parents.push(value.to_string()),
            "author" => raw.author = line[7..].strip_suffix(b"\n").unwrap_or(&line[7..]).to_vec(),
            "committer" => {}
            "gpgsig" | "gpgsig-sha256" | "mergetag" => skipping = true,
            "encoding" => {
                raw.encoding = Some(value.to_string());
                raw.others.extend_from_slice(line);
            }
            _ => raw.others.extend_from_slice(line),
        }
    }
    if raw.tree.is_empty() || raw.author.is_empty() {
        bail!("could not read commit {hash}");
    }
    Ok(raw)
}

/// `name` as the bytes a commit in `encoding` should hold: Latin-1 for an
/// ISO-8859-1 commit when every character fits, else UTF-8, as git writes.
fn encode(name: &str, encoding: Option<&str>) -> Vec<u8> {
    let latin1 = encoding.is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1"
        )
    });
    if latin1 && name.chars().all(|c| (c as u32) < 256) {
        name.chars().map(|c| c as u8).collect()
    } else {
        name.as_bytes().to_vec()
    }
}

fn ident(name: &[u8], email: &str, date: &[u8]) -> Vec<u8> {
    let mut out = name.to_vec();
    out.extend_from_slice(format!(" <{email}> ").as_bytes());
    out.extend_from_slice(date);
    out
}

fn date_of(ident: &[u8]) -> &[u8] {
    match ident.iter().rposition(|&b| b == b'>') {
        Some(i) => ident[i + 1..].trim_ascii(),
        None => b"",
    }
}

/// Writes one commit object: with `git commit-tree` when it must be signed,
/// otherwise byte for byte with `git hash-object`, which keeps every other
/// header (jj's change-id, say) and the exact message.
fn write_commit(
    root: &Path,
    raw: &Raw,
    parents: &[String],
    author: &[u8],
    committer: &[u8],
    sign: Option<&[String]>,
) -> Result<String> {
    let mut cmd = git(root);
    let mut input = Vec::new();
    match sign {
        Some(settings) => {
            for setting in settings {
                cmd.args(["-c", setting]);
            }
            if let Some(encoding) = &raw.encoding {
                cmd.args(["-c", &format!("i18n.commitEncoding={encoding}")]);
            }
            cmd.args(["commit-tree", &raw.tree, "-S"]);
            for parent in parents {
                cmd.args(["-p", parent]);
            }
            let (name, rest) = split_at_email(author);
            let (cname, crest) = split_at_email(committer);
            cmd.env("GIT_AUTHOR_NAME", String::from_utf8_lossy(name).as_ref())
                .env("GIT_AUTHOR_EMAIL", rest.0)
                .env("GIT_AUTHOR_DATE", format!("@{}", rest.1))
                .env(
                    "GIT_COMMITTER_NAME",
                    String::from_utf8_lossy(cname).as_ref(),
                )
                .env("GIT_COMMITTER_EMAIL", crest.0);
            input.extend_from_slice(&raw.message);
        }
        None => {
            cmd.args(["hash-object", "-t", "commit", "-w", "--stdin"]);
            input.extend_from_slice(format!("tree {}\n", raw.tree).as_bytes());
            for parent in parents {
                input.extend_from_slice(format!("parent {parent}\n").as_bytes());
            }
            input.extend_from_slice(b"author ");
            input.extend_from_slice(author);
            input.extend_from_slice(b"\ncommitter ");
            input.extend_from_slice(committer);
            input.push(b'\n');
            input.extend_from_slice(&raw.others);
            input.push(b'\n');
            input.extend_from_slice(&raw.message);
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git")?;
    child
        .stdin
        .take()
        .context("writing to git")?
        .write_all(&input)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "writing a commit failed, nothing was changed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn split_at_email(ident: &[u8]) -> (&[u8], (String, String)) {
    let open = ident.iter().rposition(|&b| b == b'<').unwrap_or(0);
    let close = ident
        .iter()
        .rposition(|&b| b == b'>')
        .unwrap_or(ident.len());
    let email = String::from_utf8_lossy(&ident[(open + 1).min(close)..close]).to_string();
    let date = String::from_utf8_lossy(date_of(ident)).to_string();
    (ident[..open].trim_ascii_end(), (email, date))
}

/// Rebuilds the unpushed commits from the first wrong one on, from their own
/// trees, headers and exact message bytes, with the profile as author of each
/// one whose author is wrong (keeping its date) and as committer of all of
/// them. git runs with replace objects off so their content is never baked
/// in. The branch HEAD was on when planning then moves once with
/// `update-ref` from exactly the planned tip, so a commit or branch switch
/// made meanwhile makes it refuse. The work tree, index, ignored files and
/// hooks are never touched, and the profile is applied only after all that
/// worked.
pub fn rewrite(repo: &Repo, profile: &Profile, plan: &Plan) -> Result<()> {
    let root = &repo.root;
    preflight(root, plan)?;
    let Some(first) = plan.wrong.first() else {
        return Ok(());
    };
    if current_branch(root) != plan.branch {
        bail!("HEAD moved to another branch since gitenv looked; nothing was changed");
    }
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
    let now = git(root)
        .args(["var", "GIT_COMMITTER_IDENT"])
        .env("GIT_COMMITTER_NAME", &profile.user_name)
        .env("GIT_COMMITTER_EMAIL", &profile.email)
        .output()
        .context("running git var")?
        .stdout;
    let now = date_of(&now).to_vec();
    let name = git::as_git_writes(&profile.user_name);

    let todo = &plan.unpushed[start..];
    let hashes: Vec<&str> = todo.iter().map(|c| c.hash.as_str()).collect();
    let raws = read_commits(root, &hashes)?;
    let progress = todo.len() >= 200 && io::stderr().is_terminal();
    let mut replaced: Option<(String, String)> = None;
    for (i, (commit, raw)) in todo.iter().zip(raws).enumerate() {
        if progress && i % 100 == 0 {
            eprint!("\rgitenv: rewriting commit {} of {}", i + 1, todo.len());
        }
        let encoding = raw.encoding.as_deref();
        let author = if is_profile(&commit.author, &commit.email, profile) {
            raw.author.clone()
        } else {
            ident(
                &encode(name, encoding),
                &profile.email,
                date_of(&raw.author),
            )
        };
        let committer = ident(&encode(name, encoding), &profile.email, &now);
        let parents: Vec<String> = raw
            .parents
            .iter()
            .map(|p| match &replaced {
                Some((old, new)) if old == p => new.clone(),
                _ => p.clone(),
            })
            .collect();
        let new = write_commit(
            root,
            &raw,
            &parents,
            &author,
            &committer,
            sign.then_some(settings.as_slice()),
        )?;
        replaced = Some((commit.hash.clone(), new));
    }

    if progress {
        eprintln!("\rgitenv: rewrote {} commits{:10}", todo.len(), "");
    }
    let (_, new_head) = replaced.context("nothing was rebuilt")?;
    let target = plan.branch.as_deref().unwrap_or("HEAD");
    let mut args = vec![
        "update-ref",
        "-m",
        "gitenv fix-authors: re-author unpushed commits",
    ];
    if plan.branch.is_none() {
        args.push("--no-deref");
    }
    args.extend([target, new_head.as_str(), plan.head.as_str()]);
    git_out(root, &args).with_context(|| {
        format!(
            "moving {target} failed: it changed since gitenv looked, or a hook refused; \
             nothing was changed"
        )
    })?;
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

        git(&dir, &["tag", "v1", "HEAD~1"]);
        fix(&dir).unwrap();
        assert!(plan_is_clean(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn fix_wrong_after(dir: &Path) -> Result<()> {
        commit(dir, "ada@example.com", "wrong");
        fix(dir)
    }

    #[test]
    fn refuses_when_head_moved_since_planning() {
        let dir = base("moved");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit(&dir, "ada@acme.example", "base");
        git(&dir, &["tag", "v0"]);
        commit(&dir, "ada@example.com", "wrong");
        git(&dir, &["branch", "other", "v0"]);
        let repo = git::discover(&dir).unwrap().unwrap();
        let planned = plan(&dir, &acme()).unwrap();

        git(&dir, &["switch", "-q", "other"]);
        commit(&dir, "ada@acme.example", "precious");
        let other = git(&dir, &["rev-parse", "other"]);
        let err = rewrite(&repo, &acme(), &planned).unwrap_err().to_string();
        assert!(err.contains("another branch"), "{err}");
        assert_eq!(git(&dir, &["rev-parse", "other"]), other);

        git(&dir, &["switch", "-q", "main"]);
        commit(&dir, "ada@acme.example", "made while asking");
        let main = git(&dir, &["rev-parse", "main"]);
        let err = rewrite(&repo, &acme(), &planned).unwrap_err().to_string();
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(git(&dir, &["rev-parse", "main"]), main);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn hash_object(dir: &Path, text: &str) -> String {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap().stdout;
        String::from_utf8(out).unwrap().trim().to_string()
    }

    #[test]
    fn replace_objects_are_not_baked_in_and_other_headers_are_kept() {
        let dir = base("replace");
        let real = |dir: &Path, rev: &str| {
            git_env(
                dir,
                &["cat-file", "commit", rev],
                &[("GIT_NO_REPLACE_OBJECTS", "1")],
            ) + "\n"
        };
        git(&dir, &["init", "-q", "-b", "main"]);
        commit(&dir, "ada@example.com", "a");
        commit(&dir, "ada@example.com", "b original");
        let b = git(&dir, &["rev-parse", "HEAD"]);
        commit(&dir, "ada@example.com", "c");
        let c = git(&dir, &["rev-parse", "HEAD"]);
        let fake = hash_object(&dir, &real(&dir, &b).replace("b original", "b REPLACEMENT"));
        git(&dir, &["replace", &b, &fake]);
        let tip = hash_object(
            &dir,
            &real(&dir, &c).replacen("\n\n", "\nchange-id zyxwvutsrqponmlk\n\n", 1),
        );
        git(&dir, &["update-ref", "refs/heads/main", &tip]);

        fix(&dir).unwrap();
        let log = git_env(
            &dir,
            &["log", "--format=%s %ae"],
            &[("GIT_NO_REPLACE_OBJECTS", "1")],
        );
        assert_eq!(
            log,
            "c ada@acme.example\nb original ada@acme.example\na ada@acme.example"
        );
        assert!(real(&dir, "HEAD").contains("\nchange-id zyxwvutsrqponmlk\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_branch_named_like_a_tag_and_a_file_named_head() {
        let dir = base("names");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit(&dir, "ada@acme.example", "a");
        git(&dir, &["tag", "main"]);
        commit(&dir, "ada@example.com", "b");
        std::fs::write(dir.join("HEAD"), "x").unwrap();
        let planned = plan(&dir, &acme()).unwrap();
        assert_eq!(planned.wrong.len(), 1);
        fix(&dir).unwrap();
        assert!(plan_is_clean(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn latin1_commits_and_trimmed_names_are_clean_after_one_run() {
        let dir = base("latin1");
        git(&dir, &["init", "-q"]);
        git(
            &dir,
            &[
                "-c",
                "i18n.commitEncoding=ISO-8859-1",
                "-c",
                "user.name=Bob",
                "-c",
                "user.email=bob@old.example",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        let mut jose = acme();
        jose.user_name = "José ".into();
        let repo = git::discover(&dir).unwrap().unwrap();
        let planned = plan(&dir, &jose).unwrap();
        rewrite(&repo, &jose, &planned).unwrap();
        assert!(
            plan(&dir, &jose).unwrap().wrong.is_empty(),
            "second run is a no-op"
        );
        assert_eq!(git(&dir, &["log", "-1", "--format=%an"]), "José");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
