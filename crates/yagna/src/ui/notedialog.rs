//! The note's editor: the popup the reader writes a note in.
//!
//! The note is the reader's own words about one repository, so the
//! editor is a paragraph — as long as the reader's thoughts, with no
//! cap and no markup. It opens over the page, it saves with its own
//! button, it deletes with its own button, and it closes without
//! saving on escape or on a click outside it (the app's one rule).
//!
//! Items: `NoteActions` — the closures the pane hands the popup;
//! `render` — the popup.
//! Depends on: gpui (the elements, the key events), crate::ui::field
//! (the paragraph field), crate::ui::theme (the palette).
//! Called by: crate::ui (the list pane, when the note's editor is
//! open).

use std::rc::Rc;

use gpui::{App, KeyDownEvent, Window, div, prelude::*, px};

use super::field::TextField;
use super::rows::RowLayout;
use super::theme;

/// A keystroke in the paragraph: the pane reads it and decides (escape
/// closes, a character changes the text).
pub(crate) type KeyHandler = Rc<dyn Fn(&KeyDownEvent, &mut Window, &mut App)>;

/// One of the popup's buttons.
pub(crate) type ActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A click in the paragraph seeks the caret to the clicked place.
pub(crate) type SeekHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// What the popup's parts report.
pub(crate) struct NoteActions {
    /// A keystroke in the paragraph.
    pub on_key: KeyHandler,
    /// A click in the paragraph: the caret seeks the click.
    pub on_seek: SeekHandler,
    /// The note is saved — the pane reads the field's own text.
    pub on_save: ActionHandler,
    /// The note is deleted.
    pub on_delete: ActionHandler,
    /// The popup closes without saving.
    pub on_close: ActionHandler,
}

/// The popup's width: wide enough for a paragraph, narrow enough to
/// sit over a list.
const POPUP_WIDTH: gpui::Pixels = px(640.0);

/// The popup: the title, the paragraph, and the two buttons.
pub(crate) fn render(
    field: &TextField,
    has_note: bool,
    actions: &NoteActions,
    window: &mut Window,
) -> gpui::Stateful<gpui::Div> {
    let on_close = Rc::clone(&actions.on_close);
    div()
        .id("note-modal")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::Hsla::from(theme::background()).opacity(0.75))
        .on_click(move |_, window, cx| on_close(window, cx))
        .child(
            div()
                .id("note-panel")
                .w(POPUP_WIDTH)
                .max_h(px(600.0))
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_md()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .text_size(RowLayout::top_size())
                        .text_color(theme::text())
                        .child("Add a note:"),
                )
                .child(field.render_paragraph(
                    window,
                    {
                        let on_key = Rc::clone(&actions.on_key);
                        move |event, window, cx| on_key(event, window, cx)
                    },
                    {
                        let on_seek = Rc::clone(&actions.on_seek);
                        move |index, window, cx| on_seek(index, window, cx)
                    },
                ))
                .child(
                    div()
                        .text_size(RowLayout::small_size())
                        .text_color(theme::text_muted())
                        .child("ctrl+enter saves · esc cancels · a click outside cancels"),
                )
                .child(buttons(has_note, actions)),
        )
}

/// The two buttons: delete and save, side by side at the right.
fn buttons(has_note: bool, actions: &NoteActions) -> gpui::Div {
    let on_save = Rc::clone(&actions.on_save);
    let on_delete = Rc::clone(&actions.on_delete);
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .gap_2()
        .child(
            div()
                .id("note-delete")
                .px_3()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(if has_note {
                    theme::text_muted()
                } else {
                    theme::border()
                })
                .on_click(move |_, window, cx| on_delete(window, cx))
                .child("delete"),
        )
        .child(
            div()
                .id("note-save")
                .px_3()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_alt()))
                .text_size(RowLayout::small_size())
                .text_color(theme::accent())
                .on_click(move |_, window, cx| on_save(window, cx))
                .child("save"),
        )
}
