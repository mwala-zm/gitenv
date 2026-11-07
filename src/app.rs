use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cmd;
use crate::export;
use crate::filter::{self, Filter};
use crate::fix;
use crate::form::Form;
use crate::git::{self, Repo};
use crate::profile::{Config, Profile};
use crate::rules::{self, Reason};
use crate::scan::{self, Row};
use crate::theme::Theme;
use crate::watch::{self, Watch};

#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Help,
    Edit(Box<Form>),
    ConfirmDelete,
    Scan(Box<ScanView>),
    FixAuthors(Box<FixView>),
}

/// Work the main loop runs between frames, so it can hand the terminal
/// back first when the work may ask for a passphrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    FixAuthors,
}

#[derive(Debug, Clone)]
pub struct FixView {
    pub profile: Profile,
    pub why: String,
    pub plan: fix::Plan,
    pub ticked: Vec<bool>,
    pub cursor: usize,
    pub blocked: Option<String>,
    pub signs: bool,
    pub confirming: bool,
}

impl FixView {
    pub fn chosen(&self) -> Vec<String> {
        self.plan
            .wrong
            .iter()
            .zip(&self.ticked)
            .filter(|(_, t)| **t)
            .map(|(c, _)| c.hash.clone())
            .collect()
    }

    /// How many unpushed commits get a new hash: every one from the first
    /// ticked commit on.
    pub fn rebuilt(&self) -> usize {
        let chosen = self.chosen();
        self.plan
            .unpushed
            .iter()
            .position(|c| chosen.contains(&c.hash))
            .map_or(0, |start| self.plan.unpushed.len() - start)
    }
}

#[derive(Debug, Clone)]
pub struct ScanView {
    pub root: PathBuf,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub filter: Filter,
}

