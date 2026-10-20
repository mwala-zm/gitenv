use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, Mode};
use crate::form::{Form, LABEL_WIDTH, LABELS};
use crate::git::Repo;

pub fn render(frame: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);

    render_header(frame, header);
    render_profiles(frame, left, app);
    render_repo(frame, right, app);
    render_footer(frame, footer, app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Help => render_help(frame),
        Mode::Edit(form) => render_form(frame, form),
        Mode::ConfirmDelete => render_confirm_delete(frame, app),
    }
}

/// A rectangle of the given size centred in `area`, clamped to fit.
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
        ("?  h", "show this help"),
        ("q  Esc", "quit"),
        ("", ""),
        ("Applying writes user.name, user.email and, when set,", ""),
        ("user.signingkey and core.sshCommand into .git/config.", ""),
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

    let area = centered(frame.area(), 62, lines.len() as u16 + 2);
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
            Line::from(vec![
                Span::styled(format!("{label:<label_width$}"), style),
                Span::raw(tail(value, value_width)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(match &form.error {
        Some(err) => Line::from(err.clone().red()),
        None => Line::from("Tab/↓ next field   Enter save   Esc cancel".dim()),
    });

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(form.title())),
        area,
    );

    // Put the terminal cursor at the end of the field being typed into.
    let shown = form.values[form.field].chars().count().min(value_width) as u16;
    let x = area.x + 1 + label_width as u16 + shown;
    let y = area.y + 1 + form.field as u16;
    if x < area.right() {
        frame.set_cursor_position((x, y));
    }
}

/// The last `width` characters of `s`, so the end of a long value stays
/// visible while typing.
fn tail(s: &str, width: usize) -> String {
    let len = s.chars().count();
    s.chars().skip(len.saturating_sub(width)).collect()
}

fn render_header(frame: &mut Frame, area: Rect) {
    let title = Line::from(vec![
        " gitenv ".bold().reversed(),
        " right identity, right repo".dim(),
    ]);
    frame.render_widget(Paragraph::new(title), area);
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
        return;
    }

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
        Line::from(""),
        verdict,
        rule_note,
        local_note,
    ];
    lines.extend(history_lines(repo));

    let text = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(text, area);
}

/// The author audit: were recent commits made with a different identity?
fn history_lines(repo: &Repo) -> Vec<Line<'static>> {
    let inspected = repo.commits_inspected();
    if inspected == 0 {
        return Vec::new();
    }
    let foreign = repo.foreign_authors();
    let mut lines = vec![Line::from(""), Line::from("Recent commits".bold())];
    if foreign.is_empty() {
        lines.push(Line::from(
            format!("last {inspected} all authored with the current email").dim(),
        ));
        return lines;
    }
    let count: usize = foreign.iter().map(|a| a.commits).sum();
    lines.push(Line::from(vec![
        "⚠ ".yellow(),
        format!("{count} of the last {inspected} were authored with another email:").yellow(),
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
