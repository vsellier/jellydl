//! One-line text input with a cursor.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone)]
pub struct TextInput {
    text: String,
    /// Cursor position, in characters (not bytes).
    cursor: usize,
}

impl TextInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte index of the character at `pos`.
    fn byte(&self, pos: usize) -> usize {
        self.text
            .char_indices()
            .nth(pos)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Handle an editing key. Returns `None` when the key is not an editing key,
    /// `Some(true)` when the text changed, `Some(false)` when only the cursor moved.
    pub fn on_key(&mut self, k: KeyEvent) -> Option<bool> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.len(),
            // Other Ctrl keys are not text.
            KeyCode::Char(_) if ctrl => return None,
            KeyCode::Char(c) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
                return Some(true);
            }
            KeyCode::Backspace => {
                if self.cursor == 0 {
                    return Some(false);
                }
                self.cursor -= 1;
                let at = self.byte(self.cursor);
                self.text.remove(at);
                return Some(true);
            }
            KeyCode::Delete => {
                if self.cursor == self.len() {
                    return Some(false);
                }
                let at = self.byte(self.cursor);
                self.text.remove(at);
                return Some(true);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => return None,
        }
        Some(false)
    }

    /// The part of the text that fits in `width` columns, and the cursor
    /// column in it. The text scrolls so the cursor stays visible.
    pub fn view(&self, width: usize) -> (String, usize) {
        let width = width.max(1);
        // Keep one column for the cursor after the last character.
        let start = (self.cursor + 1).saturating_sub(width);
        let visible = self.text.chars().skip(start).take(width).collect();
        (visible, self.cursor - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(s: &str) -> TextInput {
        let mut t = TextInput::default();
        for c in s.chars() {
            t.on_key(key(KeyCode::Char(c)));
        }
        t
    }

    #[test]
    fn edits_at_the_cursor() {
        let mut t = typed("amlie");
        assert_eq!(t.cursor(), 5);
        t.on_key(key(KeyCode::Home));
        t.on_key(key(KeyCode::Right));
        t.on_key(key(KeyCode::Right));
        assert_eq!(t.on_key(key(KeyCode::Char('é'))), Some(true));
        assert_eq!(t.text(), "amélie");
        t.on_key(key(KeyCode::Left));
        assert_eq!(t.on_key(key(KeyCode::Delete)), Some(true));
        assert_eq!(t.text(), "amlie");
        assert_eq!(t.on_key(key(KeyCode::Backspace)), Some(true));
        assert_eq!((t.text(), t.cursor()), ("alie", 1));
        t.on_key(key(KeyCode::End));
        assert_eq!(t.on_key(key(KeyCode::Delete)), Some(false), "nothing after");
        assert_eq!(t.cursor(), 4);
    }

    #[test]
    fn ctrl_a_and_ctrl_e() {
        let mut t = typed("toto");
        assert_eq!(t.on_key(ctrl('a')), Some(false));
        assert_eq!(t.cursor(), 0);
        assert_eq!(t.on_key(key(KeyCode::Backspace)), Some(false));
        assert_eq!(t.on_key(key(KeyCode::Left)), Some(false));
        assert_eq!(t.cursor(), 0);
        t.on_key(ctrl('e'));
        assert_eq!(t.cursor(), 4);
        t.on_key(key(KeyCode::Right));
        assert_eq!(t.cursor(), 4, "stays at the end");
        assert_eq!(t.on_key(ctrl('x')), None, "not inserted");
        assert_eq!(t.text(), "toto");
        assert_eq!(t.on_key(key(KeyCode::Enter)), None);
    }

    #[test]
    fn scrolls_to_keep_the_cursor_visible() {
        let mut t = typed("abcdefghij");
        assert_eq!(t.view(5), ("ghij".into(), 4));
        t.on_key(key(KeyCode::Home));
        assert_eq!(t.view(5), ("abcde".into(), 0));
        assert_eq!(typed("abc").view(10), ("abc".into(), 3));
    }
}
