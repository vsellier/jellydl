//! One-line text input with a cursor and the usual readline keys.
//!
//! Moves: Ctrl+A/E, Ctrl+B/F, Alt+B/F, Ctrl+Left/Right, Home/End.
//! Deletes: Backspace, Ctrl+H, Del, Ctrl+D, Ctrl+W, Alt+Backspace, Alt+D,
//! Ctrl+K, Ctrl+U. Deleted text goes to a kill buffer: Ctrl+Y pastes it.
//! Also: Ctrl+T swaps characters, Alt+U/L/C change the case of a word.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone)]
pub struct TextInput {
    text: String,
    /// Cursor position, in characters (not bytes).
    cursor: usize,
    /// Text removed by the last kill commands, for Ctrl+Y.
    killed: String,
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

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    /// Start of the word before `pos` (skips spaces, then the word).
    fn word_start(&self, pos: usize) -> usize {
        let c = self.chars();
        let mut i = pos.min(c.len());
        while i > 0 && c[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !c[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    /// End of the word after `pos` (skips spaces, then the word).
    fn word_end(&self, pos: usize) -> usize {
        let c = self.chars();
        let mut i = pos;
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        while i < c.len() && !c[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Remove the characters in `from..to`. With `kill`, keep them for Ctrl+Y.
    fn remove(&mut self, from: usize, to: usize, kill: bool) -> Option<bool> {
        if from >= to {
            return Some(false);
        }
        let (a, b) = (self.byte(from), self.byte(to));
        let removed: String = self.text.drain(a..b).collect();
        if kill {
            self.killed = removed;
        }
        self.cursor = from;
        Some(true)
    }

    fn insert_str(&mut self, s: &str) {
        let at = self.byte(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    /// Change the case of the word after the cursor, and move after it.
    fn change_word(&mut self, f: impl Fn(usize, char) -> String) -> Option<bool> {
        let end = self.word_end(self.cursor);
        let chars = self.chars();
        let start = chars[self.cursor..end]
            .iter()
            .position(|c| !c.is_whitespace())
            .map_or(end, |p| self.cursor + p);
        let word: String = chars[start..end]
            .iter()
            .enumerate()
            .map(|(i, c)| f(i, *c))
            .collect();
        let (a, b) = (self.byte(start), self.byte(end));
        self.text.replace_range(a..b, &word);
        self.cursor = start + word.chars().count();
        Some(true)
    }

    /// Handle an editing key. Returns `None` when the key is not an editing key,
    /// `Some(true)` when the text changed, `Some(false)` when only the cursor moved.
    pub fn on_key(&mut self, k: KeyEvent) -> Option<bool> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let len = self.len();
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(self.cursor),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(self.cursor),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_start(self.cursor),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_end(self.cursor),
            KeyCode::Char('h') if ctrl => {
                return self.remove(self.cursor.saturating_sub(1), self.cursor, false);
            }
            KeyCode::Char('d') if ctrl => {
                return self.remove(self.cursor, (self.cursor + 1).min(len), false);
            }
            KeyCode::Char('d') if alt => {
                return self.remove(self.cursor, self.word_end(self.cursor), true);
            }
            KeyCode::Char('w') if ctrl => {
                return self.remove(self.word_start(self.cursor), self.cursor, true);
            }
            KeyCode::Backspace if ctrl || alt => {
                return self.remove(self.word_start(self.cursor), self.cursor, true);
            }
            KeyCode::Char('k') if ctrl => return self.remove(self.cursor, len, true),
            KeyCode::Char('u') if ctrl => return self.remove(0, self.cursor, true),
            KeyCode::Char('y') if ctrl => {
                if self.killed.is_empty() {
                    return Some(false);
                }
                let k = self.killed.clone();
                self.insert_str(&k);
                return Some(true);
            }
            KeyCode::Char('t') if ctrl => {
                // Swap the two characters around the cursor (before it at the end).
                if len < 2 || self.cursor == 0 {
                    return Some(false);
                }
                let at = self.cursor.min(len - 1);
                let mut c = self.chars();
                c.swap(at - 1, at);
                self.text = c.into_iter().collect();
                self.cursor = at + 1;
                return Some(true);
            }
            KeyCode::Char('u') if alt => {
                return self.change_word(|_, c| c.to_uppercase().collect());
            }
            KeyCode::Char('l') if alt => {
                return self.change_word(|_, c| c.to_lowercase().collect());
            }
            KeyCode::Char('c') if alt => {
                return self.change_word(|i, c| {
                    if i == 0 {
                        c.to_uppercase().collect()
                    } else {
                        c.to_lowercase().collect()
                    }
                });
            }
            // Other Ctrl and Alt keys are not text.
            KeyCode::Char(_) if ctrl || alt => return None,
            KeyCode::Char(c) => {
                self.insert_str(c.encode_utf8(&mut [0; 4]));
                return Some(true);
            }
            KeyCode::Backspace => {
                return self.remove(self.cursor.saturating_sub(1), self.cursor, false);
            }
            KeyCode::Delete => return self.remove(self.cursor, (self.cursor + 1).min(len), false),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
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

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    #[test]
    fn moves_by_character_and_word() {
        let mut t = typed("foo bar  baz");
        t.on_key(alt('b'));
        assert_eq!(t.cursor(), 9);
        t.on_key(alt('b'));
        assert_eq!(t.cursor(), 4);
        t.on_key(alt('f'));
        assert_eq!(t.cursor(), 7);
        t.on_key(ctrl('b'));
        assert_eq!(t.cursor(), 6);
        t.on_key(ctrl('f'));
        t.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 4);
        t.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 7);
    }

    #[test]
    fn kills_and_yanks() {
        let mut t = typed("foo bar baz");
        assert_eq!(t.on_key(ctrl('w')), Some(true));
        assert_eq!(t.text(), "foo bar ");
        t.on_key(ctrl('y'));
        assert_eq!(t.text(), "foo bar baz");
        t.on_key(ctrl('a'));
        t.on_key(alt('d'));
        assert_eq!(t.text(), " bar baz");
        t.on_key(ctrl('k'));
        assert_eq!(t.text(), "");
        t.on_key(ctrl('y'));
        assert_eq!(t.text(), " bar baz");
        t.on_key(ctrl('u'));
        assert_eq!((t.text(), t.cursor()), ("", 0));
        t.on_key(ctrl('y'));
        assert_eq!(t.text(), " bar baz");
        t.on_key(ctrl('a'));
        t.on_key(ctrl('d'));
        t.on_key(ctrl('e'));
        t.on_key(ctrl('h'));
        assert_eq!(t.text(), "bar ba");
        assert_eq!(
            t.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            Some(true)
        );
        assert_eq!(t.text(), "bar ");
    }

    #[test]
    fn transposes_and_changes_case() {
        let mut t = typed("ab");
        t.on_key(ctrl('t'));
        assert_eq!(t.text(), "ba");
        t.on_key(ctrl('a'));
        t.on_key(alt('u'));
        assert_eq!((t.text(), t.cursor()), ("BA", 2));
        t.on_key(ctrl('a'));
        t.on_key(alt('c'));
        assert_eq!(t.text(), "Ba");
        t.on_key(ctrl('a'));
        t.on_key(alt('l'));
        assert_eq!(t.text(), "ba");
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
