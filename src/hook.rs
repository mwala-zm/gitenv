use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

const MARKER: &str = "# Installed by `gitenv hook install`.";
const CHECKED: [&str; 2] = ["pre-commit", "pre-push"];
const ALL: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
];

/// The hook's text. With `global` it hands over to the repo's own hook of the
/// same name, unless that is this very file (`-ef`), as when the global hooks
/// path is some repo's own hooks folder.
fn script(name: &str, global: bool) -> String {
    script_for(name, CHECKED.contains(&name), global)
}

fn script_for(name: &str, check: bool, global: bool) -> String {
    let mut text = format!("#!/bin/sh\n{MARKER}\n");
    if check {
        text.push_str(
            "if command -v gitenv >/dev/null 2>&1; then\n\
             \tgitenv check || exit 1\n\
             else\n\
             \techo 'gitenv: not on PATH, identity not checked' >&2\n\
             fi\n",
        );
    }
    if global {
        text.push_str(&format!(
            "# core.hooksPath makes git skip the repo's own hooks; run it here.\n\
             own=\"$(git rev-parse --git-common-dir)/hooks/{name}\"\n\
             if [ -x \"$own\" ] && ! [ \"$own\" -ef \"$0\" ]; then exec \"$own\" \"$@\"; fi\n"
        ));
    }
    text
}

fn git(dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git; is it installed and on PATH?")?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((output.status.success() && !text.is_empty()).then_some(text))
}

fn default_global_dir() -> Result<PathBuf> {
    let dirs = directories::UserDirs::new().context("could not find your home directory")?;
    Ok(dirs.home_dir().join(".githooks"))
}

/// The repo's hooks directory, which honours a local core.hooksPath, or the
/// global core.hooksPath, `~/.githooks` if none is set yet; the flag says it is
/// not set. A relative global path is refused, since git reads it relative to
/// each repository.
fn hooks_dir(dir: &Path, global: bool) -> Result<(PathBuf, bool)> {
    if global {
        return match git(dir, &["config", "--global", "core.hooksPath"])? {
            Some(path) => {
                let expanded = crate::git::expand_tilde(Path::new(&path));
                if !expanded.is_absolute() {
                    bail!(
                        "your global core.hooksPath is '{path}', which git reads relative to \
                         each repository; set it to an absolute path first"
                    );
                }
                Ok((expanded, false))
            }
            None => Ok((default_global_dir()?, true)),
        };
    }
    let Some(path) = git(dir, &["rev-parse", "--git-path", "hooks"])? else {
        bail!("{} is not inside a git repository", dir.display());
    };
    if git(dir, &["config", "--local", "core.hooksPath"])?.is_none()
        && let Some(global_path) = git(dir, &["config", "--global", "core.hooksPath"])?
    {
        bail!(
            "your global core.hooksPath ({global_path}) makes git run hooks from there for \
             every repository; use `gitenv hook install --global` or `remove --global`"
        );
    }
    Ok((std::path::absolute(dir.join(path))?, false))
}

#[derive(Debug, PartialEq, Eq)]
enum State {
    Missing,
    Ours,
    Edited,
    Foreign,
}

fn state(path: &Path, name: &str) -> State {
    match fs::read_to_string(path) {
        Err(_) if !path.exists() => State::Missing,
        Ok(text)
            if [(true, true), (true, false), (false, true)]
                .iter()
                .any(|&(check, global)| text == script_for(name, check, global)) =>
        {
            State::Ours
        }
        Ok(text) if text.contains(MARKER) => State::Edited,
        _ => State::Foreign,
    }
}

/// Writes the hooks, never replacing one gitenv did not write or that was
/// edited since. git skips each repo's own `.git/hooks` once core.hooksPath is
/// set, so `global` also writes a hook for every other event that only hands
/// over to the repo's own. Left out: reference-transaction and
/// post-index-change, which run on nearly every git command, and
/// push-to-checkout, whose mere presence replaces git's own work tree update
/// for `receive.denyCurrentBranch=updateInstead`.
fn write_hooks(hooks: &Path, global: bool) -> Result<Vec<String>> {
    fs::create_dir_all(hooks).with_context(|| format!("creating {}", hooks.display()))?;
    let names: &[&str] = if global { ALL } else { &CHECKED };
    let mut lines = Vec::new();
    let mut handovers = 0;
    for &name in names {
        let path = hooks.join(name);
        match state(&path, name) {
            State::Missing | State::Ours => {}
            State::Edited => {
                lines.push(format!(
                    "  left {} alone: it was edited since gitenv wrote it",
                    path.display()
                ));
                continue;
            }
            State::Foreign if CHECKED.contains(&name) => {
                lines.push(format!(
                    "  left {} alone: it is not gitenv's; add `gitenv check || exit 1` to it",
                    path.display()
                ));
                continue;
            }
            State::Foreign => {
                lines.push(format!(
                    "  left {} alone: it is not gitenv's, so repos' own {name} hooks will not run",
                    path.display()
                ));
                continue;
            }
        }
        fs::write(&path, script(name, global))
            .with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
        if CHECKED.contains(&name) {
            lines.push(format!("  wrote {}", path.display()));
        } else {
            handovers += 1;
        }
    }
    if handovers > 0 {
        lines.push(format!(
            "  wrote {handovers} more that only run each repo's own hook of that name"
        ));
    }
    Ok(lines)
}

