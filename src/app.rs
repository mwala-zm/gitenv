use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::export;
use crate::form::Form;
use crate::git::{self, Repo};
use crate::profile::{Config, Profile};
use crate::rules::{self, Reason};
use crate::scan::{self, Row};

#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Help,
    Edit(Box<Form>),
    ConfirmDelete,
    Scan(Box<ScanView>),
}

#[derive(Debug, Clone)]
pub struct ScanView {
    pub root: PathBuf,
    pub rows: Vec<Row>,
    pub selected: usize,
}

pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub repo: Option<Repo>,
    pub selected: usize,
    pub status: Option<String>,
    pub mode: Mode,
    pub should_quit: bool,
    pub scan_root: PathBuf,
    pub config_text: Option<String>,
}

impl App {
    /// Loads the profiles and the repository containing `dir`. On first run it
    /// seeds a profile from the global git identity and writes the file,
    /// showing rather than failing on a write error. Started outside a
    /// repository, it opens on the scan of `dir`.
    pub fn new(dir: &Path) -> Result<Self> {
        let config_path = Config::path()?;
        let mut config = Config::load(&config_path)?;

        let mut status = None;
        if config.profiles.is_empty() && !config_path.exists() {
            let global = git::global_identity();
            if let (Some(user_name), Some(email)) = (global.name, global.email) {
                config.profiles.push(Profile {
                    name: "global".into(),
                    user_name,
                    email,
                    signing_key: global.signing_key,
                    ..Default::default()
                });
                if let Err(err) = config.save(&config_path) {
                    status = Some(format!(
                        "Could not create {}: {}",
                        config_path.display(),
                        err.root_cause()
                    ));
                }
            }
        }
        let config_text = std::fs::read_to_string(&config_path).ok();

        let repo = git::discover(dir)?;
        let scan_root = match &repo {
            Some(repo) => repo.root.parent().unwrap_or(&repo.root).to_path_buf(),
            None => dir.to_path_buf(),
        };
        let started_outside = repo.is_none();

        let mut app = Self {
            config,
            config_path,
            repo,
            selected: 0,
            status,
            mode: Mode::Normal,
            should_quit: false,
            scan_root,
            config_text,
        };
        app.select_active();
        if started_outside {
            let status = app.status.take();
            app.open_scan();
            app.status = status;
        }
        Ok(app)
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        let repo = self.repo.as_ref()?;
        self.config.matching(&repo.effective)
    }

    pub fn expected_profile(&self) -> Option<(&Profile, Reason)> {
        let repo = self.repo.as_ref()?;
        rules::expected(&self.config, repo)
    }

    pub fn reference_email(&self) -> Option<&str> {
        match self.expected_profile() {
            Some((p, _)) => Some(p.email.as_str()),
            None => self.repo.as_ref()?.effective.email.as_deref(),
        }
    }

    pub fn selected_profile(&self) -> Option<&Profile> {
        self.config.profiles.get(self.selected)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if matches!(self.mode, Mode::Normal | Mode::Scan(_)) {
            self.status = None;
        }
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Help => self.mode = Mode::Normal,
            Mode::Edit(_) => self.handle_edit_key(key),
            Mode::ConfirmDelete => self.handle_confirm_delete_key(key),
            Mode::Scan(_) => self.handle_scan_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.move_selection(1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.move_selection(-1),
            (KeyCode::Enter, _) => self.apply_selected(),
            (KeyCode::Char('r'), _) => {
                self.reload_config();
                self.refresh();
            }
            (KeyCode::Char('s'), _) => self.open_scan(),
            (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => self.mode = Mode::Help,
            (KeyCode::Char('n'), _) => self.mode = Mode::Edit(Box::new(Form::new())),
            (KeyCode::Char('e'), _) => {
                if let Some(profile) = self.selected_profile() {
                    self.mode = Mode::Edit(Box::new(Form::edit(self.selected, profile)));
                } else {
                    self.status = Some("No profile to edit; press n to create one".into());
                }
            }
            (KeyCode::Char('d'), _) => {
                if self.selected_profile().is_some() {
                    self.mode = Mode::ConfirmDelete;
                } else {
                    self.status = Some("No profile to delete".into());
                }
            }
            _ => {}
        }
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
        self.mode = Mode::Normal;
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
            self.delete_selected();
        } else {
            self.status = Some("Delete cancelled".into());
        }
    }

