//! One line of text with a cursor: the field every dialog is built from.

/// Text being typed, and where the next character goes. The cursor counts characters rather than
/// bytes, so typing into a path with a non-ASCII name cannot cut it in half.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// What has been typed.
    pub text: String,
    /// The character the next one is typed before, counted from 0.
    pub cursor: usize,
}

impl Input {
    /// A field holding `text`, with the cursor after it.
    pub fn new(text: impl Into<String>) -> Input {
        let text = text.into();
        let cursor = text.chars().count();
        Input { text, cursor }
    }

    /// Types `c` at the cursor.
    pub fn insert(&mut self, c: char) {
        self.clamp_cursor();
        let at = byte_at(&self.text, self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Deletes the character before the cursor.
    pub fn backspace(&mut self) {
        self.clamp_cursor();
        if self.cursor == 0 {
            return;
        }
        let at = byte_at(&self.text, self.cursor - 1);
        self.text.remove(at);
        self.cursor -= 1;
    }

    /// Moves the cursor one character back, stopping before the first one.
    pub fn move_left(&mut self) {
        self.clamp_cursor();
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor one character forward, stopping after the last one.
    pub fn move_right(&mut self) {
        self.clamp_cursor();
        let end = self.text.chars().count();
        self.cursor = (self.cursor + 1).min(end);
    }

    /// Puts the cursor before the first character.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Puts the cursor after the last character.
    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// Empties the field.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// The text in the two pieces a field is drawn from: everything before the cursor, and
    /// everything from the cursor on.
    pub fn split_at_cursor(&self) -> (String, String) {
        let at = byte_at(&self.text, self.cursor.min(self.text.chars().count()));
        (self.text[..at].to_string(), self.text[at..].to_string())
    }

    /// Keeps the cursor inside the text, which assigning to [`text`](Self::text) can move it out
    /// of. Every method that uses it calls this first.
    fn clamp_cursor(&mut self) {
        let end = self.text.chars().count();
        self.cursor = self.cursor.min(end);
    }
}

/// The byte offset of the `index`th character, or the end of the string past the last one.
fn byte_at(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(at, _)| at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_types_deletes_and_moves_without_losing_characters() {
        let mut field = Input::new("ab");
        field.insert('c');
        assert_eq!(field.text, "abc", "typing at the end did not append");

        field.home();
        field.insert('_');
        assert_eq!(
            field.text, "_abc",
            "typing at the home position did not insert"
        );

        field.end();
        field.backspace();
        assert_eq!(field.text, "_ab");
        field.move_left();
        field.backspace();
        assert_eq!(field.text, "_b");

        // Deleting at the start and moving right past the end both stop instead of wrapping or
        // running off the text.
        field.home();
        field.backspace();
        assert_eq!(field.text, "_b");
        for _ in 0..5 {
            field.move_right();
        }
        assert_eq!(field.cursor, 2);
    }

    #[test]
    fn a_cursor_counts_characters_and_survives_a_text_assigned_from_outside() {
        let mut field = Input::new("café");
        assert_eq!(field.text.chars().count(), 4);
        field.end();
        assert_eq!(field.cursor, 4, "the cursor counted bytes");

        // Assigning the text directly leaves a cursor that is past the end of it, which the next
        // keystroke has to cope with rather than panic on.
        field.text = "ab".to_string();
        field.insert('c');
        assert_eq!(field.text, "abc");

        field.clear();
        assert_eq!(field.text, "");
        assert_eq!(field.cursor, 0);
    }

    #[test]
    fn splitting_at_the_cursor_keeps_a_character_whole() {
        let mut field = Input::new("café");
        field.home();
        field.move_right();
        let (before, at_cursor) = field.split_at_cursor();
        assert_eq!(before, "c");
        assert_eq!(at_cursor, "afé");

        field.end();
        let (before, at_cursor) = field.split_at_cursor();
        assert_eq!(before, "café");
        assert_eq!(
            at_cursor, "",
            "a cursor after the last character has nothing on it"
        );
    }
}
