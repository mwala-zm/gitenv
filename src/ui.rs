use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Mode, ScanView};
use crate::form::{Form, LABEL_WIDTH, LABELS};
use crate::git::Repo;
use crate::scan;

pub fn render(frame: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, header);
    if let Mode::Scan(view) = &app.mode {
        render_scan(frame, body, view);
        render_footer(frame, footer, app);
        return;
    }

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);

    render_profiles(frame, left, app);
    render_repo(frame, right, app);
    render_footer(frame, footer, app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Help => render_help(frame),
        Mode::Edit(form) => render_form(frame, form),
        Mode::ConfirmDelete => render_confirm_delete(frame, app),
        Mode::Scan(_) => {}
    }
}

fn render_scan(frame: &mut Frame, area: Rect, view: &ScanView) {
    let ok = view.rows.iter().filter(|r| r.is_ok()).count();
    let title = format!(
        " Repositories under {}: {ok} of {} on the right profile ",
        scan::tildify(&view.root),
        view.rows.len()
    );
    let width = view
        .rows
        .iter()
        .map(|r| r.profile_column().chars().count())
        .max()
        .unwrap_or(0)
        .max("PROFILE".len());
    let items: Vec<ListItem> = view
        .rows
        .iter()
        .map(|row| {
            let word = row.status_word();
            let colour = match word {
                "ok" | "fixed" => Color::Green,
                "wrong" => Color::Red,
                _ => Color::Yellow,
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{word:<6}"), Style::new().fg(colour)),
                Span::raw(format!("{:<width$}  ", row.profile_column())),
                // The title names the root, so paths below it are shown short.
                Span::styled(
                    match row.repo.root.strip_prefix(&view.root) {
                        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
                        _ => scan::tildify(&row.repo.root),
                    },
                    Style::new().dim(),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(
            Style::new()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(view.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    Rect::new(x, y, width, height)
}

fn render_help(frame: &mut Frame) {
    let rows = [
        ("↑ ↓  j k", "move between profiles"),
        ("Enter", "apply the selected profile to this repo"),
        ("e", "edit the selected profile"),
        ("n", "create a new profile"),
        ("d", "delete the selected profile"),
        ("r", "re-read the repository"),
        ("s", "list every repository next to this one"),
        ("?  h", "show this help"),
        ("q  Esc", "quit"),
        ("", ""),
        ("Applying writes user.name, user.email and, when set,", ""),
        (
            "user.signingkey, gpg.format, commit.gpgsign, core.sshCommand",
            "",
        ),
        ("and any extra keys into .git/config.", ""),
        ("", ""),
        ("● marks the profile git is using here; ◆ marks the one", ""),
        ("whose remote or path rules claim this repo.", ""),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!("{key:<10}"), Style::new().bold()),
                Span::raw(*what),
            ])
        })
        .collect();

    let area = centered(frame.area(), 68, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Help (any key to close) ")),
        area,
    );
}

fn render_confirm_delete(frame: &mut Frame, app: &App) {
    let name = app
        .selected_profile()
        .map(|p| p.name.as_str())
        .unwrap_or("");
    let lines = vec![
        Line::from(format!("Delete profile '{name}'?").bold()),
        Line::from(""),
        Line::from("This only removes it from gitenv's config file.".dim()),
        Line::from("Repos it was applied to are left unchanged.".dim()),
        Line::from(""),
        Line::from(vec![
            " y / Enter ".bold(),
            "delete   ".dim(),
            " any other key ".bold(),
            "cancel".dim(),
        ]),
    ];
    let area = centered(frame.area(), 56, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Delete profile ")),
        area,
    );
}

fn render_form(frame: &mut Frame, form: &Form) {
    let label_width = LABEL_WIDTH;
    let area = centered(frame.area(), 78, LABELS.len() as u16 + 4);
    // Columns left for the value: inside the borders, after the label, and
    // one spare cell so the cursor never lands on the right border.
    let value_width = (area.width as usize).saturating_sub(2 + label_width + 1);

    let mut lines: Vec<Line> = LABELS
        .iter()
        .zip(form.values.iter())
        .enumerate()
        .map(|(i, (label, value))| {
            let style = if i == form.field {
                Style::new().bold().fg(Color::Cyan)
            } else {
                Style::new()
            };
            let offset = if i == form.field {
                scroll_offset(form.cursor, value_width)
            } else {
                0
            };
            Line::from(vec![
                Span::styled(format!("{label:<label_width$}"), style),
                Span::raw(window(value, offset, value_width)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(match &form.error {
        Some(err) => Line::from(err.clone().red()),
        None => Line::from("Tab/↓ next field   ←→ Home End move   Enter save   Esc cancel".dim()),
    });

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(form.title())),
        area,
    );

    let offset = scroll_offset(form.cursor, value_width);
    let x = area.x + 1 + label_width as u16 + (form.cursor - offset) as u16;
    let y = area.y + 1 + form.field as u16;
    if x < area.right() {
        frame.set_cursor_position((x, y));
    }
}

/// How many characters to hide on the left so a cursor at `cursor` fits in
/// `width` columns with one spare cell for the cursor itself.
fn scroll_offset(cursor: usize, width: usize) -> usize {
    if width == 0 {
        return cursor;
    }
    cursor.saturating_sub(width - 1)
}

fn window(s: &str, offset: usize, width: usize) -> String {
    s.chars().skip(offset).take(width).collect()
}

fn render_header(frame: &mut Frame, area: Rect) {
    let title = Line::from(vec![
        " gitenv ".bold().reversed(),
        " right identity, right repo".dim(),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

const WORDMARK: [&str; 6] = [
    r"       _ _",
    r"  __ _(_) |_  ___ _ __ __   __",
    r" / _` | | __|/ _ \ '_ \ \ / /",
    r"| (_| | | |_|  __/ | | |\ V /",
    r" \__, |_|\__|\___|_| |_| \_/",
    r" |___/",
];

fn render_wordmark(frame: &mut Frame, inner: Rect, used_rows: u16) {
    let height = WORDMARK.len() as u16;
    let width = WORDMARK.iter().map(|l| l.len()).max().unwrap_or(0) as u16;
    if inner.width < width || inner.height < used_rows.saturating_add(height + 1) {
        return;
    }
    let area = Rect::new(
        inner.x + (inner.width - width) / 2,
        inner.y + inner.height - height,
        width,
        height,
    );
    let lines: Vec<Line> = WORDMARK.iter().map(|l| Line::from(*l)).collect();
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().fg(Color::DarkGray)),
        area,
    );
}

fn render_profiles(frame: &mut Frame, area: Rect, app: &App) {
    let active = app.active_profile();
    let expected = app.expected_profile().map(|(p, _)| p);

    let items: Vec<ListItem> = app
        .config
        .profiles
        .iter()
        .map(|p| {
            let (marker, colour) = match (active == Some(p), expected == Some(p)) {
                (true, _) => ("● ", Color::Green),
                (false, true) => ("◆ ", Color::Yellow),
                (false, false) => ("  ", Color::Reset),
            };
            let line = Line::from(vec![
                Span::styled(marker, Style::new().fg(colour)),
                Span::raw(&p.name),
                Span::raw("  "),
                Span::styled(&p.email, Style::new().dim()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let block = Block::bordered().title(" Profiles ");
    let inner = block.inner(area);

    if items.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from("No profiles yet.".bold()),
            Line::from(""),
            Line::from("Create this file:"),
            Line::from(app.config_path.display().to_string().cyan()),
            Line::from(""),
            Line::from("[[profiles]]"),
            Line::from("name = \"work\""),
            Line::from("user_name = \"Your Name\""),
            Line::from("email = \"you@company.com\""),
        ])
        .wrap(Wrap { trim: false })
        .block(block);
        frame.render_widget(hint, area);
        // The hint is nine lines; allow two more for the path wrapping.
        render_wordmark(frame, inner, 11);
        return;
    }
    let used_rows = items.len() as u16;

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::new()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
    render_wordmark(frame, inner, used_rows);
}

fn render_repo(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" Current repository ");

    let Some(repo) = &app.repo else {
        let text = Paragraph::new("Not inside a git repository.".yellow()).block(block);
        frame.render_widget(text, area);
        return;
    };

    let field = |label: &str, value: &Option<String>| {
        let shown = match value {
            Some(v) => Span::raw(v.clone()),
            None => Span::styled("(not set)", Style::new().dim()),
        };
        Line::from(vec![
            Span::styled(format!("{label:<12}"), Style::new().dim()),
            shown,
        ])
    };

    let active = app.active_profile();
    let expected = app.expected_profile();
    let verdict = match (&expected, active) {
        (Some((want, _)), Some(have)) if *want != have => Line::from(vec![
            "✘ ".red(),
            format!("on profile '{}', should be '{}'", have.name, want.name)
                .red()
                .bold(),
        ]),
        (Some((want, _)), None) => Line::from(vec![
            "✘ ".red(),
            format!("matches no profile, should be '{}'", want.name)
                .red()
                .bold(),
        ]),
        (_, Some(p)) => Line::from(vec![
            "✔ ".green(),
            format!("matches profile '{}'", p.name).green(),
        ]),
        (None, None) if app.config.profiles.is_empty() => {
            Line::from("no profiles configured".dim())
        }
        (None, None) => Line::from(vec!["✘ ".red(), "does not match any profile".red().bold()]),
    };
    let rule_note = match &expected {
        Some((want, why)) => {
            Line::from(format!("rule: '{}' claims this repo ({why})", want.name).dim())
        }
        None => Line::from("no profile rule claims this repo".dim()),
    };

    let local_note = if repo.local.name.is_none() && repo.local.email.is_none() {
        Line::from("identity comes from global config".dim())
    } else {
        Line::from("identity is set in this repo's .git/config".dim())
    };

    let mut lines = vec![
        field("root", &Some(repo.root.display().to_string())),
        field("remote", &repo.remote),
        Line::from(""),
        Line::from("Effective identity".bold()),
        field("name", &repo.effective.name),
        field("email", &repo.effective.email),
        field("signing key", &repo.effective.signing_key),
        field("ssh", &repo.effective.ssh_command),
    ];
    if let Some(summary) = signing_summary(repo) {
        lines.push(field("signing", &Some(summary)));
    }
    lines.extend([Line::from(""), verdict, rule_note, local_note]);
    lines.extend(history_lines(repo, app.reference_email()));

    let text = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(text, area);
}

fn signing_summary(repo: &Repo) -> Option<String> {
    let id = &repo.effective;
    if id.signing_format.is_none() && id.sign_commits.is_none() {
        return None;
    }
    let format = id
        .signing_format
        .unwrap_or(crate::profile::SigningFormat::Openpgp)
        .as_git();
    let sign = if id.sign_commits.unwrap_or(false) {
        "commits signed"
    } else {
        "commits not signed"
    };
    Some(format!("{format}, {sign}"))
}

fn history_lines(repo: &Repo, reference: Option<&str>) -> Vec<Line<'static>> {
    let inspected = repo.commits_inspected();
    if inspected == 0 {
        return Vec::new();
    }
    let foreign = repo.foreign_authors(reference);
    let reference = reference.unwrap_or("(not set)");
    let mut lines = vec![Line::from(""), Line::from("Recent commits".bold())];
    if foreign.is_empty() {
        lines.push(Line::from(
            format!("last {inspected} all authored as {reference}").dim(),
        ));
        return lines;
    }
    let count: usize = foreign.iter().map(|a| a.commits).sum();
    lines.push(Line::from(vec![
        "⚠ ".yellow(),
        format!("{count} of the last {inspected} were not authored as {reference}:").yellow(),
    ]));
    for author in foreign.iter().take(3) {
        lines.push(Line::from(
            format!(
                "  {:>3}  {} <{}>",
                author.commits, author.name, author.email
            )
            .yellow(),
        ));
    }
    if foreign.len() > 3 {
        lines.push(Line::from(
            format!("  and {} more", foreign.len() - 3).dim(),
        ));
    }
    lines
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.status {
        Some(msg) => Line::from(msg.clone().cyan()),
        None if matches!(app.mode, Mode::Scan(_)) => Line::from(vec![
            " ↑↓/jk ".bold(),
            "select  ".dim(),
            " Enter ".bold(),
            "apply the rule's profile  ".dim(),
            " o ".bold(),
            "open  ".dim(),
            " r ".bold(),
            "rescan  ".dim(),
            " Esc ".bold(),
            "back  ".dim(),
            " q ".bold(),
            "quit".dim(),
        ]),
        None => Line::from(vec![
            " ↑↓/jk ".bold(),
            "select  ".dim(),
            " Enter ".bold(),
            "apply to repo  ".dim(),
            " e ".bold(),
            "edit  ".dim(),
            " n ".bold(),
            "new  ".dim(),
            " d ".bold(),
            "delete  ".dim(),
            " ? ".bold(),
            "help  ".dim(),
            " q ".bold(),
            "quit".dim(),
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    //! Snapshot tests: render each screen into a fake terminal and compare
    //! with the files in `src/snapshots/`. After an intended visual change,
    //! run `cargo insta review` (or `INSTA_UPDATE=always cargo test`).

    use super::*;
    use crate::git::{Author, Identity, Repo};
    use crate::profile::{Config, Profile};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn profiles() -> Vec<Profile> {
        vec![
            Profile {
                name: "personal".into(),
                user_name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                ..Default::default()
            },
            Profile {
                name: "acme".into(),
                user_name: "Ada Lovelace".into(),
                email: "ada@acme.example".into(),
                signing_key: Some("ABCD1234".into()),
                remotes: vec!["github.com/acme/*".into()],
                ..Default::default()
            },
        ]
    }

    fn repo() -> Repo {
        Repo {
            root: "/home/ada/work/acme/api".into(),
            remote: Some("git@github.com:acme/api.git".into()),
            effective: Identity {
                name: Some("Ada Lovelace".into()),
                email: Some("ada@example.com".into()),
                ..Default::default()
            },
            local: Identity::default(),
            authors: vec![
                Author {
                    name: "Ada Lovelace".into(),
                    email: "ada@example.com".into(),
                    commits: 17,
                },
                Author {
                    name: "Ada Lovelace".into(),
                    email: "ada@acme.example".into(),
                    commits: 3,
                },
            ],
        }
    }

    fn app(profiles: Vec<Profile>, repo: Option<Repo>) -> App {
        App {
            config: Config { profiles },
            config_path: "/home/ada/.config/gitenv/config.toml".into(),
            repo,
            selected: 1,
            status: None,
            mode: Mode::Normal,
            should_quit: false,
            scan_root: "/home/ada/work".into(),
        }
    }

    fn draw(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(96, 26)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn wrong_profile_with_foreign_authors() {
        insta::assert_snapshot!(draw(&app(profiles(), Some(repo()))));
    }

    #[test]
    fn on_the_right_profile() {
        let mut repo = repo();
        repo.effective.email = Some("ada@acme.example".into());
        repo.effective.signing_key = Some("ABCD1234".into());
        repo.local = repo.effective.clone();
        repo.authors.truncate(1);
        repo.authors[0].email = "ada@acme.example".into();
        let mut app = app(profiles(), Some(repo));
        app.status = Some("Applied profile 'acme' to this repo".into());
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn no_profiles_and_no_repo() {
        let mut app = app(Vec::new(), None);
        app.selected = 0;
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn help_popup() {
        let mut app = app(profiles(), Some(repo()));
        app.mode = Mode::Help;
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn form_with_cursor_mid_field() {
        let mut app = app(profiles(), Some(repo()));
        let mut form = Form::edit(1, &profiles()[1]);
        form.next_field();
        form.next_field();
        form.left();
        form.left();
        form.left();
        form.error = Some("Git user.email must look like an email address".into());
        app.mode = Mode::Edit(Box::new(form));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn confirm_delete_popup() {
        let mut app = app(profiles(), Some(repo()));
        app.mode = Mode::ConfirmDelete;
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn scan_view() {
        use crate::check::Verdict;
        use crate::rules::Reason;
        use crate::scan::Row;
        let row = |root: &str, verdict: Verdict| Row {
            repo: Repo {
                root: root.into(),
                ..repo()
            },
            verdict,
            fixed: None,
        };
        let mut app = app(profiles(), Some(repo()));
        app.mode = Mode::Scan(Box::new(ScanView {
            root: "/home/ada/work".into(),
            rows: vec![
                row(
                    "/home/ada/work/acme/api",
                    Verdict::WrongProfile {
                        root: "/home/ada/work/acme/api".into(),
                        expected: "acme".into(),
                        reason: Reason::Remote("github.com/acme/*".into()),
                        actual: Some("personal".into()),
                        identity: Identity::default(),
                    },
                ),
                row("/home/ada/work/acme/web", Verdict::Match("acme".into())),
                row(
                    "/home/ada/work/scratch",
                    Verdict::Mismatch {
                        root: "/home/ada/work/scratch".into(),
                        identity: Identity::default(),
                        no_profiles: false,
                    },
                ),
            ],
            selected: 1,
        }));
        insta::assert_snapshot!(draw(&app));
    }
}
