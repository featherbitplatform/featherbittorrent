//! One line of text with a cursor: the field every dialog is built from, and its masked twin for
//! the secrets among them.

use zeroize::Zeroizing;

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

/// Text that is a secret: a password or a key passphrase, held only inside its [`Zeroizing`] string so
/// that nothing else in the program has a copy of it, and drawn as stars.
///
/// It answers the same keys [`Input`] does because it is the same field with two differences: what is
/// typed goes straight into the zeroizing string rather than into a `String` somebody could clone, and
/// what is drawn is one `*` per character rather than the characters themselves. The cursor is kept
/// separately for the same reason - a masked field has no text to measure it against on screen.
///
/// The string arrives with room for `SECRET_CAPACITY` characters so that typing never reallocates:
/// when a `String` grows, the bytes it has already moved are left in memory that is freed without being
/// overwritten, which is the one thing a zeroizing string exists to prevent.
#[derive(Clone, PartialEq, Eq)]
pub struct Masked {
    /// What has been typed, and the only place it lives.
    text: Zeroizing<String>,
    /// The character the next one is typed before, counted from 0.
    cursor: usize,
}

/// How much a secret field is ready for from the first keystroke: comfortably more than a password or a
/// key passphrase anybody types by hand or pastes, so nothing has to move while it is being typed. Past
/// this length the string grows as any other would, and the bytes it leaves behind are freed rather than
/// overwritten - which is why the limit is worth knowing about even though nothing refuses a longer one.
const SECRET_CAPACITY: usize = 256;

impl Default for Masked {
    fn default() -> Masked {
        Masked {
            text: Zeroizing::new(String::with_capacity(SECRET_CAPACITY)),
            cursor: 0,
        }
    }
}

impl std::fmt::Debug for Masked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A field that gets logged on the way to a screen must not repeat what was typed into it.
        f.write_str("Masked(<redacted>)")
    }
}

impl Masked {
    /// An empty masked field.
    pub fn new() -> Masked {
        Masked::default()
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

    /// Empties the field, overwriting what was in it.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Whether nothing has been typed. An empty password and no password are the same thing to the
    /// backend, so this is the question the connect screen asks before it sends one.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// How many characters have been typed, which is how many stars get drawn.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// The two pieces a masked field is drawn from: how many characters sit before the cursor, and how
    /// many from the cursor on. Only counts cross into the renderer, so no character of the secret is
    /// ever copied out to be drawn.
    pub fn split_at_cursor(&self) -> (usize, usize) {
        let end = self.text.chars().count();
        let before = self.cursor.min(end);
        (before, end - before)
    }

    /// The text itself, for the one thing that has to have it: handing the password to a backend, or
    /// asking a key file whether this passphrase opens it. A borrow, not a copy - and nothing here
    /// keeps it after the call returns.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Moves the typed text out of the field without copying it, leaving an empty field behind. This is
    /// how a secret gets to [`SecretInput`](crate::modal::ConnectRequest)'s zeroizing string.
    pub(crate) fn take(&mut self) -> Zeroizing<String> {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Keeps the cursor inside the text, which a text replaced wholesale can move out of.
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
    fn a_masked_field_types_deletes_and_counts_characters() {
        let mut secret = Masked::new();
        for c in "hunter2".chars() {
            secret.insert(c);
        }
        assert_eq!(secret.text.as_str(), "hunter2", "typing did not land");
        assert_eq!(secret.len(), 7, "the count is not the character count");
        assert!(!secret.is_empty());

        // The renderer gets counts, never the text: what it draws is seven stars wherever the cursor
        // sits, and nothing it is handed can be printed back as the password.
        secret.move_left();
        let (before, at_cursor) = secret.split_at_cursor();
        assert_eq!((before, at_cursor), (6, 1));
        secret.home();
        secret.backspace();
        assert_eq!(
            secret.text.as_str(),
            "hunter2",
            "deleting at the start took something"
        );

        // Emptying it and taking what was typed both leave an empty field.
        secret.clear();
        assert!(secret.is_empty());
        let mut other = Masked::new();
        other.insert('x');
        assert_eq!(other.take().as_str(), "x");
        assert!(other.is_empty(), "taking left the text behind");
    }

    #[test]
    fn a_masked_field_does_not_print_what_was_typed() {
        let mut secret = Masked::new();
        for c in "hunter2".chars() {
            secret.insert(c);
        }
        assert_eq!(format!("{secret:?}"), "Masked(<redacted>)");
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

    #[test]
    fn a_secret_arrives_with_room_for_a_password() {
        // Typing into a secret is not supposed to move it: a `String` that grows leaves the bytes it has
        // already moved in memory that is freed without being overwritten.
        let mut secret = Masked::new();
        let room = secret.text.capacity();
        assert!(
            room >= SECRET_CAPACITY,
            "a fresh secret field should be ready for a password, not for one character"
        );

        for c in "correct horse battery staple hunter2".chars() {
            secret.insert(c);
        }
        assert_eq!(
            secret.text.capacity(),
            room,
            "typing a normal-length password reallocated the field, which is how bytes get left behind"
        );

        // Emptying it keeps the room for the next attempt.
        secret.clear();
        assert_eq!(secret.text.capacity(), room);
    }
}
