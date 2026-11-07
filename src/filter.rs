use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A `/` filter over a list: the query typed so far, and whether keys still
/// go into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub query: String,
    pub typing: bool,
}

impl Filter {
    pub fn is_active(&self) -> bool {
        self.typing || !self.query.is_empty()
    }

    /// Whether the query's characters appear in `text` in order, ignoring
    /// case, each one either right after the previous one or at the start of
    /// a word, so `acm` finds `acme` and `aapi` finds `acme/api` without
    /// `acm` also finding the scattered letters of `ada@example.com`. Spaces
    /// in the query only separate words.
    pub fn matches(&self, text: &str) -> bool {
        let query: Vec<char> = self
            .query
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect();
        let chars: Vec<char> = text.chars().collect();
        let starts: Vec<bool> = (0..chars.len())
            .map(|i| {
                i == 0
                    || !chars[i - 1].is_alphanumeric()
                    || (chars[i - 1].is_lowercase() && chars[i].is_uppercase())
            })
            .collect();
        let lower: Vec<char> = chars
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect();
        let mut failed = std::collections::HashSet::new();
        fuzzy(&query, 0, &lower, &starts, 0, false, &mut failed)
    }

    pub fn start(&mut self) {
        self.typing = true;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Takes a key while typing and says whether it was used. Arrows and
    /// Ctrl-C are left to the list, so it can still be moved and quit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if !self.typing {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => self.typing = false,
            KeyCode::Esc => self.clear(),
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char('u') if ctrl => self.query.clear(),
            KeyCode::Char('w') if ctrl => {
                let kept = self.query.trim_end().rfind(' ').map_or(0, |i| i + 1);
                self.query.truncate(kept);
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.query.push(c)
            }
            _ => return false,
        }
        true
    }
}

/// Whether `query[qi..]` matches from `lower[from..]`, where `adjacent`
/// says the character at `from` directly follows the previous match.
/// `failed` remembers dead ends so long texts stay fast.
fn fuzzy(
    query: &[char],
    qi: usize,
    lower: &[char],
    starts: &[bool],
    from: usize,
    adjacent: bool,
    failed: &mut std::collections::HashSet<(usize, usize, bool)>,
) -> bool {
    if qi == query.len() {
        return true;
    }
    if failed.contains(&(qi, from, adjacent)) {
        return false;
    }
    for j in from..lower.len() {
        if lower[j] == query[qi]
            && ((adjacent && j == from) || starts[j])
            && fuzzy(query, qi + 1, lower, starts, j + 1, true, failed)
        {
            return true;
        }
    }
    failed.insert((qi, from, adjacent));
    false
}

/// The entry `delta` steps from `selected` among the `visible` ones,
/// wrapping around; the first visible one when `selected` is hidden.
pub fn step(visible: &[usize], selected: usize, delta: isize) -> usize {
    match visible.iter().position(|&i| i == selected) {
        Some(at) => {
            let next = (at as isize + delta).rem_euclid(visible.len() as isize);
            visible[next as usize]
        }
        None => visible.first().copied().unwrap_or(selected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(query: &str) -> Filter {
        Filter {
            query: query.into(),
            typing: false,
        }
    }

    #[test]
    fn matches_characters_in_order_ignoring_case() {
        assert!(filter("").matches("anything"));
        assert!(filter("acm").matches("acme"));
        assert!(filter("AAPI").matches("acme/api"));
        assert!(!filter("mca").matches("acme"));
        assert!(!filter("acmee").matches("acme"));
        assert!(!filter("acm").matches("personal Ada ada@example.com"));
        assert!(filter("ops").matches("acme-ops"));
        assert!(filter("ao").matches("acme-ops"));
        assert!(filter("gh").matches("gitHub"));
        assert!(filter("work api").matches("~/work/acme/api"));
    }

    #[test]
    fn typing_edits_the_query_and_enter_keeps_it() {
        let mut f = Filter::default();
        assert!(!f.handle_key(KeyEvent::from(KeyCode::Char('a'))));
        f.start();
        for c in "acme api".chars() {
            assert!(f.handle_key(KeyEvent::from(KeyCode::Char(c))));
        }
        f.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(f.query, "acme ");
        f.handle_key(KeyEvent::from(KeyCode::Backspace));
        assert!(!f.handle_key(KeyEvent::from(KeyCode::Down)));
        assert!(!f.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        f.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(f, filter("acme"));
        f.start();
        f.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(f, Filter::default());
    }

    #[test]
    fn step_wraps_within_the_visible_entries() {
        assert_eq!(step(&[1, 3, 4], 3, 1), 4);
        assert_eq!(step(&[1, 3, 4], 4, 1), 1);
        assert_eq!(step(&[1, 3, 4], 1, -1), 4);
        assert_eq!(step(&[1, 3, 4], 2, 1), 1);
        assert_eq!(step(&[], 2, 1), 2);
    }
}
