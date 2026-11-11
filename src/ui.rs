use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, CloneChoice, CloneView, DoctorView, FixView, Mode, ReportView, ScanView, Setup,
    SetupAction,
};
use crate::doctor::Level;
use crate::export::Includes;
use crate::filter::Filter;
use crate::form::{Check, Form, LABEL_WIDTH, LABELS};
use crate::git::Repo;
use crate::hook::HookStatus;
use crate::rules;
use crate::scan;

pub fn render(frame: &mut Frame, app: &App) {
    draw(frame, app);
    app.theme.paint(frame.buffer_mut());
}

/// Where each part of the screen goes, shared by drawing and by the mouse
/// so a click lands on what was drawn there.
pub struct Areas {
    pub header: Rect,
    pub body: Rect,
    pub footer: Rect,
    pub left: Rect,
    pub right: Rect,
}

pub fn areas(screen: Rect) -> Areas {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(screen);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);
    Areas {
        header,
        body,
        footer,
        left,
        right,
    }
}

/// The fix-authors view's heading, commit list and summary line.
pub fn fix_parts(body: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(Block::bordered().inner(body))
}

/// Which entry of a list drawn in `inner` row `y` shows. A list redrawn each
/// frame scrolls only as far as it must to show the `selected` entry last.
pub fn list_row(inner: Rect, selected: Option<usize>, y: u16) -> Option<usize> {
    if y < inner.y || y >= inner.y + inner.height {
        return None;
    }
    let offset = selected.map_or(0, |s| (s + 1).saturating_sub(inner.height as usize));
    Some(offset + (y - inner.y) as usize)
}

fn draw(frame: &mut Frame, app: &App) {
    let Areas {
        header,
        body,
        footer,
        left,
        right,
    } = areas(frame.area());

    render_header(frame, header);
    if let Mode::Scan(view) = &app.mode {
        render_scan(frame, body, view);
        render_footer(frame, footer, app);
        if view.confirming {
            render_confirm_scan(frame, view);
        }
        return;
    }
    if let Mode::Report(view) = &app.mode {
        render_report(frame, body, app, view);
        render_footer(frame, footer, app);
        return;
    }
    if let Mode::FixAuthors(view) = &app.mode {
        render_fix_authors(frame, body, app, view);
        render_footer(frame, footer, app);
        if view.confirming {
            render_confirm_fix(frame, view);
        }
        return;
    }

    render_profiles(frame, left, app);
    render_repo(frame, right, app);
    render_footer(frame, footer, app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Help => render_help(frame),
        Mode::Edit(form) => render_form(frame, form, app),
        Mode::ConfirmDelete => render_confirm_delete(frame, app),
        Mode::Doctor(view) => render_doctor(frame, view),
        Mode::ConfirmSetup(action) => render_confirm_setup(frame, *action, app),
        Mode::Why => render_why(frame, app),
        Mode::Clone(view) => render_clone(frame, view, app),
        Mode::Scan(_) | Mode::FixAuthors(_) | Mode::Report(_) => {}
    }
}

/// ` /query ` for a list's title while a filter is set, with a cursor
/// while it is being typed.
fn filter_title(filter: &Filter) -> String {
    match (filter.typing, filter.query.is_empty()) {
        (true, _) => format!(" /{}█ ", filter.query),
        (false, false) => format!(" /{} ", filter.query),
        (false, true) => String::new(),
    }
}

fn render_scan(frame: &mut Frame, area: Rect, view: &ScanView) {
    let ok = view.rows.iter().filter(|r| r.is_ok()).count();
    let marked = view.to_fix().len();
    let mut title = format!(
        " Repositories under {}: {ok} of {} on the right profile ",
        scan::tildify(&view.root),
        view.rows.len(),
    );
    if marked > 0 {
        title.push_str(&format!("· {marked} marked "));
    }
    if view.wrong_only {
        title.push_str("· problems only ");
    }
    title.push_str(&filter_title(&view.filter));
    let visible = view.visible();
    let width = view
        .rows
        .iter()
        .map(|r| r.profile_column().chars().count())
        .max()
        .unwrap_or(0)
        .max("PROFILE".len());
    let mut items: Vec<ListItem> = visible
        .iter()
        .map(|&i| (&view.rows[i], view.marked.get(i).copied().unwrap_or(false)))
        .map(|(row, marked)| {
            let mark = match (row.is_fixable(), marked) {
                (true, true) => "[x] ".green(),
                (true, false) => "[ ] ".into(),
                (false, _) => "    ".into(),
            };
            let word = row.status_word();
            let colour = match word {
                "ok" | "fixed" => Color::Green,
                "wrong" => Color::Red,
                _ => Color::Yellow,
            };
            ListItem::new(Line::from(vec![
                mark,
                Span::styled(format!("{word:<6}"), Style::new().fg(colour)),
                Span::raw(format!("{:<width$}  ", row.profile_column())),
                Span::styled(shown_path(view, &row.repo.root), Style::new().dim()),
            ]))
        })
        .collect();
    if items.is_empty() {
        items.push(ListItem::new(Line::from(
            format!("No repository matches /{}", view.filter.query).dim(),
        )));
    }
    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_style(
            Style::new()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(visible.iter().position(|&i| i == view.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn short_hash(hash: &str) -> &str {
    &hash[..10.min(hash.len())]
}

fn commit_line(hash: &str, email: &str, subject: &str, width: usize) -> Vec<Span<'static>> {
    vec![
        Span::raw(format!("{}  ", short_hash(hash))),
        Span::raw(format!("{email:<width$}  ")),
        Span::raw(subject.to_string()),
    ]
}

/// The unpushed commits, oldest first as they are rewritten, with a tick box
/// on each one made as someone else, then the pushed ones made as someone
/// else, which are left alone.
fn render_fix_authors(frame: &mut Frame, area: Rect, app: &App, view: &FixView) {
    let repo = app
        .repo
        .as_ref()
        .map(|r| scan::tildify(&r.root))
        .unwrap_or_default();
    let branch = view.plan.branch().unwrap_or("detached HEAD");
    let title = format!(" Fix authors · {repo} · {branch} ");
    let width = view
        .plan
        .unpushed
        .iter()
        .chain(&view.plan.pushed_wrong)
        .map(|c| c.email.chars().count())
        .max()
        .unwrap_or(0);

    let [head, list_area, summary] = fix_parts(area);
    frame.render_widget(Block::bordered().title(title), area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            "Re-author as ".into(),
            view.profile.name.clone().bold(),
            format!(": {} <{}>", view.profile.user_name, view.profile.email).into(),
            format!("  ({})", view.why).dim(),
        ])),
        head,
    );

    let mut items = Vec::new();
    let mut selected = None;
    let mut wrong = 0;
    for commit in &view.plan.unpushed {
        let is_wrong = view.plan.wrong.iter().any(|w| w.hash == commit.hash);
        let email = if is_wrong && commit.email.eq_ignore_ascii_case(&view.profile.email) {
            format!("{} (committer)", commit.committer_email)
        } else {
            commit.email.clone()
        };
        let mut spans = if is_wrong {
            let ticked = view.ticked.get(wrong).copied().unwrap_or(false);
            if wrong == view.cursor {
                selected = Some(items.len());
            }
            wrong += 1;
            vec![if ticked {
                "[x] ".green()
            } else {
                "[ ] ".into()
            }]
        } else {
            vec!["    ".into()]
        };
        let line = commit_line(&commit.hash, &email, &commit.subject, width);
        if is_wrong {
            spans.extend(line.into_iter().map(|s| s.yellow()));
        } else {
            spans.extend(line.into_iter().map(|s| s.dim()));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    if !view.plan.pushed_wrong.is_empty() {
        items.push(ListItem::new(Line::from("")));
        items.push(ListItem::new(Line::from(
            "── already pushed, left alone ──".dim(),
        )));
        for commit in &view.plan.pushed_wrong {
            let mut spans = vec![Span::raw("    ")];
            spans.extend(
                commit_line(&commit.hash, &commit.email, &commit.subject, width)
                    .into_iter()
                    .map(|s| s.dim()),
            );
            items.push(ListItem::new(Line::from(spans)));
        }
    }
    let list = List::new(items)
        .highlight_style(
            Style::new()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, list_area, &mut state);

    let line = match &view.blocked {
        Some(why) => Line::from(vec!["Cannot rewrite: ".red().bold(), why.clone().red()]),
        None => {
            let ticked = view.ticked.iter().filter(|t| **t).count();
            let extra = view.rebuilt().saturating_sub(ticked);
            Line::from(
                format!(
                    "{ticked} re-authored · {extra} more get new hashes · old ones stay in git reflog"
                )
                .dim(),
            )
        }
    };
    frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }), summary);
}

