use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::Block;

use crate::app::{App, Mode};
use crate::ui;

/// Mouse capture is on unless `GITENV_MOUSE` is `0`, `false`, `off` or
/// `no`, for those who would rather keep the terminal's own text selection.
pub fn enabled() -> bool {
    std::env::var("GITENV_MOUSE").map_or(true, |v| {
        !matches!(
            v.to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// The wheel moves through the list on screen as `↑` `↓` do. A left click
/// highlights the profile or repository under it, and in the fix-authors
/// view ticks or unticks the commit. Popups other than help ignore the
/// mouse, so a stray click never confirms or edits anything.
pub fn handle(app: &mut App, event: MouseEvent, screen: Rect) {
    let lists = match &app.mode {
        Mode::Normal | Mode::Scan(_) => true,
        Mode::FixAuthors(view) => !view.confirming,
        _ => false,
    };
    match event.kind {
        MouseEventKind::ScrollDown if lists => app.handle_key(KeyEvent::from(KeyCode::Down)),
        MouseEventKind::ScrollUp if lists => app.handle_key(KeyEvent::from(KeyCode::Up)),
        MouseEventKind::Down(MouseButton::Left) => click(app, event.column, event.row, screen),
        _ => {}
    }
}

fn inside(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
}

fn click(app: &mut App, x: u16, y: u16, screen: Rect) {
    let areas = ui::areas(screen);
    match &mut app.mode {
        Mode::Help => app.mode = Mode::Normal,
        Mode::Normal => {
            let inner = Block::bordered().inner(areas.left);
            if !inside(inner, x, y) {
                return;
            }
            let visible = app.visible_profiles();
            let selected = visible.iter().position(|&i| i == app.selected);
            if let Some(&idx) = ui::list_row(inner, selected, y).and_then(|row| visible.get(row)) {
                app.selected = idx;
                app.status = None;
            }
        }
        Mode::Scan(view) => {
            let inner = Block::bordered().inner(areas.body);
            if !inside(inner, x, y) {
                return;
            }
            let visible = view.visible();
            let selected = visible.iter().position(|&i| i == view.selected);
            if let Some(&idx) = ui::list_row(inner, selected, y).and_then(|row| visible.get(row)) {
                view.selected = idx;
                app.status = None;
            }
        }
        Mode::FixAuthors(view) if !view.confirming => {
            let [_, list, _] = ui::fix_parts(areas.body);
            if !inside(list, x, y) {
                return;
            }
            let wrong_row = |w: usize| {
                view.plan
                    .unpushed
                    .iter()
                    .position(|c| view.plan.wrong.get(w).is_some_and(|w| w.hash == c.hash))
            };
            let Some(row) = ui::list_row(list, wrong_row(view.cursor), y) else {
                return;
            };
            let Some(commit) = view.plan.unpushed.get(row) else {
                return;
            };
            if let Some(w) = view.plan.wrong.iter().position(|c| c.hash == commit.hash) {
                view.cursor = w;
                view.ticked[w] = !view.ticked[w];
                app.status = None;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Filter;
    use crate::profile::{Config, Profile};
    use crate::theme::Theme;
    use crate::watch::Watch;
    use ratatui::crossterm::event::KeyModifiers;

    fn app(names: &[&str]) -> App {
        App {
            config: Config {
                profiles: names
                    .iter()
                    .map(|n| Profile {
                        name: n.to_string(),
                        user_name: "Ada".into(),
                        email: "ada@example.com".into(),
                        ..Default::default()
                    })
                    .collect(),
            },
            config_path: "/nonexistent/config.toml".into(),
            repo: None,
            selected: 0,
            status: None,
            mode: Mode::Normal,
            should_quit: false,
            scan_root: "/".into(),
            config_text: None,
            pending: None,
            profile_filter: Filter::default(),
            watch: Watch::default(),
            theme: Theme::default(),
        }
    }

    fn at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    const SCREEN: Rect = Rect::new(0, 0, 96, 26);

    #[test]
    fn clicking_a_profile_highlights_it_and_the_wheel_moves() {
        let mut app = app(&["personal", "acme", "oss"]);
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 5, 4),
            SCREEN,
        );
        assert_eq!(app.selected, 2);
        handle(&mut app, at(MouseEventKind::ScrollUp, 50, 10), SCREEN);
        assert_eq!(app.selected, 1);
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 5, 20),
            SCREEN,
        );
        assert_eq!(
            app.selected, 1,
            "a click below the last profile does nothing"
        );
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 60, 2),
            SCREEN,
        );
        assert_eq!(
            app.selected, 1,
            "a click in the repository pane does nothing"
        );
    }

    #[test]
    fn clicks_follow_the_filter() {
        let mut app = app(&["personal", "acme", "oss"]);
        app.profile_filter.query = "o".into();
        app.selected = 2;
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 5, 2),
            SCREEN,
        );
        assert_eq!(app.selected, 2, "the first row is the only visible profile");
    }

    #[test]
    fn popups_ignore_the_mouse_except_help() {
        let mut app = app(&["personal", "acme"]);
        app.mode = Mode::ConfirmDelete;
        handle(&mut app, at(MouseEventKind::ScrollDown, 5, 3), SCREEN);
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 5, 3),
            SCREEN,
        );
        assert!(matches!(app.mode, Mode::ConfirmDelete));
        assert_eq!(app.config.profiles.len(), 2);
        app.mode = Mode::Help;
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 5, 3),
            SCREEN,
        );
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn clicking_a_wrong_commit_ticks_it_and_others_do_nothing() {
        use crate::app::FixView;
        use crate::fix::{Commit, Plan};
        let commit = |hash: &str, email: &str| Commit {
            hash: hash.repeat(40),
            author: "Ada".into(),
            email: email.into(),
            committer: "Ada".into(),
            committer_email: email.into(),
            subject: "x".into(),
            parents: 1,
        };
        let unpushed = vec![
            commit("1", "ada@example.com"),
            commit("2", "ada@acme.example"),
            commit("3", "ada@example.com"),
        ];
        let wrong = vec![unpushed[0].clone(), unpushed[2].clone()];
        let mut app = app(&["acme"]);
        app.mode = Mode::FixAuthors(Box::new(FixView {
            profile: app.config.profiles[0].clone(),
            why: String::new(),
            plan: Plan::for_test(unpushed, wrong, Vec::new()),
            ticked: vec![true, true],
            cursor: 0,
            blocked: None,
            signs: false,
            confirming: false,
        }));
        let list_top = 4;
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 8, list_top + 2),
            SCREEN,
        );
        handle(
            &mut app,
            at(MouseEventKind::Down(MouseButton::Left), 8, list_top + 1),
            SCREEN,
        );
        let Mode::FixAuthors(view) = &app.mode else {
            panic!("still fixing");
        };
        assert_eq!(view.cursor, 1);
        assert_eq!(view.ticked, [true, false]);
    }

    #[test]
    fn list_rows_account_for_scrolling() {
        let inner = Rect::new(1, 2, 10, 3);
        assert_eq!(ui::list_row(inner, Some(0), 2), Some(0));
        assert_eq!(ui::list_row(inner, Some(4), 2), Some(2));
        assert_eq!(ui::list_row(inner, Some(4), 4), Some(4));
        assert_eq!(ui::list_row(inner, None, 5), None);
        assert_eq!(ui::list_row(inner, None, 1), None);
    }
}
