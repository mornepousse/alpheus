//! WCAG 2 contrast (relative luminance, contrast ratio), used by the
//! presets' legibility test and available to the Appearance tab later.

use super::Rgb;

/// WCAG 2 relative luminance: sRGB channels linearised (0.04045 threshold),
/// weighted 0.2126 / 0.7152 / 0.0722. 0.0 for black, 1.0 for white.
pub fn luminance(Rgb(r, g, b): Rgb) -> f32 {
    fn linear(c: u8) -> f32 {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// WCAG 2 contrast ratio, (lighter + 0.05) / (darker + 0.05): 1.0 to 21.0,
/// whatever the order of `a` and `b`.
pub fn ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn luminance_of_the_extremes_and_of_each_channel() {
        assert_eq!(luminance(Rgb(0, 0, 0)), 0.0);
        assert!(close(luminance(Rgb(255, 255, 255)), 1.0, 1e-6));
        assert!(close(luminance(Rgb(255, 0, 0)), 0.2126, 1e-6));
        assert!(close(luminance(Rgb(0, 255, 0)), 0.7152, 1e-6));
        assert!(close(luminance(Rgb(0, 0, 255)), 0.0722, 1e-6));
    }

    #[test]
    fn luminance_follows_both_segments_of_the_srgb_curve() {
        // 10/255 = 0.0392 is on the linear segment (c / 12.92); 128/255 on the power one.
        assert!(close(luminance(Rgb(10, 10, 10)), 0.003_035_27, 1e-6));
        assert!(close(luminance(Rgb(128, 128, 128)), 0.215_860_5, 1e-5));
    }

    #[test]
    fn ratio_is_symmetric_and_at_least_one() {
        assert!(close(ratio(Rgb(0, 0, 0), Rgb(255, 255, 255)), 21.0, 1e-4));
        assert!(close(ratio(Rgb(255, 255, 255), Rgb(0, 0, 0)), 21.0, 1e-4));
        assert!(close(ratio(Rgb(30, 30, 46), Rgb(30, 30, 46)), 1.0, 1e-6));
    }

    #[test]
    fn ratio_of_a_known_pair() {
        // Catppuccin Mocha Text on Base, computed with the WCAG formula: 11.3411.
        assert!(close(ratio(Rgb(0xcd, 0xd6, 0xf4), Rgb(0x1e, 0x1e, 0x2e)), 11.3411, 1e-3));
    }
}
