//! The text field: every input in the app — the library search, the
//! jump-to-page, the deep-search query, the builder's value, the
//! DISCOVER filters, the follow field, the sidebar's filter box, the
//! note's paragraph, the spec's JSON, the settings' two numeric
//! fields — is this one element. The fields that take a paragraph are
//! the same element with a line break instead of a submission.
//!
//! A field owns its text, its caret and its focus handle, and it knows
//! the four things a keystroke can mean to it: the text changed, the
//! text was submitted (enter), the field was dismissed (escape), or
//! the caret moved. What those mean is the pane's business — the field
//! only reports them. The caret is a character index: the arrows walk
//! it, home and end jump to the line's ends, backspace and delete cut
//! around it, and a click in a paragraph field seeks it — the line
//! rows' bounds are captured at paint, and the column under the click
//! is measured by shaping the line's prefixes, glyph widths and never
//! character counts. Ctrl-V inserts the clipboard at the caret and
//! Ctrl-C copies the field's whole text.
//!
//! One painting rule covers every field: a `surface` ground with a
//! hairline `border` while unfocused, the accent ring added on focus —
//! visible before it is clicked, on every ground the app paints. A
//! click inside a field stops at the field, so the pane's
//! click-outside rule never cancels an edit the reader is still in.
//!
//! Items: `TextField` — the field's focus, its text and its caret;
//! `FieldText` — the text, the caret and the rules they follow, without
//! a focus handle; `FieldKind` — text, digits-only, or paragraph;
//! `FieldAction` — what a keystroke meant.
//! Core functions: `FieldText::on_key` — the keystroke handling;
//! `TextField::render` / `render_paragraph` — the elements;
//! `on_key_clipboard` — the clipboard keys before the text's own.
//! Depends on: gpui (the focus handle, the key events, the elements,
//! the text system for click measuring, the clipboard).
//! Called by: crate::ui (every field).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, Bounds, ClipboardItem, FocusHandle, KeyDownEvent, Pixels, Point,
    SharedString, Window, div, prelude::*, px,
};

use super::rows::{RowLayout, shape_width};
use super::search::typed_text;
use super::theme;

/// What a field accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    /// Any text.
    Text,
    /// Digits only — the jump-to-page field.
    Digits,
    /// A paragraph: the reader's own words, as long as they are. Enter
    /// breaks the line instead of submitting, and nothing here caps
    /// the length.
    Paragraph,
}

/// What a keystroke meant to a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldAction {
    /// Nothing happened.
    None,
    /// The text changed.
    Changed,
    /// Enter was pressed: the text is the submission.
    Submitted(String),
    /// Escape was pressed: the field was dismissed.
    Escaped,
    /// Only the caret moved; the text is as it was.
    Moved,
}

/// How long one half of the caret's blink lasts.
const CARET_BLINK: Duration = Duration::from_millis(530);

/// The text a field holds, its caret, and the rules they follow — the
/// half of a field that needs no window and no focus handle. The caret
/// counts whole characters (not bytes) from the value's start.
#[derive(Debug)]
pub(crate) struct FieldText {
    value: String,
    kind: FieldKind,
    caret: usize,
    /// The column the last vertical move aimed at, kept while the
    /// cursor walks past shorter lines; none when the last move was
    /// not vertical.
    column: Option<usize>,
}

impl FieldText {
    /// Empty text of the given kind.
    pub(crate) fn new(kind: FieldKind) -> Self {
        Self {
            value: String::new(),
            kind,
            caret: 0,
            column: None,
        }
    }

    /// The text.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// Whether the text is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Replaces the text; the caret lands at its end.
    pub(crate) fn set(&mut self, value: &str) {
        self.value = value.to_string();
        self.caret = self.value.chars().count();
        self.column = None;
    }

    /// Empties the text.
    pub(crate) fn clear(&mut self) {
        self.value.clear();
        self.caret = 0;
        self.column = None;
    }

    /// Puts the caret at the given character index, clamped into the
    /// value. The click's seek and nothing else lands here.
    pub(crate) fn seek(&mut self, index: usize) {
        self.caret = index.min(self.value.chars().count());
        self.column = None;
    }

    /// Inserts text at the caret, the caret riding past it.
    fn insert(&mut self, text: &str) {
        let byte = char_to_byte(&self.value, self.caret);
        self.value.insert_str(byte, text);
        self.caret += text.chars().count();
        self.column = None;
    }

