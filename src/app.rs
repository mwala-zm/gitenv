use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cmd;
use crate::doctor::{self, Finding};
use crate::export::{self, Includes};
use crate::filter::{self, Filter};
use crate::fix;
use crate::form::Form;
use crate::git::{self, Repo};
use crate::hook::{self, HookStatus};
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
    Doctor(Box<DoctorView>),
    ConfirmSetup(SetupAction),
    Why,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAction {
    Hooks,
    Includes,
}

/// Whether commits are checked by the hooks, and whether git applies the
/// rules by itself through the exported includes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    pub hooks: HookStatus,
    pub includes: Result<Includes, String>,
}

#[derive(Debug, Clone)]
pub struct DoctorView {
    pub name: String,
    pub email: String,
    pub findings: Vec<Finding>,
    pub checking: bool,
}

pub type SshProbe = fn(&Path, &str) -> Result<String, String>;

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
    pub marked: Vec<bool>,
    pub wrong_only: bool,
    pub confirming: bool,
}

impl ScanView {
    pub fn new(root: PathBuf, rows: Vec<Row>) -> Self {
        Self {
            root,
            marked: vec![false; rows.len()],
            rows,
            selected: 0,
            filter: Filter::default(),
            wrong_only: false,
            confirming: false,
        }
    }