    /// Keys in the profile form. Ctrl and Alt chords arrive as characters with
    /// a modifier and are shortcuts, never typed into the field.
    fn handle_edit_key(&mut self, key: KeyEvent) {
        let Mode::Edit(form) = &mut self.mode else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.mode = Mode::Normal;
            }
            (KeyCode::Tab, _) | (KeyCode::Down, _) => form.next_field(),
            (KeyCode::BackTab, _) | (KeyCode::Up, _) => form.prev_field(),
            (KeyCode::Backspace, _) => form.backspace(),
            (KeyCode::Delete, _) => form.delete(),
            (KeyCode::Left, _) => form.left(),
            (KeyCode::Right, _) => form.right(),
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => form.home(),
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => form.end(),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => form.kill_to_start(),
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => form.kill_to_end(),
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => form.kill_word(),
            (KeyCode::Char(c), m)
                if !m.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                form.insert(c)
            }
            (KeyCode::Enter, _) => {
                let target = form.editing;
                let outcome = match form.to_profile() {
                    Ok(profile) => self.save_profile(target, profile),
                    Err(err) => Err(err),
                };
                match outcome {
                    Ok(()) => self.mode = Mode::Normal,
                    Err(err) => {
                        if let Mode::Edit(form) = &mut self.mode {
                            form.error = Some(err);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Writes `config` and only then shows it, so a failed save leaves nothing
    /// on screen that is not on disk. Refuses when something else changed the
    /// file since gitenv read it.
    fn save_config(&mut self, config: Config) -> Result<(), String> {
        let on_disk = std::fs::read_to_string(&self.config_path).ok();
        if on_disk != self.config_text {
            return Err(
                "The profiles file changed on disk since gitenv read it, so nothing was saved. \
                 Press r to reload it (Esc first to leave the form)"
                    .into(),
            );
        }
        config
            .save(&self.config_path)
            .map_err(|e| format!("Could not save config: {}", e.root_cause()))?;
        self.config = config;
        self.config_text = std::fs::read_to_string(&self.config_path).ok();
        Ok(())
    }

    fn reload_config(&mut self) {
        match Config::load(&self.config_path) {
            Ok(config) => {
                self.config = config;
                self.config_text = std::fs::read_to_string(&self.config_path).ok();
                self.selected = self
                    .selected
                    .min(self.config.profiles.len().saturating_sub(1));
            }
            Err(err) => self.status = Some(format!("Could not read config: {err:#}")),
        }
    }

    fn save_profile(&mut self, target: Option<usize>, profile: Profile) -> Result<(), String> {
        let clash = self
            .config
            .profiles
            .iter()
            .enumerate()
            .any(|(i, p)| Some(i) != target && p.name == profile.name);
        if clash {
            return Err(format!("A profile named '{}' already exists", profile.name));
        }

        let name = profile.name.clone();
        let mut config = self.config.clone();
        let mut selected = self.selected;
        match target {
            Some(i) if i < config.profiles.len() => config.profiles[i] = profile,
            _ => {
                config.profiles.push(profile);
                selected = config.profiles.len() - 1;
            }
        }
        self.save_config(config)?;
        self.selected = selected;
        self.status = Some(format!("Saved profile '{name}'{}", self.refresh_export()));
        Ok(())
    }

    fn delete_selected(&mut self) {
        if self.selected >= self.config.profiles.len() {
            return;
        }
        let mut config = self.config.clone();
        let removed = config.profiles.remove(self.selected);
        self.status = Some(match self.save_config(config) {
            Ok(()) => {
                self.selected = self
                    .selected
                    .min(self.config.profiles.len().saturating_sub(1));
                format!(
                    "Deleted profile '{}'{}",
                    removed.name,
                    self.refresh_export()
                )
            }
            Err(e) => e,
        });
    }

    fn scan_rows(&self, root: &Path) -> Result<Vec<Row>> {
        let opts = scan::Options {
            problems_only: false,
            fix: false,
        };
        scan::scan(&self.config, root, &opts).map(|(rows, _)| rows)
    }

    fn open_scan(&mut self) {
        let root = self.scan_root.clone();
        match self.scan_rows(&root) {
            Ok(rows) if rows.is_empty() => {
                self.status = Some(format!("No repositories under {}", scan::tildify(&root)));
            }
            Ok(rows) => {
                self.mode = Mode::Scan(Box::new(ScanView {
                    root,
                    rows,
                    selected: 0,
                }))
            }
            Err(err) => self.status = Some(format!("Could not scan: {err}")),
        }
    }

    fn rescan(&mut self) {
        let Mode::Scan(view) = &self.mode else {
            return;
        };
        let root = view.root.clone();
        let rows = self.scan_rows(&root);
        if let Mode::Scan(view) = &mut self.mode {
            match rows {
                Ok(rows) => {
                    view.selected = view.selected.min(rows.len().saturating_sub(1));
                    view.rows = rows;
                }
                Err(err) => self.status = Some(format!("Could not scan: {err}")),
            }
        }
    }

    fn handle_scan_key(&mut self, key: KeyEvent) {
        let Mode::Scan(view) = &mut self.mode else {
            return;
        };
        let len = view.rows.len().max(1);
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true
            }
            (KeyCode::Esc, _) | (KeyCode::Char('s'), _) => self.mode = Mode::Normal,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                view.selected = (view.selected + 1) % len
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                view.selected = (view.selected + len - 1) % len
            }
            (KeyCode::Char('r'), _) => self.rescan(),
            (KeyCode::Char('o'), _) => {
                let Some(row) = view.rows.get(view.selected) else {
                    return;
                };
                let root = row.repo.root.clone();
                match git::discover(&root) {
                    Ok(Some(repo)) => {
                        self.repo = Some(repo);
                        self.mode = Mode::Normal;
                        self.select_active();
                        self.status = Some(format!("Opened {}", scan::tildify(&root)));
                    }
                    _ => self.status = Some(format!("Could not read {}", root.display())),
                }
            }
            (KeyCode::Enter, _) => {
                let Some(row) = view.rows.get(view.selected) else {
                    return;
                };
                let repo = row.repo.clone();
                self.status = Some(match rules::expected(&self.config, &repo) {
                    None => "No rule claims this repo; press o to open it and pick a profile"
                        .to_string(),
                    Some((profile, _)) => match git::apply(&repo, profile) {
                        Ok(()) => format!(
                            "Applied profile '{}' to {}",
                            profile.name,
                            scan::tildify(&repo.root)
                        ),
                        Err(err) => format!("Failed to apply: {err}"),
                    },
                });
                self.rescan();
                if self.repo.as_ref().is_some_and(|r| r.root == repo.root) {
                    self.refresh();
                }
            }
            _ => {}
        }
    }

    fn refresh_export(&self) -> String {
        match export::refresh(&self.config, &self.config_path) {
            Ok(true) => " and updated the git includes".into(),
            Ok(false) => String::new(),
            Err(e) => format!(", but could not update the git includes: {e}"),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.config.profiles.len();
        if len == 0 {
            return;
        }
        let next = (self.selected as isize + delta).rem_euclid(len as isize);
        self.selected = next as usize;
    }

    fn apply_selected(&mut self) {
        let Some(repo) = &self.repo else {
            self.status = Some("Not inside a git repository".into());
            return;
        };
        let Some(profile) = self.selected_profile() else {
            self.status = Some("No profile selected".into());
            return;
        };
        match git::apply(repo, profile) {
            Ok(()) => {
                self.status = Some(match git::still_overridden(&repo.root, profile) {
                    Some(why) => format!("Applied profile '{}', but {why}", profile.name),
                    None => format!("Applied profile '{}' to this repo", profile.name),
                });
                self.refresh();
            }
            Err(err) => self.status = Some(format!("Failed to apply: {err}")),
        }
    }

    fn refresh(&mut self) {
        if let Some(repo) = &self.repo
            && let Ok(Some(fresh)) = git::discover(&repo.root)
        {
            self.repo = Some(fresh);
        }
    }

    fn select_active(&mut self) {
        let target = self
            .expected_profile()
            .map(|(p, _)| p)
            .or_else(|| self.active_profile());
        if let Some(target) = target
            && let Some(idx) = self.config.profiles.iter().position(|p| p == target)
        {
            self.selected = idx;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(tag: &str, profiles: Vec<Profile>) -> App {
        App {
            config: Config { profiles },
            config_path: std::env::temp_dir()
                .join(format!("gitenv-app-test-{}-{tag}.toml", std::process::id())),
            repo: None,
            selected: 0,
            status: None,
            mode: Mode::Normal,
            should_quit: false,
            scan_root: std::env::temp_dir(),
            config_text: None,
        }
    }

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.into(),
            user_name: "Ada".into(),
            email: "ada@example.com".into(),
            ..Default::default()
        }
    }

    #[test]
    fn startup_highlights_the_profile_a_rule_claims() {
        let mut acme = profile("acme");
        acme.email = "ada@acme.example".into();
        acme.remotes = vec!["github.com/acme/*".into()];
        let mut app = app_with("rule", vec![profile("personal"), acme]);
        app.repo = Some(Repo {
            root: "/work/acme/api".into(),
            remote: Some("git@github.com:acme/api.git".into()),
            effective: git::Identity {
                name: Some("Ada".into()),
                email: Some("ada@example.com".into()),
                ..Default::default()
            },
            local: Default::default(),
            authors: Vec::new(),
        });
        app.select_active();
        assert_eq!(app.selected, 1);
        assert_eq!(
            app.active_profile().map(|p| p.name.as_str()),
            Some("personal")
        );
    }

    #[test]
    fn new_profile_is_appended_and_selected() {
        let mut app = app_with("append", vec![profile("a")]);
        app.save_profile(None, profile("b")).unwrap();
        assert_eq!(app.config.profiles.len(), 2);
        assert_eq!(app.selected, 1);
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn duplicate_name_is_rejected_unless_editing_itself() {
        let mut app = app_with("duplicate", vec![profile("a"), profile("b")]);
        assert!(app.save_profile(None, profile("a")).is_err());
        assert!(app.save_profile(Some(1), profile("a")).is_err());
        assert!(app.save_profile(Some(0), profile("a")).is_ok());
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn delete_removes_profile_and_keeps_selection_in_range() {
        let mut app = app_with("delete", vec![profile("a"), profile("b")]);
        app.selected = 1;
        app.delete_selected();
        assert_eq!(app.config.profiles.len(), 1);
        assert_eq!(app.config.profiles[0].name, "a");
        assert_eq!(app.selected, 0);

        app.delete_selected();
        assert!(app.config.profiles.is_empty());
        assert_eq!(app.selected, 0);

        app.delete_selected();
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn confirm_delete_only_on_yes_or_enter() {
        let mut app = app_with("confirm", vec![profile("a")]);
        app.mode = Mode::ConfirmDelete;
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(app.config.profiles.len(), 1);
        assert!(matches!(app.mode, Mode::Normal));

        app.mode = Mode::ConfirmDelete;
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(app.config.profiles.is_empty());
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn control_chords_do_not_type_into_the_form() {
        let mut app = app_with("chords", vec![]);
        app.mode = Mode::Edit(Box::new(Form::new()));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT));
        let Mode::Edit(form) = &app.mode else {
            panic!("should still be editing");
        };
        assert_eq!(form.values[0], "bC");
    }

    #[test]
    fn scan_view_applies_the_claimed_profile_and_opens_repos() {
        let base = std::env::temp_dir().join(format!("gitenv-app-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let git = |dir: &Path, args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        for (name, remote) in [
            ("api", "git@github.com:acme/api.git"),
            ("toy", "git@github.com:ada/toy.git"),
        ] {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q"]);
            git(&dir, &["remote", "add", "origin", remote]);
            git(&dir, &["config", "--local", "user.name", "Ada"]);
            git(
                &dir,
                &["config", "--local", "user.email", "ada@example.com"],
            );
        }
        let mut acme = profile("acme");
        acme.email = "ada@acme.example".into();
        acme.remotes = vec!["github.com/acme/*".into()];
        let mut app = app_with("scan", vec![profile("personal"), acme]);
        app.scan_root = base.clone();

        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        let Mode::Scan(view) = &app.mode else {
            panic!("s should open the scan view: {:?}", app.status);
        };
        assert_eq!(view.rows.len(), 2);
        assert_eq!(view.rows[0].status_word(), "wrong");

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let Mode::Scan(view) = &app.mode else {
            panic!("still scanning");
        };
        assert_eq!(view.rows[0].status_word(), "ok", "{:?}", app.status);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Applied profile 'acme'")
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.status.as_deref().unwrap().starts_with("No rule claims"));

        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.repo.as_ref().unwrap().root.file_name().unwrap(), "toy");
        assert_eq!(app.active_profile().unwrap().name, "personal");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_failed_save_changes_nothing_on_screen() {
        let mut app = app_with("failsave", vec![profile("a")]);
        std::fs::create_dir_all(&app.config_path).unwrap();
        app.config_text = std::fs::read_to_string(&app.config_path).ok();
        assert!(app.save_profile(None, profile("b")).is_err());
        assert_eq!(app.config.profiles.len(), 1);
        app.delete_selected();
        assert_eq!(app.config.profiles.len(), 1);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Could not save config")
        );
        std::fs::remove_dir_all(&app.config_path).unwrap();
    }

    #[test]
    fn edits_made_elsewhere_are_not_overwritten() {
        let mut app = app_with("elsewhere", vec![profile("a")]);
        app.save_profile(Some(0), profile("a")).unwrap();
        let mut text = std::fs::read_to_string(&app.config_path).unwrap();
        text.push_str("\n# added in my editor\n[[profiles]]\nname = \"client\"\nuser_name = \"C\"\nemail = \"c@x\"\n");
        std::fs::write(&app.config_path, &text).unwrap();

        let err = app.save_profile(None, profile("b")).unwrap_err();
        assert!(err.contains("changed on disk"), "{err}");
        assert_eq!(std::fs::read_to_string(&app.config_path).unwrap(), text);

        app.handle_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(app.config.profiles.len(), 2);
        app.save_profile(None, profile("b")).unwrap();
        let saved = std::fs::read_to_string(&app.config_path).unwrap();
        assert!(saved.contains("# added in my editor") && saved.contains("name = \"b\""));
        std::fs::remove_file(&app.config_path).ok();
    }
}
