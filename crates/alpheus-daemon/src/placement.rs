//! Where a bar's layer surface sits, per style (themes delivery 2, spec § 3). `floating` moves it
//! 6 px off the output's top, left and right edges, makes the surface taller by its shadow, and
//! keeps input to the bar. Every other style keeps today's placement.

use alpheus_common::theme::settings::BarStyle;

/// Logical px between a floating bar and the output's top, left and right edges.
pub const FLOAT_MARGIN: u32 = 6;
/// Logical px of the floating bar's shadow, below it, inside the surface.
pub const FLOAT_SHADOW: u32 = 4;
/// The input region's width, logical px: wider than any output, clipped by the compositor to the
/// surface (its real width is only known after the configure).
pub const INPUT_WIDTH: i32 = 1 << 16;

/// A bar's layer-surface placement, logical px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Top, right, bottom, left: `LayerSurface::set_margin`'s order.
    pub margin: (i32, i32, i32, i32),
    pub surface_height: u32,
    pub exclusive_zone: i32,
    /// The input region's height from the surface's top; None: the whole surface (the default).
    pub input_height: Option<u32>,
}

fn int(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

impl Placement {
    /// The placement of a bar `bar_height` logical px tall under `style`.
    pub fn for_style(style: BarStyle, bar_height: u32) -> Placement {
        match style {
            BarStyle::Floating => {
                let m = int(FLOAT_MARGIN);
                Placement {
                    margin: (m, m, 0, m),
                    surface_height: bar_height.saturating_add(FLOAT_SHADOW),
                    // The bar alone: the compositor adds the top margin itself (spec amendment
                    // ad309a0), so windows start just under the floating bar.
                    exclusive_zone: int(bar_height),
                    input_height: Some(bar_height),
                }
            }
            BarStyle::Flat | BarStyle::Islands | BarStyle::Powerline | BarStyle::Underline | BarStyle::Translucent => {
                Placement { margin: (0, 0, 0, 0), surface_height: bar_height, exclusive_zone: int(bar_height), input_height: None }
            }
        }
    }
}

/// The input region's one rectangle (x, y, width, height), surface-local logical px; None: the
/// default region, the whole surface.
pub fn input_rect(p: &Placement) -> Option<(i32, i32, i32, i32)> {
    p.input_height.map(|h| (0, 0, INPUT_WIDTH, int(h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec § 3 (amended ad309a0):
    /// - margin 6 top, left and right;
    /// - the surface the bar plus a 4 px shadow;
    /// - exclusive zone = the bar height (compositors add the 6 px margin themselves);
    /// - input on the bar only.
    #[test]
    fn floating_is_moved_off_the_edges_and_reserves_the_bar_height() {
        let p = Placement::for_style(BarStyle::Floating, 22);
        assert_eq!(p, Placement { margin: (6, 6, 0, 6), surface_height: 26, exclusive_zone: 22, input_height: Some(22) });
        assert_eq!(Placement::for_style(BarStyle::Floating, 30).exclusive_zone, 30);
    }

    #[test]
    fn every_other_style_keeps_todays_placement() {
        for style in [BarStyle::Flat, BarStyle::Islands, BarStyle::Powerline, BarStyle::Underline, BarStyle::Translucent] {
            let today = Placement { margin: (0, 0, 0, 0), surface_height: 22, exclusive_zone: 22, input_height: None };
            assert_eq!(Placement::for_style(style, 22), today, "{style:?}");
        }
    }

    /// Spec § 3: the input region covers the bar, never the shadow; other styles keep the default
    /// (the whole surface, so clicks in the islands gaps still reach the bar's zones, spec § 4).
    #[test]
    fn the_input_region_covers_the_bar_only() {
        assert_eq!(input_rect(&Placement::for_style(BarStyle::Floating, 22)), Some((0, 0, INPUT_WIDTH, 22)));
        assert_eq!(input_rect(&Placement::for_style(BarStyle::Islands, 22)), None);
    }
}
