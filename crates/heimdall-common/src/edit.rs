//! The search field's text state: a string, a caret, and "everything selected".

#[derive(Default)]
pub struct Edit {
    pub text: String,
    /// Caret position in chars.
    pub caret: usize,
    /// The whole text is selected; typing replaces it.
    pub all: bool,
}

impl Edit {
    fn byte(&self, ch: usize) -> usize {
        self.text.char_indices().nth(ch).map(|(b, _)| b).unwrap_or(self.text.len())
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.caret = self.len();
        self.all = false;
    }

    pub fn select_all(&mut self) {
        self.all = !self.text.is_empty();
        self.caret = self.len();
    }

    pub fn insert(&mut self, s: &str) {
        if self.all {
            self.text.clear();
            self.caret = 0;
            self.all = false;
        }
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte(self.caret);
        self.text.insert_str(at, &clean);
        self.caret += clean.chars().count();
    }

    pub fn backspace(&mut self, word: bool) {
        if self.all {
            self.clear();
            return;
        }
        if self.caret == 0 {
            return;
        }
        let start = if word { self.word_left() } else { self.caret - 1 };
        let (a, b) = (self.byte(start), self.byte(self.caret));
        self.text.replace_range(a..b, "");
        self.caret = start;
    }

    pub fn delete(&mut self, word: bool) {
        if self.all {
            self.clear();
            return;
        }
        if self.caret >= self.len() {
            return;
        }
        let end = if word { self.word_right() } else { self.caret + 1 };
        let (a, b) = (self.byte(self.caret), self.byte(end));
        self.text.replace_range(a..b, "");
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.all = false;
    }

    pub fn left(&mut self, word: bool) {
        if self.all {
            self.all = false;
            self.caret = 0;
            return;
        }
        self.caret = if word { self.word_left() } else { self.caret.saturating_sub(1) };
    }

    pub fn right(&mut self, word: bool) {
        self.all = false;
        self.caret = if word { self.word_right() } else { (self.caret + 1).min(self.len()) };
    }

    pub fn home(&mut self) {
        self.all = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.all = false;
        self.caret = self.len();
    }

    fn word_left(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret;
        while i > 0 && !chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret;
        while i < chars.len() && !chars[i].is_alphanumeric() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_alphanumeric() {
            i += 1;
        }
        i
    }

    /// Caret position as a UTF-16 offset (for DirectWrite hit-testing).
    pub fn caret16(&self) -> u32 {
        self.text.chars().take(self.caret).map(|c| c.len_utf16() as u32).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut e = Edit::default();
        e.insert("hello world");
        e.backspace(true);
        assert_eq!(e.text, "hello ");
        e.left(true);
        e.insert(">");
        assert_eq!(e.text, ">hello ");
        e.select_all();
        e.insert("x");
        assert_eq!(e.text, "x");
    }
}