    /// The marked rows that can still be fixed.
    pub fn to_fix(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| self.marked.get(i).copied().unwrap_or(false) && self.rows[i].is_fixable())
            .collect()
    }

    /// The rows the filter lets through, matched on the path shown and the
    /// profile column, less those already right while only problems show.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| {
                let row = &self.rows[i];
                (!self.wrong_only || !row.is_ok())
                    && self.filter.matches(&format!(
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
    pub health: Vec<Vec<Finding>>,
    pub doctor_rx: Option<Receiver<Vec<Finding>>>,
    pub ssh_probe: SshProbe,
    pub setup: Option<Setup>,
    pub known_repos: Option<Vec<Repo>>,
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
            health: Vec::new(),
            doctor_rx: None,
            ssh_probe: doctor::ssh_probe,
            setup: None,
            known_repos: None,
        };
        app.check_health();
        app.check_setup();
        app.select_active();
        if started_outside {
            let status = app.status.take();
            app.open_scan();
            app.status = status;
        }
        Ok(app)
    }

    /// An app on `profiles` with nothing read from disk, for tests.
    #[cfg(test)]
    pub fn for_test(profiles: Vec<Profile>, config_path: PathBuf) -> Self {
        Self {
            config: Config { profiles },
            config_path,
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
            health: Vec::new(),
            doctor_rx: None,
            ssh_probe: |_, _| Err("no network in tests".into()),
            setup: None,
            known_repos: None,
        }
    }

    /// Looks at the hooks for this repository and the includes, which also
    /// brings exported includes in line with the profiles.
    pub fn check_setup(&mut self) {
        self.setup = Some(Setup {
            hooks: hook::status(self.repo.as_ref().map(|r| r.root.as_path())),
            includes: export::status(&self.config, &self.config_path).map_err(|e| format!("{e:#}")),
        });
    }

    /// Runs doctor's local checks on every profile, for the badges in the
    /// list: key files and the gpg keyring, never the network.
    pub fn check_health(&mut self) {
        self.health = self
            .config
            .profiles
            .iter()
            .map(|p| doctor::findings_for(p, None))
            .collect();
    }

    pub fn badge(&self, index: usize) -> Option<doctor::Level> {
        self.health.get(index).and_then(|f| doctor::badge(f))
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
            Mode::Help | Mode::Why => self.mode = Mode::Normal,
            Mode::Edit(_) => self.handle_edit_key(key),
            Mode::ConfirmDelete => self.handle_confirm_delete_key(key),
            Mode::Scan(_) => self.handle_scan_key(key),
            Mode::FixAuthors(_) => self.handle_fix_key(key),
            Mode::Doctor(_) => self.handle_doctor_key(key),
            Mode::ConfirmSetup(action) => {
                self.mode = Mode::Normal;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                    self.run_setup(action);
                } else {
                    self.status = Some("Nothing was changed".into());
                }
            }
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
            (KeyCode::Char('D'), _) => self.open_doctor(),
            (KeyCode::Char('w'), _) => {
                if self.repo.is_some() {
                    self.mode = Mode::Why;
                } else {
                    self.status = Some("Not inside a git repository".into());
                }
            }
            (KeyCode::Char('H'), _) => self.ask_setup(SetupAction::Hooks),
            (KeyCode::Char('I'), _) => self.ask_setup(SetupAction::Includes),
            (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => self.mode = Mode::Help,
            (KeyCode::Char('n'), _) => self.mode = Mode::Edit(Box::new(Form::new())),
            (KeyCode::Char('N'), _) => match &self.repo {
                Some(repo) => self.mode = Mode::Edit(Box::new(Form::from_repo(repo))),
                None => {
                    self.status =
                        Some("Not inside a git repository; press n for an empty profile".into())
                }
            },
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
        if form.picker.is_some() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => form.move_picker(1),
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => form.move_picker(-1),
                KeyCode::Enter => form.pick(),
                _ => form.picker = None,
            }
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.mode = Mode::Normal;
            }
            (KeyCode::Char('o'), KeyModifiers::CONTROL) if form.pickable() => {
                let ssh_dir = directories::UserDirs::new()
                    .map(|d| d.home_dir().join(".ssh"))
                    .unwrap_or_default();
                let items = form.candidates(&ssh_dir);
                if items.is_empty() {
                    form.error = Some(match form.field {
                        crate::form::SSH_KEY => "No keys with a .pub next to them in ~/.ssh".into(),
                        crate::form::GH_ACCOUNT => {
                            "gh knows no accounts; log in with `gh auth login`".into()
                        }
                        _ => "Nothing to pick from".into(),
                    });
                } else {
                    form.open_picker(items);
                }
            }
            (KeyCode::Tab, _) | (KeyCode::Down, _) => {
                form.next_field();
                self.load_known_repos();
            }
            (KeyCode::BackTab, _) | (KeyCode::Up, _) => {
                form.prev_field();
                self.load_known_repos();
            }
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
        self.check_health();
        Ok(())
    }

    fn reload_config(&mut self) {
        match Config::load(&self.config_path) {
            Ok(config) => {
                self.config = config;
                self.config_text = std::fs::read_to_string(&self.config_path).ok();
                self.check_health();
                if self.setup.is_some() {
                    self.check_setup();
                }
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
            Ok(rows) => self.mode = Mode::Scan(Box::new(ScanView::new(root, rows))),
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
                    let marked: Vec<&Path> = view
                        .to_fix()
                        .into_iter()
                        .map(|i| view.rows[i].repo.root.as_path())
                        .collect();
                    let marked: Vec<bool> = rows
                        .iter()
                        .map(|r| marked.contains(&r.repo.root.as_path()))
                        .collect();
                    view.marked = marked;
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
        if view.confirming {
            view.confirming = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                self.fix_marked();
            } else {
                self.status = Some("Nothing was changed".into());
            }
            return;
        }
        if view.filter.handle_key(key) {
            view.keep_selection_visible();
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true
            }
            (KeyCode::Char('/'), _) => view.filter.start(),
            (KeyCode::Char(' '), _) => {
                let Some(row) = view.current() else {
                    return;
                };
                if row.is_fixable() {
                    let i = view.selected;
                    view.marked[i] = !view.marked[i];
                } else {
                    self.status = Some(match row.status_word() {
                        "none" => "No rule claims this repo, so there is nothing to fix; press o to open it".into(),
                        _ => "This repo is already on the right profile".into(),
                    });
                }
            }
            (KeyCode::Char('a'), _) => {
                let wrong: Vec<usize> = view
                    .visible()
                    .into_iter()
                    .filter(|&i| view.rows[i].is_fixable())
                    .collect();
                let all = wrong.iter().all(|&i| view.marked[i]);
                for i in wrong {
                    view.marked[i] = !all;
                }
            }
            (KeyCode::Char('w'), _) => {
                view.wrong_only = !view.wrong_only;
                view.keep_selection_visible();
            }
            (KeyCode::Char('F'), _) => {
                if view.to_fix().is_empty() {
                    self.status = Some(
                        "Nothing is marked; press space to mark a repo or a to mark every wrong one"
                            .into(),
                    );
                } else {
                    view.confirming = true;
                }
            }
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
        self.take_doctor_result();
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

    /// Gives every marked repository the profile its rules name, as `scan
    /// --fix` does, and says how many worked and why any did not.
    fn fix_marked(&mut self) {
        let Mode::Scan(view) = &mut self.mode else {
            return;
        };
        let todo = view.to_fix();
        let mut errors = Vec::new();
        let mut current = false;
        for &i in &todo {
            match scan::fix(&self.config, &mut view.rows[i]) {
                Ok(()) => {
                    current |= self
                        .repo
                        .as_ref()
                        .is_some_and(|r| r.root == view.rows[i].repo.root)
                }
                Err(err) => errors.push(err),
            }
        }
        view.marked.fill(false);
        view.keep_selection_visible();
        let fixed = todo.len() - errors.len();
        let noun = |n: usize| if n == 1 { "repository" } else { "repositories" };
        self.status = Some(match errors.first() {
            None => format!("Fixed {fixed} {}", noun(fixed)),
            Some(first) => {
                let more = match errors.len() {
                    1 => String::new(),
                    n => format!(" (and {} more)", n - 1),
                };
                format!(
                    "Fixed {fixed} of {} {}; {first}{more}",
                    todo.len(),
                    noun(todo.len())
                )
            }
        });
        if current {
            self.refresh();
        }
    }

    /// Shows everything doctor says about the highlighted profile: the local
    /// checks at once, and the logins over ssh and gh as they come back from
    /// a background thread, so the screen never waits on the network.
    fn open_doctor(&mut self) {
        let Some(profile) = self.selected_profile().cloned() else {
            self.status = Some("No profile to check".into());
            return;
        };
        let findings = doctor::findings_for(&profile, None);
        let checking = doctor::needs_network(&profile);
        self.doctor_rx = None;
        if checking {
            let (tx, rx) = mpsc::channel();
            let probe = self.ssh_probe;
            let p = profile.clone();
            std::thread::spawn(move || {
                let _ = tx.send(doctor::findings_for(&p, Some(&probe)));
            });
            self.doctor_rx = Some(rx);
        }
        self.mode = Mode::Doctor(Box::new(DoctorView {
            name: profile.name,
            email: profile.email,
            findings,
            checking,
        }));
    }

    fn handle_doctor_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char('r'), _) => self.open_doctor(),
            _ => {
                self.doctor_rx = None;
                self.mode = Mode::Normal;
            }
        }
    }

    /// Takes the full findings from the background check once they arrive.
    fn take_doctor_result(&mut self) {
        let Some(rx) = &self.doctor_rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(findings) => Some(findings),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        self.doctor_rx = None;
        if let Mode::Doctor(view) = &mut self.mode {
            view.checking = false;
            if let Some(findings) = result {
                view.findings = findings;
            }
        }
    }

    /// Asks before installing what `action` names, unless it is in place.
    fn ask_setup(&mut self, action: SetupAction) {
        let done = self.setup.as_ref().is_some_and(|s| match action {
            SetupAction::Hooks => s.hooks == HookStatus::Everywhere,
            SetupAction::Includes => s.includes == Ok(Includes::Active),
        });
        if done {
            self.status = Some(match action {
                SetupAction::Hooks => "The hooks already check every repository".into(),
                SetupAction::Includes => "git already applies your rules by itself".into(),
            });
        } else {
            self.mode = Mode::ConfirmSetup(action);
        }
    }

    /// Installs the hooks for every repository, as `gitenv hook install
    /// --global` does, or the includes, as `gitenv export --install` does.
    fn run_setup(&mut self, action: SetupAction) {
        self.status = Some(match action {
            SetupAction::Hooks => {
                let home = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf());
                let dir = self
                    .repo
                    .as_ref()
                    .map(|r| r.root.clone())
                    .or(home)
                    .unwrap_or_else(std::env::temp_dir);
                match hook::install(&dir, true) {
                    Ok(_) => "Installed the hooks; every commit and push is checked now".into(),
                    Err(err) => format!("Could not install the hooks: {err:#}"),
                }
            }
            SetupAction::Includes => match export::export(&self.config, &self.config_path, true) {
                Ok(_) => {
                    "Installed the includes; new clones get the right identity by themselves".into()
                }
                Err(err) => format!("Could not install the includes: {err:#}"),
            },
        });
        self.check_setup();
    }

    /// Finds the repositories next to this one once, the first time the
    /// form reaches a rule field, so the rule preview has something to show.
    fn load_known_repos(&mut self) {
        let Mode::Edit(form) = &self.mode else {
            return;
        };
        if self.known_repos.is_some()
            || !matches!(form.field, crate::form::REMOTES | crate::form::PATHS)
        {
            return;
        }
        let mut repos: Vec<Repo> = self
            .scan_rows(&self.scan_root.clone())
            .map(|rows| rows.into_iter().map(|r| r.repo).collect())
            .unwrap_or_default();
        if let Some(here) = &self.repo
            && !repos.iter().any(|r| r.root == here.root)
        {
            repos.push(here.clone());
        }
        self.known_repos = Some(repos);
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
        if self.setup.is_some() {
            self.check_setup();
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
        App::for_test(
            profiles,
            std::env::temp_dir().join(format!("gitenv-app-test-{}-{tag}.toml", std::process::id())),
        )
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

    #[test]
    fn scan_view_fixes_only_the_marked_repos() {
        let base = std::env::temp_dir().join(format!("gitenv-app-bulk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (name, remote) in [
            ("api", "git@github.com:acme/api.git"),
            ("toy", "git@github.com:ada/toy.git"),
            ("web", "git@github.com:acme/web.git"),
        ] {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            fix_git(&dir, &["init", "-q"]);
            fix_git(&dir, &["remote", "add", "origin", remote]);
            fix_git(&dir, &["config", "--local", "user.name", "Ada"]);
            fix_git(
                &dir,
                &["config", "--local", "user.email", "ada@example.com"],
            );
        }
        let mut acme = profile("acme");
        acme.email = "ada@acme.example".into();
        acme.remotes = vec!["github.com/acme/*".into()];
        let mut app = app_with("bulk", vec![profile("personal"), acme]);
        app.scan_root = base.clone();
        let view = |app: &App| match &app.mode {
            Mode::Scan(view) => view.clone(),
            _ => panic!("not scanning: {:?}", app.status),
        };

        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Nothing is marked")
        );
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(view(&app).to_fix(), [0, 2]);
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("already on the right profile")
        );
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(view(&app).to_fix(), [0]);

        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert_eq!(view(&app).visible(), [0, 2]);

        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(app.status.as_deref(), Some("Nothing was changed"));
        assert_eq!(
            fix_git(&base.join("api"), &["config", "user.email"]),
            "ada@example.com"
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.status.as_deref(), Some("Fixed 1 repository"));
        assert_eq!(
            fix_git(&base.join("api"), &["config", "user.email"]),
            "ada@acme.example"
        );
        assert_eq!(
            fix_git(&base.join("web"), &["config", "user.email"]),
            "ada@example.com"
        );
        let after = view(&app);
        assert_eq!(after.rows[0].status_word(), "fixed");
        assert!(after.to_fix().is_empty());
        assert_eq!(
            after.visible(),
            [2],
            "the fixed repo leaves the problems-only list"
        );
        assert_eq!(
            after.selected, 2,
            "the highlight moves to a row still shown"
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        app.handle_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(view(&app).to_fix(), [2], "marks survive a rescan");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn health_badges_follow_saved_profiles() {
        let mut app = app_with("health", vec![profile("a")]);
        app.check_health();
        assert_eq!(app.badge(0), None);
        let mut broken = profile("a");
        broken.ssh_key = Some("/nonexistent/id_a".into());
        app.save_profile(Some(0), broken).unwrap();
        assert_eq!(app.badge(0), Some(doctor::Level::Fail));
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn doctor_view_shows_local_checks_then_the_logins() {
        let mut acme = profile("acme");
        acme.ssh_key = Some(std::env::current_exe().unwrap());
        acme.remotes = vec!["github.com/acme/*".into()];
        let mut app = app_with("doctor", vec![acme]);
        app.ssh_probe = |_, host| Ok(format!("Hi ada-acme! ({host})"));
        app.handle_key(KeyEvent::from(KeyCode::Char('D')));
        let Mode::Doctor(view) = &app.mode else {
            panic!("D should open the doctor view: {:?}", app.status);
        };
        assert!(view.checking);
        assert_eq!(view.findings.len(), 1, "{:?}", view.findings);
        assert!(app.doctor_rx.is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while matches!(&app.mode, Mode::Doctor(v) if v.checking) {
            assert!(
                std::time::Instant::now() < deadline,
                "the check never came back"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.tick();
        }
        let Mode::Doctor(view) = &app.mode else {
            panic!()
        };
        assert!(
            view.findings
                .iter()
                .any(|f| f.message == "github.com: Hi ada-acme! (github.com)"),
            "{:?}",
            view.findings
        );
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn setup_keys_ask_first_and_skip_what_is_in_place() {
        let mut app = app_with("setup", vec![profile("a")]);
        app.setup = Some(Setup {
            hooks: HookStatus::Everywhere,
            includes: Ok(Includes::Missing),
        });
        app.handle_key(KeyEvent::from(KeyCode::Char('H')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.status.as_deref(),
            Some("The hooks already check every repository")
        );
        app.handle_key(KeyEvent::from(KeyCode::Char('I')));
        assert!(matches!(
            app.mode,
            Mode::ConfirmSetup(SetupAction::Includes)
        ));
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.status.as_deref(), Some("Nothing was changed"));
    }

    #[test]
    fn shift_n_starts_a_profile_from_the_repo() {
        let mut app = app_with("fromrepo", vec![profile("personal")]);
        app.handle_key(KeyEvent::from(KeyCode::Char('N')));
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Not inside a git repository")
        );

        let dir = fix_repo("fromrepo");
        fix_git(
            &dir,
            &[
                "remote",
                "add",
                "origin",
                "https://gitlab.com/Client/site.git",
            ],
        );
        fix_git(
            &dir,
            &["config", "--local", "user.email", "ada@client.example"],
        );
        app.repo = git::discover(&dir).unwrap();
        app.handle_key(KeyEvent::from(KeyCode::Char('N')));
        let Mode::Edit(form) = &app.mode else {
            panic!("N should open the form: {:?}", app.status);
        };
        assert_eq!(form.values[0], "client");
        assert_eq!(form.values[2], "ada@client.example");
        assert_eq!(form.values[8], "gitlab.com/client/*");
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Normal), "{:?}", app.status);
        assert_eq!(app.active_profile().unwrap().name, "client");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_file(&app.config_path).ok();
    }

    #[test]
    fn ctrl_o_picks_a_value_and_esc_only_closes_the_picker() {
        let mut app = app_with("picker", vec![]);
        app.mode = Mode::Edit(Box::new(Form::new()));
        let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        app.handle_key(ctrl_o);
        let Mode::Edit(form) = &app.mode else {
            panic!()
        };
        assert!(form.picker.is_none(), "the name field has nothing to pick");
        for _ in 0..crate::form::SIGNING_FORMAT {
            app.handle_key(KeyEvent::from(KeyCode::Tab));
        }
        app.handle_key(ctrl_o);
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        let Mode::Edit(form) = &app.mode else {
            panic!("Esc in the picker left the form");
        };
        assert!(form.picker.is_none());
        app.handle_key(ctrl_o);
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let Mode::Edit(form) = &app.mode else {
            panic!("Enter in the picker saved the form");
        };
        assert_eq!(form.values[crate::form::SIGNING_FORMAT], "ssh");
    }
}