fn render_confirm_fix(frame: &mut Frame, view: &FixView) {
    let ticked = view.ticked.iter().filter(|t| **t).count();
    let branch = view.plan.branch().unwrap_or("detached HEAD");
    let mut lines = vec![
        Line::from(format!("Rewrite {} commit(s) on {branch}?", view.rebuilt()).bold()),
        Line::from(""),
        Line::from(format!("{ticked} get '{}' as author.", view.profile.name).dim()),
        Line::from("The old commits stay in git reflog.".dim()),
    ];
    if view.signs {
        lines.push(Line::from(
            "They are signed, so you may be asked for a passphrase.".dim(),
        ));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            " y / Enter ".bold(),
            "rewrite   ".dim(),
            " any other key ".bold(),
            "cancel".dim(),
        ]),
    ]);
    let area = centered(frame.area(), 60, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Fix authors ")),
        area,
    );
}

fn render_confirm_scan(frame: &mut Frame, view: &ScanView) {
    let todo = view.to_fix();
    let noun = if todo.len() == 1 {
        "repository"
    } else {
        "repositories"
    };
    let mut lines = vec![
        Line::from(format!("Fix {} {noun}?", todo.len()).bold()),
        Line::from(""),
    ];
    const SHOWN: usize = 8;
    for &i in todo.iter().take(SHOWN) {
        let row = &view.rows[i];
        lines.push(Line::from(vec![
            Span::raw(format!(
                "{:<28} ",
                short_path(&shown_path(view, &row.repo.root), 28)
            )),
            row.profile_column().yellow(),
        ]));
    }
    if todo.len() > SHOWN {
        lines.push(Line::from(format!("and {} more", todo.len() - SHOWN).dim()));
    }
    lines.extend([
        Line::from(""),
        Line::from("Each gets the profile its rules name.".dim()),
        Line::from("gh is left alone, since its account is global.".dim()),
        Line::from(""),
        Line::from(vec![
            " y / Enter ".bold(),
            "fix   ".dim(),
            " any other key ".bold(),
            "cancel".dim(),
        ]),
    ]);
    let area = centered(frame.area(), 64, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Fix repositories ")),
        area,
    );
}

/// A repository's path as the list shows it: relative to the folder
/// scanned, or from `~` when it is that folder itself.
fn shown_path(view: &ScanView, root: &std::path::Path) -> String {
    match root.strip_prefix(&view.root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => scan::tildify(root),
    }
}

/// `path` cut to `width` characters from the left, keeping its end, which
/// names the repository.
fn short_path(path: &str, width: usize) -> String {
    let count = path.chars().count();
    if count <= width {
        return path.to_string();
    }
    let tail: String = path.chars().skip(count - (width - 1)).collect();
    format!("…{tail}")
}