    /// Takes the character before the caret away; nothing at the start.
    fn backspace(&mut self) -> FieldAction {
        if self.caret == 0 {
            return FieldAction::None;
        }
        let byte = char_to_byte(&self.value, self.caret - 1);
        self.value
            .replace_range(byte..char_to_byte(&self.value, self.caret), "");
        self.caret -= 1;
        self.column = None;
        FieldAction::Changed
    }

    /// Takes the character at the caret away; nothing at the end.
    fn delete(&mut self) -> FieldAction {
        if self.caret >= self.value.chars().count() {
            return FieldAction::None;
        }
        let byte = char_to_byte(&self.value, self.caret);
        let end = char_to_byte(&self.value, self.caret + 1);
        self.value.replace_range(byte..end, "");
        self.column = None;
        FieldAction::Changed
    }

    /// Walks the caret one character sideways, clamped at both ends.
    fn move_horizontal(&mut self, step: isize) -> FieldAction {
        let count = self.value.chars().count() as isize;
        let next = (self.caret as isize + step).clamp(0, count);
        if next == self.caret as isize {
            return FieldAction::None;
        }
        self.caret = next as usize;
        self.column = None;
        FieldAction::Moved
    }

    /// Walks the caret one line up or down, keeping the aimed column
    /// when the line walked onto is shorter. A paragraph moves only —
    /// a single-line field has nowhere to go.
    fn move_vertical(&mut self, down: bool) -> FieldAction {
        if self.kind != FieldKind::Paragraph {
            return FieldAction::None;
        }
        let lines = line_texts(&self.value);
        let (line, col) = caret_line_col(&self.value, self.caret);
        let target = if down {
            line + 1
        } else {
            line.saturating_sub(1)
        };
        if target == line || target >= lines.len() {
            return FieldAction::None;
        }
        let aimed = self.column.unwrap_or(col);
        let landed = aimed.min(lines[target].chars().count());
        self.caret = index_of_line_col(&lines, target, landed);
        self.column = Some(aimed);
        FieldAction::Moved
    }

    /// The keystroke handling: typed characters go in at the caret,
    /// backspace and delete cut around it, the arrows and home and end
    /// walk it, escape dismisses the field, enter submits it (or breaks
    /// the line in a paragraph). A character the kind refuses is
    /// ignored.
    pub(crate) fn on_key(&mut self, event: &KeyDownEvent) -> FieldAction {
        let key = event.keystroke.key.as_str();
        let modified = event.keystroke.modifiers.modified();
        match key {
            "enter" if !modified => {
                if self.kind == FieldKind::Paragraph {
                    self.insert("\n");
                    return FieldAction::Changed;
                }
                let text = self.value.clone();
                return FieldAction::Submitted(text);
            }
            "escape" if !modified => {
                self.clear();
                return FieldAction::Escaped;
            }
            "backspace" if !modified => return self.backspace(),
            "delete" if !modified => return self.delete(),
            "left" if !modified => return self.move_horizontal(-1),
            "right" if !modified => return self.move_horizontal(1),
            "up" if !modified => return self.move_vertical(false),
            "down" if !modified => return self.move_vertical(true),
            "home" if !modified => {
                let lines = line_texts(&self.value);
                let (line, _) = caret_line_col(&self.value, self.caret);
                let start = index_of_line_col(&lines, line, 0);
                if start == self.caret {
                    return FieldAction::None;
                }
                self.caret = start;
                self.column = None;
                return FieldAction::Moved;
            }
            "end" if !modified => {
                let lines = line_texts(&self.value);
                let (line, _) = caret_line_col(&self.value, self.caret);
                let end = index_of_line_col(&lines, line, lines[line].chars().count());
                if end == self.caret {
                    return FieldAction::None;
                }
                self.caret = end;
                self.column = None;
                return FieldAction::Moved;
            }
            _ => {}
        }
        let Some(text) = typed_text(event) else {
            return FieldAction::None;
        };
        if !self.accepts(&text) {
            return FieldAction::None;
        }
        self.insert(&text);
        FieldAction::Changed
    }

    /// Whether the kind accepts the text.
    fn accepts(&self, text: &str) -> bool {
        match self.kind {
            FieldKind::Text | FieldKind::Paragraph => true,
            FieldKind::Digits => text.chars().all(|character| character.is_ascii_digit()),
        }
    }
}

