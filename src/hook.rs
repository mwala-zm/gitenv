//! The `hook` subcommand: install `gitenv check` as pre-commit and
//! pre-push hooks, in one repository or for every repository at once.
//!
//! Hooks gitenv did not write are never overwritten. The global hooks run
//! the repository's own hook afterwards, because git skips `.git/hooks`
//! once `core.hooksPath` is set.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

const MARKER: &str = "# Installed by `gitenv hook install`.";
const NAMES: [&str; 2] = ["pre-commit", "pre-push"];

fn script(name: &str, global: bool) -> String {
    let mut text = format!(
        "#!/bin/sh\n{MARKER}\n\
         if command -v gitenv >/dev/null 2>&1; then\n\
         \tgitenv check || exit 1\n\
         else\n\
         \techo 'gitenv: not on PATH, identity not checked' >&2\n\
         fi\n"
    );
    if global {
        text.push_str(&format!(
            "# core.hooksPath makes git skip the repo's own hooks; run it here.\n\
             own=\"$(git rev-parse --git-common-dir)/hooks/{name}\"\n\
             if [ -x \"$own\" ]; then exec \"$own\" \"$@\"; fi\n"
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

/// Where hooks go: the repo's hooks directory (which honours a local
/// `core.hooksPath`), or the global `core.hooksPath`, `~/.githooks` if
/// none is set yet. The flag says whether the global setting is missing.
fn hooks_dir(dir: &Path, global: bool) -> Result<(PathBuf, bool)> {
    if global {
        return Ok(match git(dir, &["config", "--global", "core.hooksPath"])? {
            Some(path) => (crate::git::expand_tilde(Path::new(&path)), false),
            None => (default_global_dir()?, true),
        });
    }
    let Some(path) = git(dir, &["rev-parse", "--git-path", "hooks"])? else {
        bail!("{} is not inside a git repository", dir.display());
    };
    let path = PathBuf::from(path);
    Ok((
        if path.is_absolute() {
            path
        } else {
            dir.join(path)
        },
        false,
    ))
}

fn ours(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|t| t.contains(MARKER))
}

fn write_hooks(hooks: &Path, global: bool) -> Result<Vec<String>> {
    fs::create_dir_all(hooks).with_context(|| format!("creating {}", hooks.display()))?;
    let mut lines = Vec::new();
    for name in NAMES {
        let path = hooks.join(name);
        if path.exists() && !ours(&path) {
            lines.push(format!(
                "  left {} alone: it is not gitenv's; add `gitenv check || exit 1` to it",
                path.display()
            ));
            continue;
        }
        fs::write(&path, script(name, global))
            .with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
        lines.push(format!("  wrote {}", path.display()));
    }
    Ok(lines)
}

/// Remove the hooks gitenv wrote; returns what was removed.
fn remove_hooks(hooks: &Path) -> Vec<String> {
    NAMES
        .iter()
        .map(|name| hooks.join(name))
        .filter(|path| ours(path) && fs::remove_file(path).is_ok())
        .map(|path| format!("  removed {}", path.display()))
        .collect()
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
        lines.push("  each repo's own hook in .git/hooks still runs after the check".into());
    }
    Ok(lines.join("\n"))
}

pub fn remove(dir: &Path, global: bool) -> Result<String> {
    let (hooks, _) = hooks_dir(dir, global)?;
    let mut lines = remove_hooks(&hooks);
    if lines.is_empty() {
        lines.push(format!("gitenv: no gitenv hooks in {}", hooks.display()));
    } else {
        lines.insert(0, format!("gitenv: hooks in {}", hooks.display()));
    }
    // An empty ~/.githooks left as core.hooksPath would silently switch
    // off every repo's own hooks, so undo the setting install made.
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

        let out = install(&dir, false).unwrap();
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

        // Running again rewrites its own hook rather than refusing.
        assert!(install(&dir, false).unwrap().contains("wrote"));

        let out = remove(&dir, false).unwrap();
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
        // The repo's own hook runs after the check; with gitenv off PATH
        // the check is skipped with a warning instead of blocking.
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
}
