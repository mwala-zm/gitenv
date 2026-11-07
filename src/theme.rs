use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// How the TUI is coloured. The screen is always drawn with the dark
/// palette; another theme repaints the finished frame, so no drawing code
/// has to know which one is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    Plain,
}

impl Theme {
    /// `NO_COLOR` set to anything wins, as https://no-color.org asks;
    /// otherwise `GITENV_THEME` picks `light`, `none` or the default `dark`.
    pub fn from_env() -> Self {
        Self::pick(
            std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            std::env::var("GITENV_THEME").ok().as_deref(),
        )
    }

    fn pick(no_color: bool, theme: Option<&str>) -> Self {
        if no_color {
            return Self::Plain;
        }
        match theme.map(str::to_ascii_lowercase).as_deref() {
            Some("light") => Self::Light,
            Some("none" | "plain" | "no-color") => Self::Plain,
            _ => Self::Dark,
        }
    }

    /// Repaints a frame drawn with the dark palette. Light swaps in darker
    /// 256-colour shades that read on a pale background and a pale
    /// highlight; plain drops every colour and shows highlights reversed.
    pub fn paint(self, buf: &mut Buffer) {
        for cell in &mut buf.content {
            match self {
                Self::Dark => {}
                Self::Light => {
                    cell.fg = light(cell.fg);
                    if cell.bg == Color::DarkGray {
                        cell.bg = Color::Indexed(254);
                    }
                }
                Self::Plain => {
                    if cell.bg != Color::Reset {
                        cell.modifier |= Modifier::REVERSED;
                    }
                    cell.fg = Color::Reset;
                    cell.bg = Color::Reset;
                }
            }
        }
    }
}

fn light(colour: Color) -> Color {
    match colour {
        Color::Green => Color::Indexed(28),
        Color::Yellow => Color::Indexed(130),
        Color::Red => Color::Indexed(124),
        Color::Cyan => Color::Indexed(25),
        Color::DarkGray => Color::Indexed(244),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn no_color_wins_over_the_theme_variable() {
        assert_eq!(Theme::pick(false, None), Theme::Dark);
        assert_eq!(Theme::pick(false, Some("Light")), Theme::Light);
        assert_eq!(Theme::pick(false, Some("none")), Theme::Plain);
        assert_eq!(Theme::pick(false, Some("sepia")), Theme::Dark);
        assert_eq!(Theme::pick(true, Some("light")), Theme::Plain);
    }

    #[test]
    fn plain_drops_colour_and_reverses_highlights() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        buf[(0, 0)].set_style(Style::new().fg(Color::Yellow));
        buf[(1, 0)].set_style(Style::new().bg(Color::DarkGray));
        Theme::Plain.paint(&mut buf);
        assert_eq!(buf[(0, 0)].fg, Color::Reset);
        assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buf[(1, 0)].bg, Color::Reset);
        assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn light_darkens_text_and_lightens_the_highlight() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        buf[(0, 0)].set_style(Style::new().fg(Color::Yellow).bg(Color::DarkGray));
        Theme::Light.paint(&mut buf);
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(130));
        assert_eq!(buf[(0, 0)].bg, Color::Indexed(254));
    }
}