/// Your commits in the period, a bar and a count per profile and the
/// repositories under each, marking commits made as another identity.
fn render_report(frame: &mut Frame, area: Rect, app: &App, view: &ReportView) {
    use crate::report::{groups, plural};
    let period = &view.periods[view.period];
    let title = format!(
        " Your commits in {} under {} ",
        period.label,
        scan::tildify(&view.root)
    );
    let groups = groups(&app.config, &view.reports);
    let totals: Vec<usize> = groups
        .iter()
        .map(|(_, members)| members.iter().map(|r| r.commits.len()).sum())
        .collect();
    let most = totals.iter().copied().max().unwrap_or(0).max(1);
    let name_width = groups
        .iter()
        .map(|(name, _)| name.as_deref().unwrap_or("(no profile)").chars().count())
        .max()
        .unwrap_or(0);
    const BAR: usize = 30;
    let mut lines = Vec::new();
    let mut slips_total = 0;
    for ((name, members), total) in groups.iter().zip(&totals) {
        let profile = name
            .as_deref()
            .and_then(|n| app.config.profiles.iter().find(|p| p.name == n));
        let filled = (BAR * total).div_ceil(most).max(1);
        lines.push(Line::from(vec![
            format!(
                "{:<name_width$}  ",
                name.as_deref().unwrap_or("(no profile)")
            )
            .bold(),
            "█".repeat(filled).cyan(),
            " ".repeat(BAR - filled + 1).into(),
            plural(*total, "commit").into(),
        ]));
        for report in members {
            let path = match report.root.strip_prefix(&view.root) {
                Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
                _ => scan::tildify(&report.root),
            };
            let mut spans = vec![
                Span::raw(format!("    {path:<width$} ", width = name_width + BAR - 2)),
                Span::raw(format!("{:>4}", report.commits.len())),
            ];
            if let Some(profile) = profile {
                let mut others: Vec<&str> = report
                    .commits
                    .iter()
                    .filter(|c| !c.email.eq_ignore_ascii_case(&profile.email))
                    .map(|c| c.email.as_str())
                    .collect();
                let slips = others.len();
                others.sort_unstable();
                others.dedup();
                if slips > 0 {
                    slips_total += slips;
                    spans.push(format!("  {slips} as {}", others.join(", ")).yellow());
                }
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
    }
    if view.reports.is_empty() {
        lines.push(Line::from(
            format!("No commits of yours in {}.", period.label).dim(),
        ));
    } else {
        let total: usize = totals.iter().sum();
        let mut summary = format!(
            "{} in {}",
            plural(total, "commit"),
            plural(view.reports.len(), "repository").replace("repositorys", "repositories")
        );
        if slips_total > 0 {
            summary.push_str(&format!("; {slips_total} made as another identity"));
        }
        lines.push(Line::from(summary.dim()));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((view.scroll, 0))
            .block(Block::bordered().title(title)),
        area,
    );
}

/// The clone dialog: the URL, where it goes, and the profile it will get,
/// which follows the URL's remote rules until another is picked.
fn render_clone(frame: &mut Frame, view: &CloneView, app: &App) {
    let width: u16 = 80.min(frame.area().width);
    let label_width = 12;
    let value_width = (width as usize).saturating_sub(2 + label_width + 1);
    let style = |row: usize| {
        if view.row == row {
            Style::new().bold().fg(Color::Cyan)
        } else {
            Style::new()
        }
    };
    let field = |row: usize, label: &str, value: &str| {
        Line::from(vec![
            Span::styled(format!("{label:<label_width$}"), style(row)),
            Span::raw(
                visible(
                    value,
                    (view.row == row).then_some(value.chars().count()),
                    value_width,
                )
                .0,
            ),
        ])
    };
    let profile = match (view.choice, view.profile(&app.config)) {
        (CloneChoice::Nobody, _) => "none: git's own identity".to_string(),
        (CloneChoice::Rules, None) if view.url.trim().is_empty() => {
            "the one the URL's rules name".to_string()
        }
        (CloneChoice::Rules, None) => "no rule claims this URL; → picks one".to_string(),
        (CloneChoice::Rules, Some((p, why))) => format!("{} ({why})", p.name),
        (CloneChoice::Named(_), Some((p, _))) => format!("{} (picked)", p.name),
        (CloneChoice::Named(_), None) => String::new(),
    };
    let mut lines = vec![
        field(0, "URL", &view.url),
        field(1, "Directory", &view.dir),
        Line::from(vec![
            Span::styled(format!("{:<label_width$}", "Profile"), style(2)),
            "‹ ".dim(),
            profile.into(),
            " ›".dim(),
        ]),
        Line::from(""),
    ];
    match &view.error {
        Some(err) => lines.extend(
            wrap(err, (width as usize).saturating_sub(2))
                .into_iter()
                .map(|l| Line::from(l.red())),
        ),
        None => lines.push(Line::from(
            "Tab next   ←→ change profile   Enter clone   Esc cancel".dim(),
        )),
    }
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Clone a repository ")),
        area,
    );
    if view.row < 2 {
        let value = if view.row == 0 { &view.url } else { &view.dir };
        let (_, column) = visible(value, Some(value.chars().count()), value_width);
        let x = area.x + 1 + label_width as u16 + column as u16;
        if x < area.right() {
            frame.set_cursor_position((x, area.y + 1 + view.row as u16));
        }
    }
}

/// Every rule that matches the repository, the one that claims it first and
/// each other with why it lost, then what the rules were matched against;
/// with no match, rules that would claim it.
fn render_why(frame: &mut Frame, app: &App) {
    let Some(repo) = &app.repo else {
        return;
    };
    let width: u16 = 80;
    let text_width = width as usize - 2;
    let found = rules::matching(&app.config, repo);
    let subjects = rules::remote_subjects(repo);
    let root = scan::tildify(&repo.root);
    let mut lines = Vec::new();
    match found.first() {
        None => {
            lines.push(Line::from("No rule claims this repository.".bold()));
            lines.push(Line::from(""));
            let mut ideas: Vec<String> = subjects
                .iter()
                .filter_map(|s| {
                    let mut parts = s.split('/');
                    let host = parts.next()?;
                    let owner = parts.next()?;
                    Some(format!("{host}/{owner}/*"))
                })
                .collect();
            ideas.push(format!("{root}/**"));
            ideas.dedup();
            for line in wrap(
                &format!(
                    "A rule like {} in a profile would claim it; N starts a profile with one.",
                    ideas.join(" or ")
                ),
                text_width,
            ) {
                lines.push(Line::from(line.dim()));
            }
        }
        Some(winner) => {
            let name_width = found
                .iter()
                .map(|r| r.profile.name.chars().count())
                .max()
                .unwrap_or(0);
            lines.push(Line::from(vec![
                "'".into(),
                winner.profile.name.clone().bold(),
                "' claims this repository. Rules that match, strongest first:".into(),
            ]));
            lines.push(Line::from(""));
            for (i, rule) in found.iter().enumerate() {
                let head = format!(
                    "{:<name_width$}  {:<6}  {}",
                    rule.profile.name,
                    rule.reason.kind(),
                    rule.reason.pattern()
                );
                if i == 0 {
                    lines.push(Line::from(vec!["✔ ".green(), head.green().bold()]));
                } else {
                    lines.push(Line::from(vec!["  ".into(), head.into()]));
                    for part in wrap(&rules::why_lost(winner, rule), text_width - 4) {
                        lines.push(Line::from(format!("    {part}").dim()));
                    }
                }
            }
        }
    }
    lines.push(Line::from(""));
    let label = |l: &str| Span::styled(format!("{l:<8}"), Style::new().dim());
    let remote = if subjects.is_empty() {
        "(no remote)".to_string()
    } else {
        subjects.join(", ")
    };
    lines.push(Line::from(vec![label("remote"), remote.into()]));
    lines.push(Line::from(vec![label("path"), root.into()]));
    lines.push(Line::from(""));
    lines.push(Line::from("any key closes".dim()));
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Why this profile? ")),
        area,
    );
}

