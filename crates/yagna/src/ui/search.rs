//! The search box: the query it holds.
//!
//! The query itself lives here; the ranking lives in the core's fuzzy
//! index, and the page the ranking produces is fetched by the list
//! pane. The search is not run on every letter: the reader asks for it
//! (Enter, or the search button), so typing stays quick and the
//! results arrive when they arrive.
//!
//! Items: `SearchState` — the query.
//! Core functions: `typed_text` — the character a keystroke typed, if
//! it typed one; `SearchState::set` — the query.
//! Depends on: gpui (the key events).
//! Called by: crate::ui (the search field and the list's key map).

use gpui::KeyDownEvent;

/// The search box's state: the query it holds.
#[derive(Debug, Default)]
pub(crate) struct SearchState {
    query: String,
}

impl SearchState {
    /// The query as typed.
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Whether a query is typed at all.
    pub(crate) fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Replaces the query.
    pub(crate) fn set(&mut self, query: &str) {
        self.query = query.to_string();
    }
}

/// The character a keystroke typed, when it typed one. The key's own
/// character is the source; a keystroke with a control, alt or platform
/// modifier typed nothing.
pub(crate) fn typed_text(event: &KeyDownEvent) -> Option<String> {
    let modifiers = event.keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return None;
    }
    if let Some(text) = event.keystroke.key_char.as_ref() {
        return (!text.is_empty()).then(|| text.clone());
    }
    let key = event.keystroke.key.as_str();
    (key.chars().count() == 1).then(|| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn key_event(source: &str) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(source).expect("test keystroke"),
            is_held: false,
        }
    }

    #[test]
    fn a_letter_typed_is_the_text() {
        assert_eq!(typed_text(&key_event("a")), Some("a".to_string()));
        assert_eq!(typed_text(&key_event("z")), Some("z".to_string()));
    }

    #[test]
    fn a_control_key_typed_nothing() {
        assert_eq!(typed_text(&key_event("enter")), None);
        assert_eq!(typed_text(&key_event("backspace")), None);
        assert_eq!(typed_text(&key_event("escape")), None);
        assert_eq!(typed_text(&key_event("up")), None);
        assert_eq!(typed_text(&key_event("ctrl-a")), None);
        assert_eq!(typed_text(&key_event("alt-a")), None);
    }

    #[test]
    fn the_query_is_what_the_reader_asked_for() {
        let mut state = SearchState::default();
        assert!(!state.is_active());
        state.set("ze");
        assert_eq!(state.query(), "ze");
        assert!(state.is_active());
        state.set("zed");
        assert_eq!(state.query(), "zed");
        state.set("");
        assert_eq!(state.query(), "");
        assert!(!state.is_active());
    }
}
