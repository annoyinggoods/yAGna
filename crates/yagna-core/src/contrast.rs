//! The WCAG 2.1 contrast math: the numbers a palette check needs.
//!
//! The relative luminance of a colour and the contrast ratio between
//! two colours are the arithmetic behind "does this text pass AA?" —
//! the arithmetic lives here, in the pure core, and the palette check
//! elsewhere decides what a ratio means for a given pair.
//!
//! Items: `Rgb` — a colour as three 8-bit channels;
//! `relative_luminance` — the luminance of one colour;
//! `contrast_ratio` — the ratio between two colours (1.0 … 21.0).
//! Depends on: nothing (pure arithmetic on the channels).
//! Called by: the palette's contrast checks.

/// A colour as the contrast math sees it: three 8-bit channels.
pub type Rgb = (u8, u8, u8);

/// The relative luminance of a colour, per WCAG 2.1.
pub fn relative_luminance(colour: Rgb) -> f32 {
    let (red, green, blue) = colour;
    0.2126 * linearise(red) + 0.7152 * linearise(green) + 0.0722 * linearise(blue)
}

/// The contrast ratio between two colours — 1.0 (identical) to 21.0
/// (black on white).
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let first = relative_luminance(a);
    let second = relative_luminance(b);
    let lighter = first.max(second);
    let darker = first.min(second);
    (lighter + 0.05) / (darker + 0.05)
}

/// One sRGB channel, linearized: the dark end is scaled down, the rest
/// is raised to the 2.4 power.
fn linearise(channel: u8) -> f32 {
    let value = channel as f32 / 255.0;
    if value <= 0.03928 {
        return value / 12.92;
    }
    ((value + 0.055) / 1.055).powf(2.4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slack the ratios are compared with: the arithmetic is done
    /// in `f32`, so the last bits wander.
    const EPSILON: f32 = 0.0001;

    #[test]
    fn black_on_white_is_the_maximum_ratio() {
        let ratio = contrast_ratio((0, 0, 0), (255, 255, 255));
        assert!(
            (ratio - 21.0).abs() < EPSILON,
            "black on white is {ratio}, not 21"
        );
    }

    #[test]
    fn white_on_white_is_one() {
        let ratio = contrast_ratio((255, 255, 255), (255, 255, 255));
        assert!((ratio - 1.0).abs() < EPSILON, "white on white is {ratio}");
    }

    #[test]
    fn the_ratio_does_not_depend_on_the_order() {
        let ratio = contrast_ratio((18, 52, 86), (240, 246, 252));
        let flipped = contrast_ratio((240, 246, 252), (18, 52, 86));
        assert!(
            (ratio - flipped).abs() < EPSILON,
            "{ratio} and {flipped} are the same pair"
        );
    }

    #[test]
    fn a_mid_grey_pair_lands_between_one_and_twenty_one() {
        let ratio = contrast_ratio((119, 119, 119), (128, 128, 128));
        assert!(
            ratio > 1.0 && ratio < 21.0,
            "two greys are {ratio}, between the extremes"
        );
    }

    #[test]
    fn the_luminance_of_the_extremes_is_zero_and_one() {
        let black = relative_luminance((0, 0, 0));
        let white = relative_luminance((255, 255, 255));
        assert!(black.abs() < EPSILON, "black is {black}");
        assert!((white - 1.0).abs() < EPSILON, "white is {white}");
    }

    #[test]
    fn the_green_channel_carries_more_weight_than_the_blue_one() {
        let green = relative_luminance((0, 255, 0));
        let blue = relative_luminance((0, 0, 255));
        assert!(green > blue, "green {green} outweighs blue {blue}");
    }
}