impl ScanView {
    /// The rows the filter lets through, matched on the path shown and the
    /// profile column.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| {
                let row = &self.rows[i];
                self.filter.matches(&format!(
                    "{} {}",
                    scan::tildify(&row.repo.root),
                    row.profile_column()
                ))
            })
            .collect()
    }

    pub fn current(&self) -> Option<&Row> {
        self.visible()
            .contains(&self.selected)
            .then(|| &self.rows[self.selected])
    }

    fn keep_selection_visible(&mut self) {
        self.selected = filter::step(&self.visible(), self.selected, 0);
    }
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
    pub pending: Option<Pending>,
    pub profile_filter: Filter,
    pub watch: Watch,
    pub theme: Theme,
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
            pending: None,
            profile_filter: Filter::default(),
            watch: Watch::default(),
            theme: Theme::from_env(),
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

    /// The highlighted profile, or none when the filter hides it, so a key
    /// never acts on a profile that is not on screen.
    pub fn selected_profile(&self) -> Option<&Profile> {
        let profile = self.config.profiles.get(self.selected)?;
        self.profile_matches(profile).then_some(profile)
    }

    fn profile_matches(&self, p: &Profile) -> bool {
        self.profile_filter
            .matches(&format!("{} {} {}", p.name, p.user_name, p.email))
    }

    pub fn visible_profiles(&self) -> Vec<usize> {
        (0..self.config.profiles.len())
            .filter(|&i| self.profile_matches(&self.config.profiles[i]))
            .collect()
    }

    fn keep_selection_visible(&mut self) {
        self.selected = filter::step(&self.visible_profiles(), self.selected, 0);
    }

    /// Highlights profile `idx`, clearing the filter if it would hide it.
    fn select(&mut self, idx: usize) {
        self.selected = idx;
        if let Some(p) = self.config.profiles.get(idx)
            && !self.profile_matches(p)
        {
            self.profile_filter.clear();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if matches!(
            self.mode,
            Mode::Normal | Mode::Scan(_) | Mode::FixAuthors(_)
        ) {
            self.status = None;
        }
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Help => self.mode = Mode::Normal,
            Mode::Edit(_) => self.handle_edit_key(key),
            Mode::ConfirmDelete => self.handle_confirm_delete_key(key),
            Mode::Scan(_) => self.handle_scan_key(key),
            Mode::FixAuthors(_) => self.handle_fix_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        if self.profile_filter.handle_key(key) {
            self.keep_selection_visible();
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('/'), _) => self.profile_filter.start(),
            (KeyCode::Esc, _) if self.profile_filter.is_active() => {
                self.profile_filter.clear();
            }
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
            (KeyCode::Char('f'), _) => self.open_fix_authors(),
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
                self.keep_selection_visible();
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
        self.select(selected);
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
                self.keep_selection_visible();
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
                    filter: Filter::default(),
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
                    view.keep_selection_visible();
                }
                Err(err) => self.status = Some(format!("Could not scan: {err}")),
            }
        }
    }

    fn handle_scan_key(&mut self, key: KeyEvent) {
        let Mode::Scan(view) = &mut self.mode else {
            return;
        };
        if view.filter.handle_key(key) {
            view.keep_selection_visible();
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true
            }
            (KeyCode::Char('/'), _) => view.filter.start(),
            (KeyCode::Esc, _) if view.filter.is_active() => view.filter.clear(),
            (KeyCode::Esc, _) | (KeyCode::Char('s'), _) => self.mode = Mode::Normal,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                view.selected = filter::step(&view.visible(), view.selected, 1)
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                view.selected = filter::step(&view.visible(), view.selected, -1)
            }
            (KeyCode::Char('r'), _) => self.rescan(),
            (KeyCode::Char('o'), _) => {
                let Some(row) = view.current() else {
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
                let Some(row) = view.current() else {
                    return;
                };
                let repo = row.repo.clone();
                self.status = Some(match rules::expected(&self.config, &repo) {
                    None => "No rule claims this repo; press o to open it and pick a profile"
                        .to_string(),
                    Some((profile, _)) => match git::apply(&repo, profile) {
                        Ok(()) => format!(
                            "Applied profile '{}' to {}{}",
                            profile.name,
                            scan::tildify(&repo.root),
                            cmd::gh_note(profile)
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

    /// Plans re-authoring this repo's unpushed commits as the profile a rule
    /// claims it for, or else the highlighted one, and opens the view when
    /// any were made as someone else.
    fn open_fix_authors(&mut self) {
        let Some(repo) = &self.repo else {
            self.status = Some("Not inside a git repository".into());
            return;
        };
        let (profile, why) = match self.expected_profile() {
            Some((profile, reason)) => (profile, format!("rule: {reason}")),
            None => match self.selected_profile() {
                Some(profile) => (profile, "the highlighted profile".to_string()),
                None => {
                    self.status = Some("No profile to re-author as; press n to create one".into());
                    return;
                }
            },
        };
        let plan = match fix::plan(&repo.root, profile) {
            Ok(plan) => plan,
            Err(err) => {
                self.status = Some(format!("Could not read the commits: {err:#}"));
                return;
            }
        };
        if plan.wrong.is_empty() {
            let pushed = match plan.pushed_wrong.len() {
                0 => String::new(),
                n => format!("; {n} pushed one(s) were not, and are left alone"),
            };
            self.status = Some(match plan.unpushed.len() {
                0 => format!("No unpushed commits to fix{pushed}"),
                n => format!(
                    "All {n} unpushed commit(s) are by '{}'{pushed}",
                    profile.name
                ),
            });
            return;
        }
        let blocked = fix::preflight(&repo.root, &plan)
            .err()
            .map(|e| format!("{e:#}"));
        self.mode = Mode::FixAuthors(Box::new(FixView {
            signs: fix::signs(repo, profile),
            profile: profile.clone(),
            why,
            ticked: vec![true; plan.wrong.len()],
            plan,
            cursor: 0,
            blocked,
            confirming: false,
        }));
    }

    fn handle_fix_key(&mut self, key: KeyEvent) {
        let Mode::FixAuthors(view) = &mut self.mode else {
            return;
        };
        if view.confirming {
            view.confirming = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                self.pending = Some(Pending::FixAuthors);
            } else {
                self.status = Some("Nothing was rewritten".into());
            }
            return;
        }
        let len = view.ticked.len().max(1);
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true
            }
            (KeyCode::Esc, _) | (KeyCode::Char('f'), _) => self.mode = Mode::Normal,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => view.cursor = (view.cursor + 1) % len,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                view.cursor = (view.cursor + len - 1) % len
            }
            (KeyCode::Char(' '), _) => {
                if let Some(t) = view.ticked.get_mut(view.cursor) {
                    *t = !*t;
                }
            }
            (KeyCode::Char('a'), _) => {
                let all = view.ticked.iter().all(|t| *t);
                view.ticked.iter_mut().for_each(|t| *t = !all);
            }
            (KeyCode::Enter, _) => {
                if let Some(why) = &view.blocked {
                    self.status = Some(format!("Cannot rewrite: {why}"));
                } else if view.ticked.iter().any(|t| *t) {
                    view.confirming = true;
                } else {
                    self.status = Some("Nothing is ticked; press space to tick a commit".into());
                }
            }
            _ => {}
        }
    }

    /// Whether the pending work may ask for a passphrase, so the terminal has
    /// to be handed back while it runs.
    pub fn pending_needs_terminal(&self) -> bool {
        match (&self.pending, &self.mode) {
            (Some(Pending::FixAuthors), Mode::FixAuthors(view)) => view.signs,
            _ => false,
        }
    }

    pub fn run_pending(&mut self) {
        let Some(Pending::FixAuthors) = self.pending.take() else {
            return;
        };
        let Mode::FixAuthors(view) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let Some(repo) = &self.repo else {
            return;
        };
        let chosen = view.chosen();
        let outcome = fix::rewrite_chosen(repo, &view.profile, &view.plan, &chosen, &mut |_, _| {});
        self.status = Some(match outcome {
            Ok(()) => format!(
                "Rewrote {} commit(s) as '{}' and applied it{}; the old ones are in git reflog",
                view.rebuilt(),
                view.profile.name,
                cmd::gh_note(&view.profile)
            ),
            Err(err) => format!("Fix authors failed: {err:#}"),
        });
        self.refresh();
        self.select_active();
    }

    /// Reloads what changed on disk since the last look: the profiles file,
    /// when something other than gitenv wrote it, and the repository, when
    /// its config, its HEAD or the global git config moved. Only the lists
    /// reload, so a form, a delete or a rewrite never finds its profile moved;
    /// they catch up once closed. A profiles file that fails to load is
    /// reported once, not on every look. The first look at a repository
    /// re-reads it after noting its files, so a change made while it was
    /// first read is not taken as the starting point.
    pub fn tick(&mut self) {
        if !matches!(self.mode, Mode::Normal | Mode::Scan(_) | Mode::Help) {
            return;
        }
        let on_disk = std::fs::read_to_string(&self.config_path).ok();
        if on_disk != self.config_text && self.watch.config_tried.as_ref() != Some(&on_disk) {
            self.watch.config_tried = Some(on_disk);
            let before = self.config.clone();
            self.reload_config();
            if self.config != before {
                self.status = Some("The profiles file changed on disk; reloaded it".into());
                if matches!(self.mode, Mode::Scan(_)) {
                    self.rescan();
                }
            }
        }

        let Some(root) = self.repo.as_ref().map(|r| r.root.clone()) else {
            return;
        };
        if self.watch.repo_root.as_ref() != Some(&root) {
            self.watch.repo_files = watch::repo_files(&root);
            self.watch.repo_stamps = watch::stamps(&self.watch.repo_files);
            self.watch.repo_root = Some(root);
            self.refresh();
            return;
        }
        let stamps = watch::stamps(&self.watch.repo_files);
        if stamps != self.watch.repo_stamps {
            self.watch.repo_stamps = stamps;
            self.refresh();
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
        self.selected = filter::step(&self.visible_profiles(), self.selected, delta);
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
                let gh = cmd::gh_note(profile);
                self.status = Some(match git::still_overridden(&repo.root, profile) {
                    Some(why) => format!("Applied profile '{}', but {why}{gh}", profile.name),
                    None => format!("Applied profile '{}' to this repo{gh}", profile.name),
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
            self.select(idx);
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
            pending: None,
            profile_filter: Filter::default(),
            watch: Watch::default(),
            theme: Theme::default(),
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

        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        type_keys(&mut app, "toy");
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let Mode::Scan(view) = &app.mode else {
            panic!("still scanning");
        };
        assert_eq!(view.visible(), [1]);
        assert_eq!(
            view.current().unwrap().repo.root.file_name().unwrap(),
            "toy"
        );
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(
            matches!(app.mode, Mode::Scan(_)),
            "Esc clears the filter first"
        );
        app.handle_key(KeyEvent::from(KeyCode::Char('k')));

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

    fn fix_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitenv-app-fix-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        fix_git(&dir, &["init", "-q"]);
        fix_git(&dir, &["config", "--local", "user.name", "Ada"]);
        fix_git(
            &dir,
            &["config", "--local", "user.email", "ada@example.com"],
        );
        dir
    }

    fn fix_git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn fix_commit(dir: &Path, email: &str, msg: &str) {
        fix_git(
            dir,
            &[
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

    fn acme_app(tag: &str, dir: &Path) -> App {
        let mut acme = profile("acme");
        acme.email = "ada@acme.example".into();
        acme.sign_commits = Some(false);
        let mut app = app_with(tag, vec![profile("personal"), acme]);
        app.repo = git::discover(dir).unwrap();
        app.selected = 1;
        app
    }

    #[test]
    fn fix_authors_with_nothing_wrong_only_says_so() {
        let dir = fix_repo("clean");
        fix_commit(&dir, "ada@acme.example", "right");
        let mut app = acme_app("fixclean", &dir);
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.status.as_deref(),
            Some("All 1 unpushed commit(s) are by 'acme'")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fix_authors_ticks_toggles_and_cancels() {
        let dir = fix_repo("toggle");
        fix_commit(&dir, "ada@example.com", "one");
        fix_commit(&dir, "bob@acme.example", "two");
        let head = fix_git(&dir, &["rev-parse", "HEAD"]);
        let mut app = acme_app("fixtoggle", &dir);
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        let Mode::FixAuthors(view) = &app.mode else {
            panic!("f should open the view: {:?}", app.status);
        };
        assert_eq!(view.ticked, [true, true]);
        assert_eq!(view.why, "the highlighted profile");

        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        let Mode::FixAuthors(view) = &app.mode else {
            panic!()
        };
        assert_eq!(view.ticked, [true, false]);
        assert_eq!(view.rebuilt(), 2);

        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        let Mode::FixAuthors(view) = &app.mode else {
            panic!()
        };
        assert_eq!(view.ticked, [true, true]);
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Nothing is ticked")
        );

        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let Mode::FixAuthors(view) = &app.mode else {
            panic!()
        };
        assert!(view.confirming);
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(app.pending, None);
        assert_eq!(app.status.as_deref(), Some("Nothing was rewritten"));
        assert_eq!(fix_git(&dir, &["rev-parse", "HEAD"]), head);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fix_authors_rewrites_only_the_ticked_commits() {
        let dir = fix_repo("rewrite");
        fix_commit(&dir, "ada@example.com", "mine");
        fix_commit(&dir, "bob@acme.example", "pairing");
        let mut app = acme_app("fixrewrite", &dir);
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.pending, Some(Pending::FixAuthors));
        assert!(!app.pending_needs_terminal());
        app.run_pending();
        assert!(matches!(app.mode, Mode::Normal));
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Rewrote 2 commit(s) as 'acme'"),
            "{:?}",
            app.status
        );
        assert_eq!(
            fix_git(&dir, &["log", "--format=%ae %s"]),
            "bob@acme.example pairing\nada@acme.example mine"
        );
        assert_eq!(app.active_profile().unwrap().name, "acme");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn type_keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn filter_hides_profiles_and_keys_act_only_on_visible_ones() {
        let mut app = app_with(
            "filter",
            vec![profile("personal"), profile("acme"), profile("acme-ops")],
        );
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        type_keys(&mut app, "acm");
        assert_eq!(app.visible_profiles(), [1, 2]);
        assert_eq!(app.selected, 1);
        type_keys(&mut app, "jk");
        assert_eq!(app.profile_filter.query, "acmjk");
        assert!(app.selected_profile().is_none());
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.selected, 2);
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.should_quit);
        assert_eq!(app.visible_profiles(), [0, 1, 2]);
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn a_new_profile_the_filter_would_hide_clears_it() {
        let mut app = app_with("filternew", vec![profile("acme")]);
        app.profile_filter.query = "acme".into();
        app.save_profile(None, profile("personal")).unwrap();
        assert_eq!(app.selected, 1);
        assert!(!app.profile_filter.is_active());
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn tick_reloads_a_profiles_file_changed_elsewhere_but_not_mid_form() {
        let mut app = app_with("tick", vec![profile("a")]);
        app.save_profile(Some(0), profile("a")).unwrap();
        app.tick();
        assert_eq!(app.status.as_deref(), Some("Saved profile 'a'"));

        let mut text = std::fs::read_to_string(&app.config_path).unwrap();
        text.push_str("\n[[profiles]]\nname = \"b\"\nuser_name = \"B\"\nemail = \"b@x\"\n");
        std::fs::write(&app.config_path, &text).unwrap();
        app.mode = Mode::Edit(Box::new(Form::new()));
        app.tick();
        assert_eq!(app.config.profiles.len(), 1);

        app.mode = Mode::Normal;
        app.tick();
        assert_eq!(app.config.profiles.len(), 2);
        assert!(app.status.as_deref().unwrap().contains("changed on disk"));

        std::fs::write(&app.config_path, "not toml [").unwrap();
        app.tick();
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Could not read config")
        );
        app.status = None;
        app.tick();
        assert_eq!(app.status, None, "a broken file is reported once");
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn tick_refreshes_the_repo_when_its_config_changes() {
        let dir = fix_repo("tick");
        let mut app = app_with("tickrepo", vec![profile("a")]);
        app.repo = git::discover(&dir).unwrap();
        app.tick();
        fix_git(
            &dir,
            &["config", "--local", "user.email", "ada@acme.example"],
        );
        app.tick();
        assert_eq!(
            app.repo.as_ref().unwrap().effective.email.as_deref(),
            Some("ada@acme.example")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