/// Doctor's findings for one profile, each wrapped under its level, with a
/// note while the logins are still being checked.
fn render_doctor(frame: &mut Frame, view: &DoctorView) {
    let width: u16 = 72;
    let text_width = width as usize - 2 - 6;
    let mut lines = Vec::new();
    for f in &view.findings {
        let word = match f.level {
            Level::Ok => "ok  ".green(),
            Level::Warn => "warn".yellow(),
            Level::Fail => "fail".red(),
        };
        for (i, part) in wrap(&f.message, text_width).into_iter().enumerate() {
            let lead = if i == 0 { word.clone() } else { "    ".into() };
            lines.push(Line::from(vec![lead, "  ".into(), part.into()]));
        }
    }
    if view.checking {
        lines.push(Line::from("…     checking logins over ssh and gh".dim()));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            " r ".bold(),
            "check again   ".dim(),
            " any other key ".bold(),
            "close".dim(),
        ]),
    ]);
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(format!(" Doctor: {} <{}> ", view.name, view.email))),
        area,
    );
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
        ("u", "undo the last apply or fix"),
        ("e", "edit the selected profile"),
        ("n", "create a new profile"),
        ("N", "create a profile from this repo's identity and remote"),
        ("d", "delete the selected profile"),
        ("r", "re-read the repo and the profiles file"),
        ("f", "re-author unpushed commits made as someone else"),
        ("/", "filter the list; Enter keeps it, Esc clears it"),
        ("D", "check the selected profile's keys and logins"),
        ("H", "install the hooks that check every commit"),
        ("I", "let git apply your rules by itself"),
        ("w", "why this repo belongs to the profile it does"),
        ("c", "clone a repository onto the right profile"),
        ("R", "your commits by profile, this month and others"),
        (
            "s",
            "list every repository next to this one, or go back to it",
        ),
        ("?  h", "show this help"),
        ("q  Esc", "quit"),
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

fn render_form(frame: &mut Frame, form: &Form, app: &App) {
    let label_width = LABEL_WIDTH;
    let width = 78.min(frame.area().width);
    let text_width = (width as usize).saturating_sub(2);
    let mut notes: Vec<Line> = Vec::new();
    match (&form.error, form.check(form.field)) {
        (Some(err), _) => notes.extend(
            wrap(err, text_width)
                .into_iter()
                .map(|l| Line::from(l.red())),
        ),
        (None, Some(Check::Bad(why))) => notes.extend(
            wrap(&format!("✘ {why}"), text_width)
                .into_iter()
                .map(|l| Line::from(l.red())),
        ),
        (None, Some(Check::Good(what))) => notes.extend(
            wrap(&format!("✔ {what}"), text_width)
                .into_iter()
                .map(|l| Line::from(l.green())),
        ),
        (None, None) => {}
    }
    if matches!(form.field, crate::form::REMOTES | crate::form::PATHS)
        && let Some(repos) = &app.known_repos
    {
        notes.extend(preview_lines(form, app, repos, text_width));
    }
    let pick = if form.pickable() {
        "   Ctrl-O pick"
    } else {
        ""
    };
    notes.push(Line::from(
        format!("Tab/↓ next   ←→ move   Enter save   Esc cancel{pick}").dim(),
    ));
    let area = centered(
        frame.area(),
        width,
        LABELS.len() as u16 + 3 + notes.len() as u16,
    );
    let value_width = (area.width as usize).saturating_sub(2 + label_width + 1);

    let mut lines: Vec<Line> = LABELS
        .iter()
        .zip(form.values.iter())
        .enumerate()
        .map(|(i, (label, value))| {
            let bad = matches!(form.check(i), Some(Check::Bad(_)));
            let style = match (i == form.field, bad) {
                (_, true) => Style::new().bold().fg(Color::Red),
                (true, false) => Style::new().bold().fg(Color::Cyan),
                (false, false) => Style::new(),
            };
            let cursor = (i == form.field).then_some(form.cursor);
            Line::from(vec![
                Span::styled(format!("{label:<label_width$}"), style),
                Span::raw(visible(value, cursor, value_width).0),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.extend(notes);

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(form.title())),
        area,
    );

    let y = area.y + 1 + form.field as u16;
    if let Some(picker) = &form.picker {
        render_picker(frame, picker, area, y);
        return;
    }
    let (_, column) = visible(&form.values[form.field], Some(form.cursor), value_width);
    let x = area.x + 1 + label_width as u16 + column as u16;
    if x < area.right() {
        frame.set_cursor_position((x, y));
    }
}

/// Which of the repositories next to this one the rules being typed would
/// claim, and which stay with a more specific or earlier rule of another
/// profile.
fn preview_lines(
    form: &Form,
    app: &App,
    repos: &[crate::git::Repo],
    width: usize,
) -> Vec<Line<'static>> {
    let preview = form.preview(&app.config, repos);
    let shown = |path: &std::path::Path| match path.strip_prefix(&app.scan_root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => scan::tildify(path),
    };
    let listed = |lead: String, names: Vec<String>| {
        let mut text = lead;
        for (i, name) in names.iter().enumerate() {
            let more = format!(" +{} more", names.len() - i);
            let piece = if i == 0 {
                format!(" {name}")
            } else {
                format!(", {name}")
            };
            if text.chars().count() + piece.chars().count() + more.len() > width {
                text.push_str(&more);
                break;
            }
            text.push_str(&piece);
        }
        text
    };
    let mut lines = Vec::new();
    if preview.claimed.is_empty() && preview.lost.is_empty() {
        lines.push(Line::from(
            format!(
                "none of the {} nearby repositories matches these rules",
                repos.len()
            )
            .dim(),
        ));
        return lines;
    }
    if !preview.claimed.is_empty() {
        let names = preview.claimed.iter().map(|p| shown(p)).collect();
        let lead = format!(
            "✔ claims {} of {} nearby repos:",
            preview.claimed.len(),
            repos.len()
        );
        lines.push(Line::from(listed(lead, names).green()));
    }
    if !preview.lost.is_empty() {
        let names = preview
            .lost
            .iter()
            .map(|(p, by)| format!("{} ({by})", shown(p)))
            .collect();
        let lead = format!(
            "! {} stay with a stronger or earlier rule:",
            preview.lost.len()
        );
        lines.push(Line::from(listed(lead, names).yellow()));
    }
    lines
}

/// The values to pick from, just below the field, over the rest of the form.
fn render_picker(frame: &mut Frame, picker: &crate::form::Picker, form: Rect, row: u16) {
    const SHOWN: usize = 8;
    let height = picker.items.len().min(SHOWN) as u16 + 2;
    let x = form.x + 1 + LABEL_WIDTH as u16 - 1;
    let width = form.right().saturating_sub(x + 1);
    let screen = frame.area();
    let y = (row + 1).min(screen.bottom().saturating_sub(height));
    let area = Rect::new(x, y, width, height).intersection(screen);
    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|(_, label)| ListItem::new(label.clone()))
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(" ↑↓ choose · Enter pick · Esc close "))
        .highlight_style(
            Style::new()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(picker.selected));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split(' ') {
        let line = lines.last_mut().unwrap();
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(word.to_string());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    lines
}

