use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// When and how big each watched file was, or `None` while it is missing,
/// so creating or deleting one counts as a change too.
pub type Stamps = Vec<Option<(SystemTime, u64)>>;

pub fn stamps(paths: &[PathBuf]) -> Stamps {
    paths
        .iter()
        .map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            Some((meta.modified().ok()?, meta.len()))
        })
        .collect()
}

/// The files whose change alters what gitenv shows for the repository at
/// `root`: its config and worktree config, which say who git commits as,
/// HEAD and its reflog, which move with every commit and checkout, and the
/// global git config files.
pub fn repo_files(root: &Path) -> Vec<PathBuf> {
    let names = ["config", "config.worktree", "HEAD", "logs/HEAD"];
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("rev-parse");
    for name in names {
        cmd.args(["--git-path", name]);
    }
    let mut files: Vec<PathBuf> = match cmd.output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| {
                let path = PathBuf::from(l);
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            })
            .collect(),
        _ => Vec::new(),
    };
    files.extend(global_files());
    files
}

fn global_files() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("GIT_CONFIG_GLOBAL") {
        return vec![PathBuf::from(path)];
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    home.map(|h| h.join(".gitconfig"))
        .into_iter()
        .chain(xdg.map(|x| x.join("git/config")))
        .collect()
}

/// What the TUI last saw on disk, so it can reload when something else
/// changes the profiles file or the repository it shows.
#[derive(Debug, Default, Clone)]
pub struct Watch {
    pub repo_root: Option<PathBuf>,
    pub repo_files: Vec<PathBuf>,
    pub repo_stamps: Stamps,
    pub config_tried: Option<Option<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_change_when_a_file_appears_or_grows() {
        let dir = std::env::temp_dir().join(format!("gitenv-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config");
        let before = stamps(std::slice::from_ref(&file));
        assert_eq!(before, [None]);
        std::fs::write(&file, "a").unwrap();
        let written = stamps(std::slice::from_ref(&file));
        assert_ne!(written, before);
        std::fs::write(&file, "ab").unwrap();
        assert_ne!(stamps(std::slice::from_ref(&file)), written);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn repo_files_point_into_the_git_dir() {
        let dir = std::env::temp_dir().join(format!("gitenv-watch-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        let files = repo_files(&dir);
        assert!(files[0].ends_with(".git/config"), "{files:?}");
        assert!(files[3].ends_with(".git/logs/HEAD"), "{files:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