/// The byte offset of the character index in a text — past the end is
/// the text's length.
fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// The value's lines, without their line breaks. The rendering's rows
/// and the caret's arithmetic share this one split.
fn line_texts(value: &str) -> Vec<&str> {
    value.split('\n').collect()
}

/// Where the caret sits: the line's index and the column within it, in
/// whole characters. A caret on a line break reads as the line's end.
fn caret_line_col(value: &str, caret: usize) -> (usize, usize) {
    let mut seen = 0usize;
    for (index, line) in line_texts(value).into_iter().enumerate() {
        let length = line.chars().count();
        if caret <= seen + length {
            return (index, caret - seen);
        }
        seen += length + 1;
    }
    (0, 0)
}

/// The caret's character index for a line and column, clamped into the
/// line — and past the lines, into the value's end.
fn index_of_line_col(lines: &[&str], line: usize, column: usize) -> usize {
    let mut index = 0usize;
    for (at, text) in lines.iter().enumerate() {
        if at == line {
            return index + column.min(text.chars().count());
        }
        index += text.chars().count() + 1;
    }
    // Past the last line: the breaks counted one too many — the last
    // line has none.
    index.saturating_sub(1)
}

/// A single-line text field: its focus handle, its text, and the line
/// bounds its paragraph click-seek measured.
#[derive(Debug)]
pub(crate) struct TextField {
    /// The field's name — the caret's element id, so two fields do not
    /// share one animation.
    name: String,
    focus: FocusHandle,
    text: FieldText,
    placeholder: SharedString,
    /// The field's place in the tab order: the search comes first, the
    /// filters next, the jump field last.
    tab_index: isize,
    /// The family the click-seek measures with, when the field paints
    /// itself in a face other than the app's own.
    measure_family: Option<SharedString>,
    /// The paragraph's line rows' bounds, captured each paint — the
    /// click's y picks the line, its x the column.
    line_bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

impl TextField {
    /// A field with the given name, focus handle, placeholder and kind.
    pub(crate) fn new(
        name: &str,
        focus: FocusHandle,
        placeholder: impl Into<SharedString>,
        kind: FieldKind,
    ) -> Self {
        Self {
            name: name.to_string(),
            focus,
            text: FieldText::new(kind),
            placeholder: placeholder.into(),
            tab_index: 0,
            measure_family: None,
            line_bounds: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Measures the click's columns in the given family — the spec's
    /// field paints in the mono face, and the measurement must match.
    pub(crate) fn with_measure_family(mut self, family: impl Into<SharedString>) -> Self {
        self.measure_family = Some(family.into());
        self
    }

    /// Places the field in the tab order: the search comes first, the
    /// filters after it, the jump field last.
    pub(crate) fn at_tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    /// The field's focus handle.
    pub(crate) fn focus(&self) -> &FocusHandle {
        &self.focus
    }

    /// Gives the field the focus — an editor that opens with its field
    /// focused can be typed into and left.
    pub(crate) fn take_focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    /// The field's text.
    pub(crate) fn value(&self) -> &str {
        self.text.value()
    }

    /// Whether the field holds nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the field's text.
    pub(crate) fn set(&mut self, value: &str) {
        self.text.set(value);
    }

    /// Empties the field.
    pub(crate) fn clear(&mut self) {
        self.text.clear();
    }

    /// Puts the caret where a click (or anything else) says, clamped.
    pub(crate) fn move_caret_to(&mut self, index: usize) {
        self.text.seek(index);
    }

    /// The clipboard keys before the text's own: Ctrl-V inserts the
    /// clipboard's text at the caret, Ctrl-C copies the field's whole
    /// text. Everything else is the plain keystroke handling.
    pub(crate) fn on_key_clipboard(&mut self, event: &KeyDownEvent, cx: &App) -> FieldAction {
        let control = event.keystroke.modifiers.control;
        let key = event.keystroke.key.as_str();
        if control && key == "v" {
            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                return FieldAction::None;
            };
            self.text
                .insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
            return FieldAction::Changed;
        }
        if control && key == "c" {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text.value.clone()));
            return FieldAction::None;
        }
        self.on_key(event)
    }