/// The part of `value` that fits in `width` columns, scrolled so the cursor (a
/// character index, if this field has it) stays in view with a spare cell, and
/// the cursor's column within it. Wide characters such as CJK take two columns.
fn visible(value: &str, cursor: Option<usize>, width: usize) -> (String, usize) {
    let chars: Vec<char> = value.chars().collect();
    let cols = |cs: &[char]| -> usize { cs.iter().map(|c| c.width().unwrap_or(0)).sum() };
    let cursor = cursor.map(|c| c.min(chars.len()));
    let mut offset = 0;
    if let Some(cursor) = cursor {
        let under = chars
            .get(cursor)
            .map_or(1, |c| c.width().unwrap_or(1).max(1));
        while offset < cursor && cols(&chars[offset..cursor]) + under > width {
            offset += 1;
        }
    }
    let mut shown = String::new();
    let mut used = 0;
    for &c in &chars[offset..] {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        shown.push(c);
    }
    let column = cursor.map_or(0, |cursor| cols(&chars[offset..cursor]));
    (shown, column)
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

    let visible = app.visible_profiles();
    let mut items: Vec<ListItem> = visible
        .iter()
        .map(|&i| (&app.config.profiles[i], app.badge(i)))
        .map(|(p, badge)| {
            let (marker, colour) = match (active == Some(p), expected == Some(p)) {
                (true, _) => ("● ", Color::Green),
                (false, true) => ("◆ ", Color::Yellow),
                (false, false) => ("  ", Color::Reset),
            };
            let badge = match badge {
                Some(Level::Fail) => " ✗".red(),
                Some(Level::Warn) => " !".yellow(),
                Some(Level::Ok) => " ✓".green().dim(),
                None => "".into(),
            };
            let line = Line::from(vec![
                Span::styled(marker, Style::new().fg(colour)),
                Span::raw(&p.name),
                badge,
                Span::raw("  "),
                Span::styled(&p.email, Style::new().dim()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let block = Block::bordered().title(format!(" Profiles {}", filter_title(&app.profile_filter)));
    let inner = block.inner(area);

    if items.is_empty() && app.profile_filter.is_active() {
        items.push(ListItem::new(Line::from(
            format!("No profile matches /{}", app.profile_filter.query).dim(),
        )));
    }
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
    state.select(visible.iter().position(|&i| i == app.selected));
    frame.render_stateful_widget(list, area, &mut state);
    render_wordmark(frame, inner, used_rows);
}

fn render_repo(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" Current repository ");

    let Some(repo) = &app.repo else {
        let mut lines = vec![Line::from("Not inside a git repository.".yellow())];
        lines.extend(setup_lines(app.setup.as_ref()));
        let text = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block);
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
    let expected_profile = expected.as_ref().map(|(p, _)| *p);
    let verdict = match (expected_profile, active) {
        (Some(want), Some(have)) if !repo.effective.matches(want) => Line::from(vec![
            "✘ ".red(),
            format!("on profile '{}', should be '{}'", have.name, want.name)
                .red()
                .bold(),
        ]),
        (Some(want), _) if !repo.effective.matches(want) => Line::from(vec![
            "✘ ".red(),
            format!("matches no profile, should be '{}'", want.name)
                .red()
                .bold(),
        ]),
        (Some(p), _) | (None, Some(p)) => Line::from(vec![
            "✔ ".green(),
            format!("matches profile '{}'", p.name).green(),
        ]),
        (None, None) if app.config.profiles.is_empty() => {
            Line::from("no profiles configured".dim())
        }
        (None, None) => Line::from(vec![
            "✘ ".red(),
            "does not match any profile".red().bold(),
            " · N makes one from it".dim(),
        ]),
    };
    let rule_note = match &expected {
        Some((want, why)) => {
            Line::from(format!("rule: '{}' claims this repo ({why})", want.name).dim())
        }
        None => Line::from("no profile rule claims this repo".dim()),
    };

    let local_note = Line::from(format!("identity comes from {}", repo.identity_source()).dim());

    let mut lines = vec![
        field("root", &Some(repo.root.display().to_string())),
        field("remote", &repo.remote.as_deref().map(rules::hide_password)),
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
    lines.extend(setup_lines(app.setup.as_ref()));

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

/// Whether the hooks check commits and git applies the rules by itself,
/// each with the key that puts it right.
fn setup_lines(setup: Option<&Setup>) -> Vec<Line<'static>> {
    let Some(setup) = setup else {
        return Vec::new();
    };
    let row = |label: &'static str, good: bool, text: String, hint: Option<&'static str>| {
        let mut spans = vec![
            Span::styled(format!("{label:<12}"), Style::new().dim()),
            if good {
                "✔ ".green()
            } else {
                "✘ ".yellow()
            },
            if good { text.into() } else { text.yellow() },
        ];
        if let Some(hint) = hint {
            spans.push(format!(" · {hint}").dim());
        }
        Line::from(spans)
    };
    let hooks = match setup.hooks {
        HookStatus::Everywhere => row("hooks", true, "check every commit and push".into(), None),
        HookStatus::ThisRepo => row(
            "hooks",
            true,
            "check this repository only".into(),
            Some("H for every one"),
        ),
        HookStatus::Missing => row(
            "hooks",
            false,
            "not installed".into(),
            Some("H installs them"),
        ),
    };
    let includes = match &setup.includes {
        Ok(Includes::Active) => row(
            "includes",
            true,
            "git applies your rules itself".into(),
            None,
        ),
        Ok(Includes::Overridden) => row(
            "includes",
            false,
            "later settings win".into(),
            Some("I moves them last"),
        ),
        Ok(Includes::Missing) => row(
            "includes",
            false,
            "not installed".into(),
            Some("I installs them"),
        ),
        Ok(Includes::NotExported) => row(
            "includes",
            false,
            "not set up".into(),
            Some("I sets them up"),
        ),
        Err(err) => row("includes", false, format!("could not check: {err}"), None),
    };
    vec![Line::from(""), Line::from("Setup".bold()), hooks, includes]
}

fn render_confirm_setup(frame: &mut Frame, action: SetupAction, app: &App) {
    let includes = crate::export::dir(&app.config_path).join(crate::export::INCLUDES);
    let (title, question, what): (&str, &str, Vec<String>) = match action {
        SetupAction::Hooks => (
            " Install hooks ",
            "Check every commit and push in every repository?",
            vec![
                "Runs `gitenv hook install --global`: writes the hooks into".into(),
                "your global core.hooksPath, setting it to ~/.githooks if".into(),
                "it is unset. Each repository's own hooks still run.".into(),
            ],
        ),
        SetupAction::Includes => (
            " Install includes ",
            "Let git pick the identity by itself?",
            vec![
                "Runs `gitenv export --install`: adds an include of".into(),
                scan::tildify(&includes),
                "to the end of your global git config, so a fresh clone".into(),
                "has the right identity before its first commit.".into(),
            ],
        ),
    };
    let mut lines = vec![Line::from(question.bold()), Line::from("")];
    lines.extend(what.into_iter().map(|l| Line::from(l.dim())));
    lines.extend([
        Line::from(""),
        Line::from(vec![
            " y / Enter ".bold(),
            "install   ".dim(),
            " any other key ".bold(),
            "cancel".dim(),
        ]),
    ]);
    let area = centered(frame.area(), 66, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let typing = match &app.mode {
        Mode::Scan(view) => view.filter.typing,
        Mode::Normal => app.profile_filter.typing,
        _ => false,
    };
    let line = match &app.status {
        Some(msg) => Line::from(msg.clone().cyan()),
        None if typing => Line::from(vec![
            " type ".bold(),
            "to filter  ".dim(),
            " ↑↓ ".bold(),
            "select  ".dim(),
            " Enter ".bold(),
            "keep the filter  ".dim(),
            " Esc ".bold(),
            "clear it".dim(),
        ]),
        None if matches!(app.mode, Mode::Scan(_)) => Line::from(vec![
            " space ".bold(),
            "mark  ".dim(),
            " a ".bold(),
            "all  ".dim(),
            " F ".bold(),
            "fix marked  ".dim(),
            " Enter ".bold(),
            "fix one  ".dim(),
            " o ".bold(),
            "open  ".dim(),
            " w ".bold(),
            "problems  ".dim(),
            " / ".bold(),
            "filter  ".dim(),
            " Esc ".bold(),
            "back".dim(),
        ]),
        None if matches!(app.mode, Mode::Report(_)) => Line::from(vec![
            " Tab/←→ ".bold(),
            "period  ".dim(),
            " ↑↓/jk ".bold(),
            "scroll  ".dim(),
            " Esc ".bold(),
            "back  ".dim(),
            " q ".bold(),
            "quit".dim(),
        ]),
        None if matches!(app.mode, Mode::FixAuthors(_)) => Line::from(vec![
            " ↑↓/jk ".bold(),
            "select  ".dim(),
            " space ".bold(),
            "tick  ".dim(),
            " a ".bold(),
            "all/none  ".dim(),
            " Enter ".bold(),
            "rewrite  ".dim(),
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
            " f ".bold(),
            "authors  ".dim(),
            " ? ".bold(),
            "help  ".dim(),
            if app.back_to_scan.is_some() {
                " Esc ".bold()
            } else {
                " q ".bold()
            },
            if app.back_to_scan.is_some() {
                "repos".dim()
            } else {
                "quit".dim()
            },
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Author, Identity, Repo};
    use crate::profile::Profile;
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
        let mut app = App::for_test(profiles, "/home/ada/.config/gitenv/config.toml".into());
        app.repo = repo;
        app.selected = 1;
        app.scan_root = "/home/ada/work".into();
        app
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

    fn fix_view() -> crate::app::FixView {
        use crate::fix::{Commit, Plan};
        let commit = |hash: &str, email: &str, subject: &str| Commit {
            hash: hash.repeat(40),
            author: "Ada Lovelace".into(),
            email: email.into(),
            committer: "Ada Lovelace".into(),
            committer_email: email.into(),
            subject: subject.into(),
            parents: 1,
        };
        let unpushed = vec![
            commit("3", "ada@example.com", "Add login form"),
            commit("8", "ada@example.com", "Fix typo in header"),
            commit("9", "bob@acme.example", "Pair session: tests"),
            commit("5", "ada@acme.example", "Bump deps"),
        ];
        let wrong = unpushed[..3].to_vec();
        let pushed = vec![commit("a", "ada@example.com", "Initial commit")];
        crate::app::FixView {
            profile: profiles()[1].clone(),
            why: "rule: remote matches github.com/acme/*".into(),
            plan: Plan::for_test(unpushed, wrong, pushed),
            ticked: vec![true, true, false],
            cursor: 2,
            blocked: None,
            signs: true,
            confirming: false,
        }
    }

    #[test]
    fn fix_authors_view() {
        let mut app = app(profiles(), Some(repo()));
        app.mode = Mode::FixAuthors(Box::new(fix_view()));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn fix_authors_confirm_popup() {
        let mut app = app(profiles(), Some(repo()));
        let mut view = fix_view();
        view.confirming = true;
        app.mode = Mode::FixAuthors(Box::new(view));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn fix_authors_blocked() {
        let mut app = app(profiles(), Some(repo()));
        let mut view = fix_view();
        view.blocked = Some("git is in the middle of a rebase; finish or abort it first".into());
        app.mode = Mode::FixAuthors(Box::new(view));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn filtered_profiles_while_typing() {
        let mut app = app(profiles(), Some(repo()));
        app.profile_filter.query = "acm".into();
        app.profile_filter.typing = true;
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn scan_view_with_marks_and_the_fix_popup() {
        use crate::check::Verdict;
        use crate::rules::Reason;
        use crate::scan::Row;
        let wrong = |root: &str| Row {
            repo: Repo {
                root: root.into(),
                ..repo()
            },
            verdict: Verdict::WrongProfile {
                root: root.into(),
                expected: "acme".into(),
                reason: Reason::Remote("github.com/acme/*".into()),
                actual: Some("personal".into()),
                identity: Identity::default(),
            },
            fixed: None,
        };
        let mut app = app(profiles(), Some(repo()));
        let mut view = ScanView::new(
            "/home/ada/work".into(),
            vec![
                wrong("/home/ada/work/acme/api"),
                wrong("/home/ada/work/acme/web"),
                wrong("/home/ada/work/acme/infra"),
            ],
        );
        view.marked = vec![true, false, true];
        view.wrong_only = true;
        app.mode = Mode::Scan(Box::new(view.clone()));
        insta::assert_snapshot!("scan_marks", draw(&app));
        view.confirming = true;
        app.mode = Mode::Scan(Box::new(view));
        insta::assert_snapshot!("scan_fix_popup", draw(&app));
    }

    #[test]
    fn health_badges_and_the_doctor_view() {
        use crate::app::DoctorView;
        use crate::doctor::Finding;
        let mut app = app(profiles(), Some(repo()));
        let finding = |level, message: &str| Finding {
            level,
            message: message.into(),
        };
        app.health = vec![
            vec![finding(Level::Ok, "nothing to check beyond name and email")],
            vec![
                finding(Level::Fail, "gpg has no secret key ABCD1234"),
                finding(Level::Ok, "ssh key /home/ada/.ssh/id_acme exists"),
            ],
        ];
        app.mode = Mode::Doctor(Box::new(DoctorView {
            name: "acme".into(),
            email: "ada@acme.example".into(),
            findings: app.health[1].clone(),
            checking: true,
        }));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn setup_section_and_its_popup() {
        let mut app = app(profiles(), Some(repo()));
        app.repo.as_mut().unwrap().authors.clear();
        app.setup = Some(Setup {
            hooks: HookStatus::Missing,
            includes: Ok(Includes::Overridden),
        });
        insta::assert_snapshot!("setup_section", draw(&app));
        app.mode = Mode::ConfirmSetup(SetupAction::Includes);
        insta::assert_snapshot!("setup_popup", draw(&app));
    }

    #[test]
    fn form_flags_bad_fields_and_shows_a_picker() {
        let mut app = app(profiles(), Some(repo()));
        let mut form = Form::edit(1, &profiles()[1]);
        form.values[crate::form::SSH_KEY] = "/nonexistent/id_acme".into();
        form.field = crate::form::SSH_KEY;
        form.cursor = form.values[form.field].chars().count();
        app.mode = Mode::Edit(Box::new(form.clone()));
        insta::assert_snapshot!("form_checks", draw(&app));
        form.field = crate::form::SIGNING_FORMAT;
        form.open_picker(form.candidates(std::path::Path::new("/nonexistent")));
        app.mode = Mode::Edit(Box::new(form));
        insta::assert_snapshot!("form_picker", draw(&app));
    }

    #[test]
    fn form_previews_what_the_rules_claim() {
        let mut app = app(profiles(), Some(repo()));
        let mut infra = Profile {
            name: "infra".into(),
            user_name: "Ada".into(),
            email: "ada@acme.example".into(),
            ..Default::default()
        };
        infra.remotes = vec!["github.com/acme/infra".into()];
        app.config.profiles.push(infra);
        let repo_at = |name: &str| Repo {
            root: format!("/home/ada/work/acme/{name}").into(),
            remote: Some(format!("git@github.com:acme/{name}.git")),
            ..repo()
        };
        app.known_repos = Some(vec![repo_at("api"), repo_at("web"), repo_at("infra")]);
        let mut form = Form::edit(1, &app.config.profiles[1]);
        form.field = crate::form::REMOTES;
        form.cursor = form.values[form.field].chars().count();
        app.mode = Mode::Edit(Box::new(form));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn why_lists_the_matching_rules_and_why_each_lost() {
        let mut app = app(profiles(), Some(repo()));
        app.config.profiles[0].paths = vec!["/home/ada/work/**".into()];
        app.config.profiles.push(Profile {
            name: "agency".into(),
            user_name: "Ada".into(),
            email: "ada@agency.example".into(),
            remotes: vec!["github.com/**".into()],
            ..Default::default()
        });
        app.mode = Mode::Why;
        insta::assert_snapshot!("why_claimed", draw(&app));
        app.config.profiles.iter_mut().for_each(|p| {
            p.remotes.clear();
            p.paths.clear();
        });
        insta::assert_snapshot!("why_unclaimed", draw(&app));
    }

    #[test]
    fn clone_dialog_follows_the_rules() {
        let mut app = app(profiles(), Some(repo()));
        let mut view = crate::app::CloneView::new("/home/ada/work".into());
        view.url = "git@github.com:acme/web.git".into();
        view.dir = "~/work/web".into();
        app.mode = Mode::Clone(Box::new(view));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn report_view_bars_and_slips() {
        use crate::report::{Commit, RepoReport, periods};
        let commit = |email: &str| Commit {
            date: "2026-11-02".into(),
            hash: "abc1234".into(),
            email: email.into(),
            subject: "work".into(),
        };
        let report = |path: &str, profile: &str, emails: &[&str]| RepoReport {
            root: format!("/home/ada/work/{path}").into(),
            profile: Some(profile.into()),
            commits: emails.iter().map(|e| commit(e)).collect(),
        };
        let mut app = app(profiles(), Some(repo()));
        app.mode = Mode::Report(Box::new(crate::app::ReportView {
            root: "/home/ada/work".into(),
            periods: periods((2026, 11, 7)),
            period: 2,
            reports: vec![
                report("acme/api", "acme", &["ada@acme.example"; 12]),
                report(
                    "acme/web",
                    "acme",
                    &["ada@acme.example", "ada@example.com", "ada@example.com"],
                ),
                report("toy", "personal", &["ada@example.com"; 4]),
            ],
            scroll: 0,
        }));
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn the_footer_offers_the_way_back_to_the_list() {
        let mut app = app(profiles(), Some(repo()));
        app.back_to_scan = Some(Box::new(ScanView::new("/home/ada/work".into(), Vec::new())));
        let screen = draw(&app);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("Esc repos"), "{footer}");
        assert!(!footer.contains("quit"), "{footer}");
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
        app.mode = Mode::Scan(Box::new(ScanView::new(
            "/home/ada/work".into(),
            vec![
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
        )));
        if let Mode::Scan(view) = &mut app.mode {
            view.selected = 1;
        }
        insta::assert_snapshot!(draw(&app));
    }

    #[test]
    fn wide_characters_keep_the_cursor_in_view() {
        let value = "日本語".repeat(8);
        let (shown, column) = visible(&value, Some(24), 40);
        assert_eq!(column, 39 - 1, "two columns per character, one cell spare");
        assert!(shown.chars().map(|c| c.width().unwrap()).sum::<usize>() <= 40);
        let (shown, column) = visible("abc", Some(1), 40);
        assert_eq!((shown.as_str(), column), ("abc", 1));
    }

    fn svg_colour(colour: Color, fallback: &'static str) -> &'static str {
        match colour {
            Color::Reset => fallback,
            Color::Black => "#1e2127",
            Color::Red | Color::LightRed => "#e06c75",
            Color::Green | Color::LightGreen => "#98c379",
            Color::Yellow | Color::LightYellow => "#e5c07b",
            Color::Blue | Color::LightBlue => "#61afef",
            Color::Magenta | Color::LightMagenta => "#c678dd",
            Color::Cyan | Color::LightCyan => "#56b6c2",
            Color::Gray => "#abb2bf",
            Color::DarkGray => "#5c6370",
            Color::White => "#ffffff",
            _ => fallback,
        }
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// The strokes of a box-drawing character as line segments in a unit
    /// cell, so borders are continuous lines rather than font glyphs that
    /// leave gaps between rows.
    fn box_segments(symbol: &str) -> Option<&'static [(f64, f64, f64, f64)]> {
        const H: (f64, f64, f64, f64) = (0.0, 0.5, 1.0, 0.5);
        const V: (f64, f64, f64, f64) = (0.5, 0.0, 0.5, 1.0);
        Some(match symbol {
            "─" => &[H],
            "│" => &[V],
            "┌" => &[(0.5, 0.5, 1.0, 0.5), (0.5, 0.5, 0.5, 1.0)],
            "┐" => &[(0.0, 0.5, 0.5, 0.5), (0.5, 0.5, 0.5, 1.0)],
            "└" => &[(0.5, 0.0, 0.5, 0.5), (0.5, 0.5, 1.0, 0.5)],
            "┘" => &[(0.5, 0.0, 0.5, 0.5), (0.0, 0.5, 0.5, 0.5)],
            "├" => &[V, (0.5, 0.5, 1.0, 0.5)],
            "┤" => &[V, (0.0, 0.5, 0.5, 0.5)],
            _ => return None,
        })
    }

    fn to_svg(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let (cw, ch, pad, bar) = (8.4_f64, 18.0_f64, 16.0_f64, 28.0_f64);
        let (fg0, bg0) = ("#abb2bf", "#282c34");
        let total_w = f64::from(width) * cw + 2.0 * pad;
        let total_h = f64::from(height) * ch + 2.0 * pad + bar;
        let mut back = String::new();
        let mut lines = String::new();
        let mut text = String::new();
        for y in 0..height {
            let mut run: Option<(u16, &str)> = None;
            for x in 0..=width {
                let top = pad + bar + f64::from(y) * ch;
                let cell_bg = (x < width).then(|| {
                    let cell = &buffer[(x, y)];
                    if cell.modifier.contains(Modifier::REVERSED) {
                        svg_colour(cell.fg, fg0)
                    } else {
                        svg_colour(cell.bg, bg0)
                    }
                });
                if run.is_some_and(|(_, colour)| Some(colour) != cell_bg) {
                    let (start, colour) = run.take().unwrap();
                    if colour != bg0 {
                        back.push_str(&format!(
                            "<rect x=\"{:.1}\" y=\"{top:.1}\" width=\"{:.1}\" height=\"{ch}\" fill=\"{colour}\"/>\n",
                            pad + f64::from(start) * cw,
                            f64::from(x - start) * cw
                        ));
                    }
                }
                if run.is_none() {
                    run = cell_bg.map(|colour| (x, colour));
                }
            }
            for x in 0..width {
                let cell = &buffer[(x, y)];
                let fg = if cell.modifier.contains(Modifier::REVERSED) {
                    svg_colour(cell.bg, bg0)
                } else {
                    svg_colour(cell.fg, fg0)
                };
                let (left, top) = (pad + f64::from(x) * cw, pad + bar + f64::from(y) * ch);
                let symbol = cell.symbol();
                let opacity = if cell.modifier.contains(Modifier::DIM) {
                    " opacity=\"0.6\""
                } else {
                    ""
                };
                if let Some(segments) = box_segments(symbol) {
                    for (x1, y1, x2, y2) in segments {
                        lines.push_str(&format!(
                            "<line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" stroke=\"{fg}\"{opacity}/>\n",
                            left + x1 * cw,
                            top + y1 * ch,
                            left + x2 * cw,
                            top + y2 * ch
                        ));
                    }
                } else if !symbol.trim().is_empty() {
                    let bold = if cell.modifier.contains(Modifier::BOLD) {
                        " font-weight=\"bold\""
                    } else {
                        ""
                    };
                    text.push_str(&format!(
                        "<text x=\"{left:.1}\" y=\"{:.1}\" fill=\"{fg}\"{bold}{opacity}>{}</text>\n",
                        top + ch * 0.74,
                        escape(symbol)
                    ));
                }
            }
        }
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total_w}\" height=\"{total_h}\" \
             viewBox=\"0 0 {total_w} {total_h}\">\n\
             <rect width=\"100%\" height=\"100%\" rx=\"10\" fill=\"{bg0}\"/>\n\
             <circle cx=\"20\" cy=\"16\" r=\"6\" fill=\"#ff5f56\"/>\
             <circle cx=\"40\" cy=\"16\" r=\"6\" fill=\"#ffbd2e\"/>\
             <circle cx=\"60\" cy=\"16\" r=\"6\" fill=\"#27c93f\"/>\n\
             {back}<g stroke-width=\"1.3\">\n{lines}</g>\n\
             <g font-family=\"Menlo, 'SF Mono', Consolas, 'DejaVu Sans Mono', monospace\" \
             font-size=\"14\">\n{text}</g>\n</svg>\n"
        )
    }

    #[test]
    #[ignore = "writes the README screenshot; run with --ignored"]
    fn readme_screenshot() {
        let mut profiles = profiles();
        profiles.push(Profile {
            name: "oss".into(),
            user_name: "Ada Lovelace".into(),
            email: "ada@lovelace.dev".into(),
            signing_key: Some("~/.ssh/id_ed25519.pub".into()),
            signing_format: Some(crate::profile::SigningFormat::Ssh),
            sign_commits: Some(true),
            ..Default::default()
        });
        let mut app = app(profiles, Some(repo()));
        app.selected = 1;
        let path = std::env::var("GITENV_SCREENSHOT").unwrap_or_else(|_| "docs/tui.svg".into());
        std::fs::write(&path, to_svg(&app, 100, 24)).unwrap();
    }

    #[test]
    fn long_form_errors_wrap() {
        let mut app = app(profiles(), Some(repo()));
        let mut form = Form::edit(1, &profiles()[1]);
        form.error = Some(
            "The profiles file changed on disk since gitenv read it, so nothing was saved. \
             Press r to reload it (Esc first to leave the form)"
                .into(),
        );
        app.mode = Mode::Edit(Box::new(form));
        let screen = draw(&app);
        assert!(screen.contains("Press r to reload it"), "{screen}");
    }
}
