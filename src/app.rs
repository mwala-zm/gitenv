//! Application state and key handling.
//!
//! Rails analogy: the controller. `App` is the single source of truth for
//! the screen, and `handle_key` turns a key press into a state change.
//! Nothing in here draws anything.

use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::form::Form;
use crate::git::{self, Repo};
use crate::profile::{Config, Profile};

/// What the screen is doing. Popups sit on top of the normal view.
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Help,
    Edit(Form),
    /// Asking whether to delete the highlighted profile.
    ConfirmDelete,
}

pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub repo: Option<Repo>,
    /// Index into `config.profiles` of the highlighted row.
    pub selected: usize,
    /// One-line message shown in the footer after an action.
    pub status: Option<String>,
    pub mode: Mode,
    pub should_quit: bool,
}

impl App {
    /// Build the app looking at the repository that contains `dir`.
    pub fn new(dir: &Path) -> Result<Self> {
        let config_path = Config::path()?;
        let mut config = Config::load(&config_path)?;

        // First run: seed a profile from the global git identity so the
        // screen is not empty, and write the file so the user can edit it.
        if config.profiles.is_empty() && !config_path.exists() {
            let global = git::global_identity();
            if let (Some(user_name), Some(email)) = (global.name, global.email) {
                config.profiles.push(Profile {
                    name: "global".into(),
                    user_name,
                    email,
                    signing_key: global.signing_key,
                    ssh_key: None,
                });
                config.save(&config_path)?;
            }
        }

        let repo = git::discover(dir)?;

        let mut app = Self {
            config,
            config_path,
            repo,
            selected: 0,
            status: None,
            mode: Mode::Normal,
            should_quit: false,
        };
        app.select_active();
        Ok(app)
    }

    /// The profile that matches what git would use right now, if any.
    pub fn active_profile(&self) -> Option<&Profile> {
        let repo = self.repo.as_ref()?;
        self.config.matching(&repo.effective)
    }

    pub fn selected_profile(&self) -> Option<&Profile> {
        self.config.profiles.get(self.selected)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Help => self.mode = Mode::Normal,
            Mode::Edit(_) => self.handle_edit_key(key),
            Mode::ConfirmDelete => self.handle_confirm_delete_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.move_selection(1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.move_selection(-1),
            (KeyCode::Enter, _) => self.apply_selected(),
            (KeyCode::Char('r'), _) => self.refresh(),
            (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => self.mode = Mode::Help,
            (KeyCode::Char('n'), _) => self.mode = Mode::Edit(Form::new()),
            (KeyCode::Char('e'), _) => {
                if let Some(profile) = self.selected_profile() {
                    self.mode = Mode::Edit(Form::edit(self.selected, profile));
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
            // Ctrl+A and friends arrive as Char('a') with a modifier; they
            // are shortcuts, not text.
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

    /// Insert or replace a profile, then persist the config file.
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
        match target {
            Some(i) if i < self.config.profiles.len() => self.config.profiles[i] = profile,
            _ => {
                self.config.profiles.push(profile);
                self.selected = self.config.profiles.len() - 1;
            }
        }

        self.config
            .save(&self.config_path)
            .map_err(|e| format!("Could not save config: {e}"))?;
        self.status = Some(format!("Saved profile '{name}'"));
        Ok(())
    }

    /// Remove the highlighted profile and persist the config file. Repos
    /// the profile was applied to keep their `.git/config` as it is.
    fn delete_selected(&mut self) {
        if self.selected >= self.config.profiles.len() {
            return;
        }
        let removed = self.config.profiles.remove(self.selected);
        self.selected = self
            .selected
            .min(self.config.profiles.len().saturating_sub(1));
        self.status = Some(match self.config.save(&self.config_path) {
            Ok(()) => format!("Deleted profile '{}'", removed.name),
            Err(e) => format!("Could not save config: {e}"),
        });
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
                self.status = Some(format!("Applied profile '{}' to this repo", profile.name));
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

    /// Move the highlight onto the profile that is currently active.
    fn select_active(&mut self) {
        if let Some(active) = self.active_profile()
            && let Some(idx) = self.config.profiles.iter().position(|p| p == active)
        {
            self.selected = idx;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tag` keeps each test on its own file; tests run in parallel.
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
        }
    }

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.into(),
            user_name: "Ada".into(),
            email: "ada@example.com".into(),
            signing_key: None,
            ssh_key: None,
        }
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

        // Nothing left: a further delete is a no-op rather than a panic.
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
        app.mode = Mode::Edit(Form::new());
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT));
        let Mode::Edit(form) = &app.mode else {
            panic!("should still be editing");
        };
        assert_eq!(form.values[0], "bC");
    }
}