fn remove_hooks(hooks: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let mut removed = 0;
    for &name in ALL {
        let path = hooks.join(name);
        match state(&path, name) {
            State::Ours if fs::remove_file(&path).is_ok() => {
                if CHECKED.contains(&name) {
                    lines.push(format!("  removed {}", path.display()));
                } else {
                    removed += 1;
                }
            }
            State::Edited => lines.push(format!(
                "  left {}: it was edited since gitenv wrote it",
                path.display()
            )),
            _ => {}
        }
    }
    if removed > 0 {
        lines.push(format!(
            "  removed {removed} more that ran each repo's own hook"
        ));
    }
    lines
}

fn leftovers(hooks: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(hooks) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| {
            !ALL.contains(&name.as_str()) || state(&hooks.join(name), name) != State::Ours
        })
        .collect();
    names.sort();
    names
}

/// With the global hooks folder staying in use, drops the identity check but
/// keeps every hook handing over to the repo's own, since git would
/// otherwise run none of them.
fn keep_handovers(hooks: &Path) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for &name in &CHECKED {
        let path = hooks.join(name);
        if state(&path, name) == State::Ours {
            fs::write(&path, script_for(name, false, true))
                .with_context(|| format!("writing {}", path.display()))?;
            lines.push(format!(
                "  removed the identity check from {}",
                path.display()
            ));
        }
    }
    Ok(lines)
}

pub fn install(dir: &Path, global: bool) -> Result<String> {
    let (hooks, unset) = hooks_dir(dir, global)?;
    let mut lines = vec![format!("gitenv: hooks in {}", hooks.display())];
    lines.extend(write_hooks(&hooks, global)?);
    if unset {
        let status = Command::new("git")
            .args(["config", "--global", "core.hooksPath"])
            .arg(&hooks)
            .status()
            .context("running git config --global")?;
        if !status.success() {
            bail!("git config --global core.hooksPath failed");
        }
        lines.push(format!(
            "  set core.hooksPath to {} in your global git config",
            hooks.display()
        ));
    }
    if global {
        lines.push("  every other hook in each repo's .git/hooks still runs".into());
    }
    Ok(lines.join("\n"))
}

/// Removes the hooks gitenv wrote that nobody edited. An empty `~/.githooks`
/// left as the global core.hooksPath would switch off every repo's own hooks,
/// so the setting install made is undone.
pub fn remove(dir: &Path, global: bool) -> Result<String> {
    let (hooks, _) = hooks_dir(dir, global)?;
    let staying = if global && hooks == default_global_dir()? {
        leftovers(&hooks)
    } else {
        Vec::new()
    };
    if !staying.is_empty() {
        let mut lines = vec![format!("gitenv: hooks in {}", hooks.display())];
        lines.extend(keep_handovers(&hooks)?);
        lines.push(format!(
            "  core.hooksPath stays set because {} {} still there, so the hooks that run each \
             repo's own hooks were kept; remove {} and run this again to undo the rest",
            staying.join(", "),
            if staying.len() == 1 { "is" } else { "are" },
            if staying.len() == 1 { "it" } else { "them" },
        ));
        return Ok(lines.join("\n"));
    }
    let mut lines = remove_hooks(&hooks);
    if lines.is_empty() {
        lines.push(format!("gitenv: no gitenv hooks in {}", hooks.display()));
    } else {
        lines.insert(0, format!("gitenv: hooks in {}", hooks.display()));
    }
    let empty = fs::read_dir(&hooks).is_ok_and(|mut d| d.next().is_none());
    if global && empty && hooks == default_global_dir()? {
        let status = Command::new("git")
            .args(["config", "--global", "--unset", "core.hooksPath"])
            .status()
            .context("running git config --global --unset")?;
        if status.success() {
            fs::remove_dir(&hooks).ok();
            lines.push("  unset core.hooksPath in your global git config".into());
        }
    }
    Ok(lines.join("\n"))
}

