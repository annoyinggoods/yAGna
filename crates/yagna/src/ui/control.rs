//! The shell's controls: the buttons, the chips and the pills, all
//! painted the same way.
//!
//! One place decides how a control looks, so the whole window shares
//! one hit area, one focus ring and one resting ink. A control is
//! never a bare text link: it has a target the reader can see and a
//! focus ring they can follow with the keyboard.
//!
//! Items: `button` — a control that acts; `icon_button` — a control
//! that carries an icon as well as a label; `primary_button` — the one
//! filled control a surface wears; `CONTROL_HEIGHT` — the height every
//! control occupies.
//! Depends on: gpui (the elements), crate::ui::theme (the palette),
//! crate::ui::typography (the sizes).
//! Called by: crate::ui (every surface that draws a control).

use gpui::{ClickEvent, SharedString, Window, div, prelude::*};

use super::icons::Icon;
use super::rows::RowLayout;
use super::theme;
use super::tooltip::TooltipHandle;

/// The height a control occupies: enough for the pointer to find it
/// and for the ring to fit inside.
pub(crate) const CONTROL_HEIGHT: gpui::Pixels = gpui::px(32.0);

/// A control that acts: the resting look, the hover ground, and the
/// click.
pub(crate) fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id.into())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(CONTROL_HEIGHT)
        .px_3()
        .rounded_sm()
        .border_2()
        .border_color(gpui::transparent_black())
        .bg(theme::surface())
        .text_size(RowLayout::small_size())
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_alt()))
        .on_click(on_click)
        .child(label)
}

/// A control that carries an icon as well as a label. The icon never
/// replaces the label — the label is what the control is — and the
/// tooltip repeats the name for the reader who is looking at the
/// shape.
pub(crate) fn icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    tip: &TooltipHandle,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let ink = theme::text();
    let label = label.into();
    let name = label.to_string();
    tip.clone().attach(
        div()
            .id(id.into())
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(CONTROL_HEIGHT)
            .px_3()
            .rounded_sm()
            .border_2()
            .border_color(gpui::transparent_black())
            .bg(theme::surface())
            .text_size(RowLayout::small_size())
            .text_color(ink)
            .cursor_pointer()
            .hover(|style| style.bg(theme::surface_alt()))
            .on_click(on_click)
            .child(super::tabs::icon_element(icon, ink))
            .child(label),
        &name,
    )
}

/// The one filled control on a surface: the action the reader most
/// likely wants. At most one of these wears the accent.
pub(crate) fn primary_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(CONTROL_HEIGHT)
        .px_3()
        .rounded_sm()
        .bg(theme::accent())
        .text_size(RowLayout::small_size())
        .text_color(theme::background())
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .on_click(on_click)
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_is_tall_enough_to_hit() {
        assert!(
            CONTROL_HEIGHT >= gpui::px(30.0),
            "the control is a target the pointer can find"
        );
    }
}