    /// The paragraph field's element: the text, one line per row, the
    /// caret on its own line between the text around it. A paragraph
    /// may be long, so the box scrolls; nothing here caps how much the
    /// reader may write. A click seeks the caret: the line rows' bounds
    /// are captured at paint, and the column is measured, not counted.
    pub(crate) fn render_paragraph(
        &self,
        window: &mut Window,
        on_key: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
        on_seek: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let focused = self.focus.is_focused(window);
        let empty = self.is_empty();
        let value = self.text.value.to_string();
        let lines: Vec<String> = if empty {
            vec![self.placeholder.to_string()]
        } else {
            value.split('\n').map(str::to_string).collect()
        };
        let (caret_line, caret_col) = if empty {
            (0, 0)
        } else {
            caret_line_col(&value, self.text.caret)
        };
        let mut body = div().flex().flex_col().on_children_prepainted({
            let line_bounds = Rc::clone(&self.line_bounds);
            move |bounds, _, _| *line_bounds.borrow_mut() = bounds.clone()
        });
        for (index, line) in lines.iter().enumerate() {
            let text = SharedString::from(line.clone());
            let row = if focused && index == caret_line {
                let before = SharedString::from(line.chars().take(caret_col).collect::<String>());
                let after = SharedString::from(line.chars().skip(caret_col).collect::<String>());
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.0))
                    .child(before)
                    .child(render_caret(&self.name))
                    .child(after)
            } else {
                div().flex().flex_row().gap(px(2.0)).child(text)
            };
            body = body.child(row);
        }
        let line_bounds = Rc::clone(&self.line_bounds);
        let measure_font = self.measure_font();
        let size = RowLayout::small_size();
        let caret_now = self.text.caret;
        let lines_for_click = lines.clone();
        div()
            .id(SharedString::from(format!("paragraph-{}", self.name)))
            .tab_index(self.tab_index)
            .track_focus(&self.focus)
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                let index = match event {
                    gpui::ClickEvent::Mouse(mouse) => seek_index(
                        &line_bounds.borrow(),
                        &lines_for_click,
                        mouse.up.position,
                        window,
                        &measure_font,
                        size,
                    ),
                    gpui::ClickEvent::Keyboard(_) => caret_now,
                };
                on_seek(index, window, cx);
            })
            .on_key_down(on_key)
            .w_full()
            .h(PARAGRAPH_HEIGHT)
            .overflow_y_scroll()
            .scrollbar_width(super::repo::TabLayout::SCROLLBAR)
            .px_2()
            .py_2()
            .rounded_sm()
            .border_1()
            .border_color(if focused {
                gpui::Hsla::from(theme::accent())
            } else {
                gpui::Hsla::from(theme::border())
            })
            .when(focused, |field| field.border_2())
            .bg(theme::surface())
            .text_size(RowLayout::small_size())
            .text_color(if empty {
                theme::text_muted()
            } else {
                theme::text()
            })
            .child(body)
    }

    /// The face the click's columns are measured in: the field's own
    /// when it carries one, the app's otherwise.
    fn measure_font(&self) -> gpui::Font {
        gpui::Font {
            family: self
                .measure_family
                .clone()
                .unwrap_or_else(|| SharedString::from(theme::UI_FONT)),
            features: gpui::FontFeatures::default(),
            fallbacks: None,
            weight: gpui::FontWeight::NORMAL,
            style: gpui::FontStyle::Normal,
        }
    }

    /// The keystroke handling — the text's own.
    pub(crate) fn on_key(&mut self, event: &KeyDownEvent) -> FieldAction {
        self.text.on_key(event)
    }

    /// The field's element: the placeholder when empty, the text when
    /// not. `on_key` is called for every keystroke the field receives.
    ///
    /// The pinned framework has no text input element and no caret, so
    /// the app draws one — a thin bar that blinks while the field has
    /// the focus. The animation runs only then: an always-running
    /// animation makes the window redraw forever.
    ///
    /// Every field in the app paints through this one rule: a `surface`
    /// ground with a hairline `border` when unfocused, the accent ring
    /// added on focus — the ring is an addition, never a replacement —
    /// and a click inside the field never reaches the pane's
    /// click-outside rule.
    pub(crate) fn render(
        &self,
        window: &mut Window,
        on_key: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let empty = self.is_empty();
        let focused = self.focus.is_focused(window);
        let caret: gpui::AnyElement = if focused {
            render_caret(&self.name).into_any_element()
        } else {
            div().w(px(0.0)).into_any_element()
        };
        let mut row = div().flex().flex_row().items_center().gap(px(2.0));
        if empty {
            row = row.child(self.placeholder.clone()).child(caret);
        } else {
            let before = SharedString::from(
                self.value()
                    .chars()
                    .take(self.text.caret)
                    .collect::<String>(),
            );
            let after = SharedString::from(
                self.value()
                    .chars()
                    .skip(self.text.caret)
                    .collect::<String>(),
            );
            row = row.child(before).child(caret).child(after);
        }
        div()
            .id(SharedString::from(format!("text-field-{}", self.name)))
            .tab_index(self.tab_index)
            .track_focus(&self.focus)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                on_click(window, cx);
            })
            .on_key_down(on_key)
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(if focused {
                gpui::Hsla::from(theme::accent())
            } else {
                gpui::Hsla::from(theme::border())
            })
            .when(focused, |field| field.border_2())
            .bg(theme::surface())
            .text_size(RowLayout::small_size())
            .text_color(if empty {
                theme::text_muted()
            } else {
                theme::text()
            })
            .child(row)
    }
}