pub fn run(result: Result<String>) -> ExitCode {
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
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

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitenv-hook-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    #[test]
    fn installs_into_the_repo_and_leaves_foreign_hooks_alone() {
        let dir = repo("local");
        let hooks = dir.join(".git/hooks");
        fs::write(hooks.join("pre-push"), "#!/bin/sh\nmake test\n").unwrap();

        let out = write_hooks(&hooks, false).unwrap().join("\n");
        assert!(out.contains("wrote"), "{out}");
        assert!(out.contains("left") && out.contains("pre-push"), "{out}");
        let pre_commit = fs::read_to_string(hooks.join("pre-commit")).unwrap();
        assert!(pre_commit.contains("gitenv check || exit 1"));
        assert!(
            !pre_commit.contains("exec"),
            "a local hook must not run itself"
        );
        assert_eq!(
            fs::read_to_string(hooks.join("pre-push")).unwrap(),
            "#!/bin/sh\nmake test\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(hooks.join("pre-commit"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }

        assert!(
            write_hooks(&hooks, false)
                .unwrap()
                .join("\n")
                .contains("wrote")
        );

        let out = remove_hooks(&hooks).join("\n");
        assert!(out.contains("removed"), "{out}");
        assert!(!hooks.join("pre-commit").exists());
        assert!(hooks.join("pre-push").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn global_hooks_hand_over_to_the_repo_hook() {
        let text = script("pre-push", true);
        assert!(text.contains("hooks/pre-push\""), "{text}");
        assert!(text.contains("exec \"$own\" \"$@\""), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn hook_scripts_run_under_sh() {
        let dir = repo("run");
        let hooks = dir.join("global-hooks");
        write_hooks(&hooks, true).unwrap();
        fs::write(
            dir.join(".git/hooks/pre-commit"),
            "#!/bin/sh\necho own-hook-ran\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.join(".git/hooks/pre-commit"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let output = Command::new(hooks.join("pre-commit"))
            .current_dir(&dir)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "own-hook-ran"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("not on PATH"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    fn run_with_timeout(cmd: &mut Command) -> std::process::Output {
        use std::os::unix::process::CommandExt;
        let mut child = cmd
            .process_group(0)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = Command::new("kill")
            .args(["-9", &format!("-{}", child.id())])
            .status();
        let _ = child.wait();
        panic!("hook did not finish: it is probably running itself");
    }

    #[cfg(unix)]
    #[test]
    fn a_global_hook_in_the_repos_own_folder_does_not_run_itself() {
        let dir = repo("loop");
        let hooks = dir.join(".git/hooks");
        write_hooks(&hooks, true).unwrap();
        let output = run_with_timeout(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "core.hooksPath=.git/hooks", "-c", "user.name=A"])
                .args(["-c", "user.email=a@example.com", "commit", "-q"])
                .args(["--allow-empty", "-m", "x"])
                .env("PATH", "/usr/bin:/bin"),
        );
        assert!(output.status.success(), "{output:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn global_hooks_hand_over_every_kind_of_repo_hook() {
        let dir = repo("handover");
        let global = dir.join("global-hooks");
        write_hooks(&global, true).unwrap();
        let commit_msg = dir.join(".git/hooks/commit-msg");
        fs::write(&commit_msg, "#!/bin/sh\necho 'Change-Id: I123' >> \"$1\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&commit_msg, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = run_with_timeout(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .arg("-c")
                .arg(format!("core.hooksPath={}", global.display()))
                .args(["-c", "user.name=A", "-c", "user.email=a@example.com"])
                .args(["commit", "-q", "--allow-empty", "-m", "one"])
                .env("PATH", "/usr/bin:/bin"),
        );
        assert!(output.status.success(), "{output:?}");
        let message = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["log", "-1", "--format=%B"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&message.stdout).contains("Change-Id: I123"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_hooks_are_neither_replaced_nor_removed() {
        let dir = repo("edited");
        let hooks = dir.join(".git/hooks");
        let hook = hooks.join("pre-commit");
        write_hooks(&hooks, false).unwrap();
        let mut text = fs::read_to_string(&hook).unwrap();
        text.push_str("make lint || exit 1\n");
        fs::write(&hook, &text).unwrap();

        let out = write_hooks(&hooks, false).unwrap().join("\n");
        assert!(out.contains("edited since gitenv wrote it"), "{out}");
        assert_eq!(fs::read_to_string(&hook).unwrap(), text);
        let out = remove_hooks(&hooks).join("\n");
        assert!(out.contains("edited since gitenv wrote it"), "{out}");
        assert!(hook.exists());
        assert!(
            !dir.join(".git/hooks/pre-push").exists(),
            "the untouched one went"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_or_foreign_files_keep_the_handovers_in_place() {
        let dir = repo("keep");
        let hooks = dir.join("global-hooks");
        write_hooks(&hooks, true).unwrap();
        assert!(leftovers(&hooks).is_empty());
        let mut text = fs::read_to_string(hooks.join("pre-push")).unwrap();
        text.push_str("# mine\n");
        fs::write(hooks.join("pre-push"), &text).unwrap();
        fs::write(hooks.join("my-notes.txt"), "x").unwrap();
        assert_eq!(leftovers(&hooks), vec!["my-notes.txt", "pre-push"]);

        keep_handovers(&hooks).unwrap();
        let pre_commit = fs::read_to_string(hooks.join("pre-commit")).unwrap();
        assert!(!pre_commit.contains("gitenv check"));
        assert!(pre_commit.contains("exec \"$own\""));
        assert_eq!(state(&hooks.join("pre-commit"), "pre-commit"), State::Ours);
        assert_eq!(fs::read_to_string(hooks.join("pre-push")).unwrap(), text);
        assert!(hooks.join("commit-msg").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
