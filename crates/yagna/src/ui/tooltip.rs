//! The tooltip: the name a control is announced with, shown on hover.
//!
//! Every icon in the app carries a name — the shape's own, in the help
//! card's legend. A control that draws only a shape carries that name
//! in a tooltip, so the reader learns what the shape does instead of
//! guessing. One view serves the whole window: the text is shared, and
//! each control's hover writes its own name into it.
//!
//! Items: `Tooltip` — the view the window draws; `TooltipHandle` — the
//! shared text and the view, handed to the controls that need it.
//! Depends on: gpui (the view, the elements), crate::ui::theme (the
//! inks), crate::ui::rows (the size).
//! Called by: crate::ui (the pane installs it), crate::ui::control,
//! crate::ui::tabs and crate::ui::shell (the icon-only controls).

use std::sync::{Arc, Mutex};

use gpui::{AnyView, Context, IntoElement, Render, SharedString, Window, div, prelude::*};

use super::rows::RowLayout;
use super::theme;

/// The tooltip's text, shared between the window and the view.
type Text = Arc<Mutex<Option<SharedString>>>;

/// Reads the shared text, recovering from a poisoned lock: a panic in
/// one hover must not take the tooltip down.
fn read(text: &Text) -> Option<SharedString> {
    text.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Writes the shared text, recovering from a poisoned lock the same
/// way.
fn write(text: &Text, value: Option<SharedString>) {
    let mut slot = text.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = value;
}

/// The tooltip's view: a small card with the control's name in it.
pub(crate) struct Tooltip {
    /// The text the hovered control wrote.
    text: Text,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let text = read(&self.text).unwrap_or_default();
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(theme::surface_alt())
            .border_1()
            .border_color(theme::border())
            .text_size(RowLayout::small_size())
            .text_color(theme::text())
            .child(text)
    }
}

/// The window's tooltip: the shared text and the view every control
/// points at. Cheap to clone — a control takes a copy.
#[derive(Clone)]
pub(crate) struct TooltipHandle {
    /// The text the hovered control wrote.
    text: Text,
    /// The view the window draws for the tooltip.
    view: AnyView,
}

impl TooltipHandle {
    /// Installs the tooltip for a window: one view, one shared text.
    pub(crate) fn install<T: 'static>(cx: &mut Context<T>) -> Self {
        let text: Text = Arc::new(Mutex::new(None));
        let entity = cx.new(|_| Tooltip {
            text: Arc::clone(&text),
        });
        Self {
            text,
            view: AnyView::from(entity),
        }
    }

    /// Attaches the tooltip to a control: hovering it writes the name
    /// into the shared text, and leaving takes it away.
    pub(crate) fn attach<E>(&self, element: E, label: &str) -> E
    where
        E: gpui::StatefulInteractiveElement,
    {
        let hovered_text = Arc::clone(&self.text);
        let tooltip_text = Arc::clone(&self.text);
        let view = self.view.clone();
        let name = SharedString::from(label.to_string());
        element
            .on_hover(move |hovered, window, _app| {
                if !hovered {
                    write(&hovered_text, None);
                    window.refresh();
                }
            })
            .tooltip(move |_window, _app| {
                write(&tooltip_text, Some(name.clone()));
                view.clone()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_round_trips() {
        let text: Text = Arc::new(Mutex::new(None));
        assert_eq!(read(&text), None);
        write(&text, Some(SharedString::from("close")));
        assert_eq!(
            read(&text).map(|value| value.to_string()),
            Some("close".into())
        );
        write(&text, None);
        assert_eq!(read(&text), None);
    }
}
