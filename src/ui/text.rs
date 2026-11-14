//! Text helpers: display-width-aware wrapping and the Composer's editor.

use unicode_width::UnicodeWidthChar;

/// Display width of a char (control chars count 0, wide CJK 2).
pub fn char_width(c: char) -> usize {
    if c == '\t' {
        return 4;
    }
    c.width().unwrap_or(0)
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Truncate to `max` display columns, adding `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Hard-wrap text to `width` columns (breaking anywhere; CJK text has no
/// spaces). Newlines start new lines. Always returns at least one line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut cur = String::new();
        let mut w = 0;
        for c in line.chars() {
            let c = if c == '\t' { ' ' } else { c };
            let cw = char_width(c);
            if w + cw > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
        out.push(cur);
    }
    out
}

/// Prefix the first wrapped line with `first` and the rest with `rest`
/// (same width), wrapping `text` into the remaining width.
pub fn wrap_prefixed(text: &str, width: usize, first: &str, rest: &str) -> Vec<String> {
    let inner = width.saturating_sub(str_width(first)).max(1);
    wrap(text, inner)
        .into_iter()
        .enumerate()
        .map(|(i, l)| format!("{}{l}", if i == 0 { first } else { rest }))
        .collect()
}

/// A small multi-line editor for the Composer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextInput {
    chars: Vec<char>,
    /// Cursor position as a char index (0..=len).
    cursor: usize,
}

impl TextInput {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_blank(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        for c in s.chars() {
            self.insert(c);
        }
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    fn line_start(&self) -> usize {
        self.chars[..self.cursor]
            .iter()
            .rposition(|&c| c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.chars[self.cursor..]
            .iter()
            .position(|&c| c == '\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.chars.len())
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Move to the same column on the previous (`-1`) or next (`1`) logical line.
    pub fn vertical(&mut self, dir: i32) {
        let start = self.line_start();
        let col = self.cursor - start;
        if dir < 0 {
            if start == 0 {
                self.cursor = 0;
                return;
            }
            let prev_end = start - 1;
            let prev_start = self.chars[..prev_end]
                .iter()
                .rposition(|&c| c == '\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            self.cursor = (prev_start + col).min(prev_end);
        } else {
            let end = self.line_end();
            if end == self.chars.len() {
                self.cursor = end;
                return;
            }
            let next_start = end + 1;
            let next_end = self.chars[next_start..]
                .iter()
                .position(|&c| c == '\n')
                .map(|i| next_start + i)
                .unwrap_or(self.chars.len());
            self.cursor = (next_start + col).min(next_end);
        }
    }

    /// Delete from the line start to the cursor (Ctrl+U).
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Delete the word before the cursor (Ctrl+W).
    pub fn delete_word(&mut self) {
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.chars.drain(i..self.cursor);
        self.cursor = i;
    }

    /// Wrapped display lines and the cursor's (row, col) within them.
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut lines = vec![String::new()];
        let mut w = 0;
        let mut cursor = (0, 0);
        for (i, &c) in self.chars.iter().enumerate() {
            if i == self.cursor {
                cursor = (lines.len() - 1, w);
            }
            if c == '\n' {
                lines.push(String::new());
                w = 0;
                continue;
            }
            let cw = char_width(c);
            if w + cw > width {
                lines.push(String::new());
                w = 0;
                if i == self.cursor {
                    cursor = (lines.len() - 1, 0);
                }
            }
            lines.last_mut().expect("non-empty").push(c);
            w += cw;
        }
        if self.cursor == self.chars.len() {
            if w >= width {
                lines.push(String::new());
                w = 0;
            }
            cursor = (lines.len() - 1, w);
        }
        (lines, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_truncate() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("日本"), 4);
        assert_eq!(truncate("日本語テキスト", 7), "日本語…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
    }

    #[test]
    fn wrap_handles_cjk_and_newlines() {
        assert_eq!(wrap("日本語です", 4), vec!["日本", "語で", "す"]);
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(wrap("", 5), vec![""]);
        assert_eq!(
            wrap_prefixed("abcdef", 5, "> ", "  "),
            vec!["> abc", "  def"]
        );
    }

    #[test]
    fn editing() {
        let mut t = TextInput::default();
        t.insert_str("hello");
        t.left();
        t.left();
        t.insert('X');
        assert_eq!(t.text(), "helXlo");
        t.backspace();
        assert_eq!(t.text(), "hello");
        t.delete();
        assert_eq!(t.text(), "helo");
        t.insert('l');
        assert_eq!(t.text(), "hello");
        t.end();
        t.insert_str("\r\nworld");
        assert_eq!(t.text(), "hello\nworld");
        t.vertical(-1);
        t.home();
        t.insert('>');
        assert_eq!(t.text(), ">hello\nworld");
        t.vertical(1);
        t.end();
        t.delete_word();
        assert_eq!(t.text(), ">hello\n");
        t.vertical(-1);
        t.end();
        t.kill_to_start();
        assert_eq!(t.text(), "\n");
        assert!(t.is_blank());
    }

    #[test]
    fn layout_cursor() {
        let mut t = TextInput::default();
        t.insert_str("abcdef");
        let (lines, cur) = t.layout(4);
        assert_eq!(lines, vec!["abcd", "ef"]);
        assert_eq!(cur, (1, 2));
        t.set("abcd");
        let (lines, cur) = t.layout(4);
        assert_eq!(lines, vec!["abcd", ""]);
        assert_eq!(cur, (1, 0));
        t.set("日本\n語");
        t.home();
        let (lines, cur) = t.layout(10);
        assert_eq!(lines, vec!["日本", "語"]);
        assert_eq!(cur, (1, 0));
        t.vertical(-1);
        t.end();
        assert_eq!(t.layout(10).1, (0, 4));
    }
}
