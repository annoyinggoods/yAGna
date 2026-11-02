//! The type ramp: the sizes every surface paints with, and the font
//! size the reader chose.
//!
//! One place decides the sizes, so the whole app moves together when
//! the reader turns the type up or down. The ramp's base values are the
//! floor every surface respects: nothing functional is painted smaller
//! than the body size, and the row title, the preview and the section
//! headings step up from there.
//!
//! Items: `TypeScale` — the ramp's base sizes and the reader's scale;
//! `set_scale`/`scale` — the applied scale; `scaled` — one base size
//! through the scale.
//! Depends on: gpui (the pixel type).
//! Called by: crate::ui (every surface that paints text).

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::Pixels;

/// The scale the reader chose, as a percentage of the base ramp. The
/// value lives in the settings file; this is the applied copy.
static SCALE: AtomicU8 = AtomicU8::new(100);

/// The smallest scale the ramp accepts: at the floor every base size
/// keeps its guaranteed minimum.
const MIN_SCALE: u8 = 85;

/// The largest scale the ramp accepts: past this the row title stops
/// fitting the row it sits in.
const MAX_SCALE: u8 = 160;

/// The type ramp: the base sizes every surface paints with.
pub(crate) struct TypeScale;

impl TypeScale {
    /// The body size: the smallest size functional text is painted at.
    pub(crate) const BODY: f32 = 14.0;
    /// The row title, one step above the body.
    pub(crate) const TITLE: f32 = 16.0;
    /// The preview text, between the body and the title.
    pub(crate) const PREVIEW: f32 = 13.0;
    /// A section heading, well above the body.
    pub(crate) const HEADING: f32 = 20.0;
    /// The surface's own title — the one h1 a surface carries.
    pub(crate) const SURFACE_TITLE: f32 = 24.0;
    /// The smallest size anything is painted at, whatever the scale.
    pub(crate) const FLOOR: f32 = 12.0;
}

/// The scale the reader chose, as a percentage of the base ramp.
pub(crate) fn scale() -> f32 {
    f32::from(SCALE.load(Ordering::Relaxed)) / 100.0
}

/// Applies the reader's font size: a percentage of the base ramp,
/// clamped to the range the ramp accepts.
pub(crate) fn set_scale(percent: u8) {
    let clamped = percent.clamp(MIN_SCALE, MAX_SCALE);
    SCALE.store(clamped, Ordering::Relaxed);
}

/// One base size through the reader's scale, never below the floor.
pub(crate) fn scaled(base: f32) -> Pixels {
    gpui::px(scaled_points(base))
}

/// One base size in points through the reader's scale, never below the
/// floor. The floor is what keeps a small scale from painting
/// functional text too small to read.
pub(crate) fn scaled_points(base: f32) -> f32 {
    (base * scale()).max(TypeScale::FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale is applied, and it is clamped to the accepted range.
    #[test]
    fn the_scale_applies_and_clamps() {
        set_scale(100);
        assert!((scale() - 1.0).abs() < 0.001);
        set_scale(120);
        assert!((scale() - 1.2).abs() < 0.001);
        set_scale(10);
        assert!((scale() - f32::from(MIN_SCALE) / 100.0).abs() < 0.001);
        set_scale(250);
        assert!((scale() - f32::from(MAX_SCALE) / 100.0).abs() < 0.001);
        set_scale(100);
    }

    /// Nothing is ever painted under the floor, however small the
    /// reader's scale.
    #[test]
    fn the_floor_holds_at_every_scale() {
        for percent in [MIN_SCALE, 100, MAX_SCALE] {
            set_scale(percent);
            assert!(
                scaled_points(TypeScale::BODY) >= TypeScale::FLOOR,
                "the body stays above the floor at {percent}%"
            );
            assert!(
                scaled_points(TypeScale::PREVIEW) >= TypeScale::FLOOR,
                "the preview stays above the floor at {percent}%"
            );
        }
        set_scale(100);
    }

    /// A larger scale paints larger text.
    #[test]
    fn a_larger_scale_paints_larger_text() {
        set_scale(100);
        let base = scaled(TypeScale::BODY);
        set_scale(140);
        assert!(scaled(TypeScale::BODY) > base);
        set_scale(100);
    }
}