/// The character column a click at `x` pixels into a line lands on:
/// the first boundary whose shaped prefix passes the click — widths
/// measured, never counted. A click past the line's end lands at its
/// end.
fn column_for_x(window: &Window, text: &str, x: Pixels, font: &gpui::Font, size: Pixels) -> usize {
    let count = text.chars().count();
    if count == 0 {
        return 0;
    }
    let text_system = window.text_system();
    let (mut low, mut high) = (0usize, count);
    while low < high {
        let middle = (low + high) / 2;
        let prefix: String = text.chars().take(middle).collect();
        if shape_width(text_system, &prefix, font, size) <= x {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// The caret index a click in the paragraph box seeks: the line row
/// the click's y is on — the first row whose top is above the click —
/// and the column the click's x measures to on that line.
fn seek_index(
    bounds: &[Bounds<Pixels>],
    lines: &[String],
    position: Point<Pixels>,
    window: &Window,
    font: &gpui::Font,
    size: Pixels,
) -> usize {
    if bounds.is_empty() || lines.is_empty() {
        return 0;
    }
    let mut line = 0usize;
    for (index, row) in bounds.iter().enumerate() {
        if position.y >= row.origin.y {
            line = index;
        } else {
            break;
        }
    }
    let line = line.min(lines.len() - 1);
    let x = (position.x - bounds[line].origin.x).max(px(0.0));
    let column = column_for_x(window, &lines[line], x, font, size);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    index_of_line_col(&refs, line, column)
}

/// The caret: a thin bar that blinks while the field has the focus.
/// The pinned framework has no text input element and no caret of its
/// own, so the app draws one — the reader needs to see that the
/// keystrokes go somewhere.
fn render_caret(name: &str) -> impl gpui::IntoElement {
    let id = SharedString::from(format!("caret-{name}"));
    div()
        .id(id.clone())
        .w(px(2.0))
        .h(px(14.0))
        .bg(theme::accent())
        .with_animation(id, Animation::new(CARET_BLINK).repeat(), |caret, delta| {
            let blink = if delta < 0.5 { 1.0 } else { 0.0 };
            caret.opacity(blink)
        })
}

/// The width a fixed field takes: the jump-to-page field's.
pub(crate) const JUMP_WIDTH: gpui::Pixels = px(120.0);

/// The height a paragraph field takes: the note's own box, tall enough
/// to read a thought in.
pub(crate) const PARAGRAPH_HEIGHT: gpui::Pixels = px(220.0);

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

    fn field(kind: FieldKind) -> FieldText {
        FieldText::new(kind)
    }

    fn typed(kind: FieldKind, text: &str) -> FieldText {
        let mut field = field(kind);
        for character in text.chars() {
            let event = KeyDownEvent {
                keystroke: Keystroke {
                    key: character.to_string(),
                    key_char: Some(character.to_string()),
                    modifiers: gpui::Modifiers::default(),
                },
                is_held: false,
            };
            field.on_key(&event);
        }
        field
    }

    #[test]
    fn a_typed_character_goes_in() {
        let mut field = field(FieldKind::Text);
        assert_eq!(field.on_key(&key_event("a")), FieldAction::Changed);
        assert_eq!(field.value(), "a");
    }

    #[test]
    fn a_digits_field_refuses_letters() {
        let mut field = field(FieldKind::Digits);
        assert_eq!(field.on_key(&key_event("a")), FieldAction::None);
        assert!(field.is_empty());
        assert_eq!(field.on_key(&key_event("7")), FieldAction::Changed);
        assert_eq!(field.value(), "7");
    }

    #[test]
    fn backspace_takes_one_off() {
        let mut field = typed(FieldKind::Text, "ab");
        assert_eq!(field.on_key(&key_event("backspace")), FieldAction::Changed);
        assert_eq!(field.value(), "a");
        assert_eq!(field.caret, 1);
    }

    /// The caret's whole point: an edit lands where the caret is, not
    /// at the end.
    #[test]
    fn an_edit_lands_at_the_caret_not_at_the_end() {
        let mut field = typed(FieldKind::Text, "ac");
        field.on_key(&key_event("left"));
        assert_eq!(field.on_key(&key_event("b")), FieldAction::Changed);
        assert_eq!(field.value(), "abc");
        assert_eq!(field.caret, 2, "the caret rode past the b");
    }

    #[test]
    fn backspace_cuts_before_the_caret_and_delete_at_it() {
        let mut field = typed(FieldKind::Text, "abcd");
        field.on_key(&key_event("left"));
        field.on_key(&key_event("left"));
        assert_eq!(field.caret, 2);
        assert_eq!(field.on_key(&key_event("backspace")), FieldAction::Changed);
        assert_eq!(field.value(), "acd");
        assert_eq!(field.on_key(&key_event("delete")), FieldAction::Changed);
        assert_eq!(field.value(), "ad");
        assert_eq!(field.caret, 1, "delete leaves the caret where it was");
    }

    #[test]
    fn backspace_and_delete_at_their_ends_do_nothing() {
        let mut field = field(FieldKind::Text);
        assert_eq!(field.on_key(&key_event("backspace")), FieldAction::None);
        let mut full = typed(FieldKind::Text, "x");
        full.on_key(&key_event("left"));
        full.on_key(&key_event("right"));
        assert_eq!(full.caret, 1);
        assert_eq!(full.on_key(&key_event("delete")), FieldAction::None);
    }

    #[test]
    fn the_arrows_walk_and_clamp() {
        let mut field = typed(FieldKind::Text, "ab");
        assert_eq!(field.on_key(&key_event("left")), FieldAction::Moved);
        assert_eq!(field.on_key(&key_event("left")), FieldAction::Moved);
        assert_eq!(
            field.on_key(&key_event("left")),
            FieldAction::None,
            "clamped"
        );
        assert_eq!(field.caret, 0);
        assert_eq!(field.on_key(&key_event("right")), FieldAction::Moved);
        assert_eq!(field.caret, 1);
    }

    #[test]
    fn home_and_end_walk_the_line() {
        let mut field = typed(FieldKind::Text, "ab");
        assert_eq!(field.on_key(&key_event("home")), FieldAction::Moved);
        assert_eq!(field.caret, 0);
        assert_eq!(field.on_key(&key_event("home")), FieldAction::None);
        assert_eq!(field.on_key(&key_event("end")), FieldAction::Moved);
        assert_eq!(field.caret, 2);
    }

    /// A paragraph's vertical walk keeps the aimed column when the
    /// line walked onto is shorter, and loses it on a sideways move.
    #[test]
    fn up_and_down_keep_the_aimed_column() {
        let mut field = typed(FieldKind::Paragraph, "long line\nab\nlonger line");
        assert_eq!(field.caret, 24, "typed to the very end");
        field.seek(0);
        for _ in 0..5 {
            field.on_key(&key_event("right"));
        }
        assert_eq!(field.caret, 5, "column 5 on the first line");
        field.on_key(&key_event("down"));
        assert_eq!(
            field.caret, 12,
            "the middle line clamps the column to its end"
        );
        field.on_key(&key_event("down"));
        assert_eq!(field.caret, 18, "the aim survives the short line");
        field.on_key(&key_event("up"));
        assert_eq!(field.caret, 12, "back on the middle line, still clamped");
        field.on_key(&key_event("left"));
        field.on_key(&key_event("down"));
        assert_eq!(field.caret, 14, "a fresh aim of column 1 lands at column 1");
    }

    #[test]
    fn up_at_the_first_line_and_down_at_the_last_do_nothing() {
        let mut field = typed(FieldKind::Paragraph, "a\nb");
        field.seek(0);
        assert_eq!(field.on_key(&key_event("up")), FieldAction::None);
        field.seek(3);
        assert_eq!(field.on_key(&key_event("down")), FieldAction::None);
        assert_eq!(field.caret, 3, "still at the very end");
    }

    #[test]
    fn home_and_end_respect_the_caret_line_in_a_paragraph() {
        let mut field = typed(FieldKind::Paragraph, "first\nsecond");
        assert_eq!(field.caret, 12);
        field.on_key(&key_event("home"));
        assert_eq!(field.caret, 6, "the last line's start");
        field.on_key(&key_event("up"));
        assert_eq!(field.caret, 0, "home aimed column 0, up keeps column 0");
        field.on_key(&key_event("down"));
        assert_eq!(field.caret, 6, "back to the last line's start");
        field.on_key(&key_event("end"));
        assert_eq!(field.caret, 12, "the last line's end");
        field.on_key(&key_event("up"));
        assert_eq!(
            field.caret, 5,
            "aimed column 6 clamps to the first line's end"
        );
    }

    #[test]
    fn a_paragraph_breaks_the_line_instead_of_submitting() {
        let mut field = typed(FieldKind::Paragraph, "a");
        assert_eq!(field.on_key(&key_event("enter")), FieldAction::Changed);
        field.on_key(&key_event("b"));
        assert_eq!(field.value().len(), 3, "the line breaks");
        assert!(field.value().starts_with('a'));
        assert!(field.value().ends_with('b'));
        assert_eq!(
            field.on_key(&key_event("enter")),
            FieldAction::Changed,
            "a paragraph never submits"
        );
    }

    #[test]
    fn a_paragraph_takes_every_character() {
        let mut field = field(FieldKind::Paragraph);
        for _ in 0..5_000 {
            field.on_key(&key_event("x"));
        }
        assert_eq!(field.value().len(), 5_000, "no length is refused");
    }

    #[test]
    fn enter_submits_and_escape_dismisses() {
        let mut field = field(FieldKind::Text);
        field.on_key(&key_event("z"));
        field.on_key(&key_event("e"));
        field.on_key(&key_event("d"));
        assert_eq!(
            field.on_key(&key_event("enter")),
            FieldAction::Submitted("zed".to_string())
        );
        field.on_key(&key_event("z"));
        assert_eq!(field.on_key(&key_event("escape")), FieldAction::Escaped);
        assert!(field.is_empty(), "escape empties the field");
        assert_eq!(field.caret, 0, "and the caret is back at the start");
    }

    #[test]
    fn replacing_the_text_puts_the_caret_at_its_end() {
        let mut field = typed(FieldKind::Text, "abc");
        field.on_key(&key_event("home"));
        field.set("xy");
        assert_eq!(field.caret, 2);
    }

    #[test]
    fn the_seek_clamps_into_the_value() {
        let mut field = typed(FieldKind::Text, "ab");
        field.seek(1);
        assert_eq!(field.caret, 1);
        field.seek(99);
        assert_eq!(field.caret, 2);
    }

    #[test]
    fn the_line_arithmetic_agrees_with_the_split() {
        let value = "one\ntwo\nthree";
        assert_eq!(caret_line_col(value, 0), (0, 0));
        assert_eq!(caret_line_col(value, 3), (0, 3), "on the break = line end");
        assert_eq!(caret_line_col(value, 4), (1, 0));
        assert_eq!(caret_line_col(value, 8), (2, 0));
        assert_eq!(caret_line_col(value, 13), (2, 5));
        let lines = line_texts(value);
        assert_eq!(index_of_line_col(&lines, 0, 2), 2);
        assert_eq!(index_of_line_col(&lines, 1, 0), 4);
        assert_eq!(index_of_line_col(&lines, 2, 3), 11);
        assert_eq!(index_of_line_col(&lines, 2, 99), 13, "clamped to the line");
        assert_eq!(index_of_line_col(&lines, 9, 0), 13, "past the lines = end");
    }

    #[test]
    fn a_caret_on_a_line_break_renders_at_that_lines_end() {
        let value = "ab\ncd";
        assert_eq!(caret_line_col(value, 2), (0, 2));
        assert_eq!(caret_line_col(value, 3), (1, 0));
    }
}
