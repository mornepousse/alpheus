//! Compiled-in themes (spec § 2): fifteen families, each a dark half and (but
//! for Nord, Dracula and Phosphor) a light half of the 16 roles, taken from the
//! official palettes (the last six are the UI refresh's own designs). Role
//! choices follow Catppuccin's UI style guide and map the other families by
//! analogy (themes plan). Where an official
//! colour failed the contrast test, the nearest passing palette colour is
//! used and the comment says so. Each half also carries the seven named hues
//! (spec § 9) from the same palette. About 1.9 KB of read-only data in all.

use super::{Hues, Rgb, Roles};

/// One theme family: its dark half, and its light half when it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub dark: Roles,
    pub light: Option<Roles>,
}

/// `0xrrggbb` → colour, at compile time.
const fn h(x: u32) -> Rgb {
    Rgb((x >> 16) as u8, (x >> 8) as u8, x as u8)
}

/// Catppuccin Mocha.
pub(crate) const CATPPUCCIN_DARK: Roles = Roles {
    bar_bg: h(0x181825),            // Mantle
    pill_bg: h(0x313244),           // Surface0
    popup_bg: h(0x1e1e2e),          // Base
    raised: h(0x45475a),            // Surface1
    selection: h(0x585b70),         // Surface2
    fg: h(0xcdd6f4),                // Text
    fg_dim: h(0x7f849c),            // Overlay1
    on_accent: h(0x1e1e2e),         // Base
    accent: h(0xcba6f7),            // Mauve
    accent_container: h(0xcba6f7),  // Mauve
    accent2: h(0x89b4fa),           // Blue
    accent2_container: h(0x89b4fa), // Blue
    ok: h(0xa6e3a1),                // Green
    warn: h(0xf9e2af),              // Yellow
    error: h(0xf38ba8),             // Red
    urgent: h(0xfab387),            // Peach
    hues: Hues {
        red: h(0xf38ba8),    // Red
        orange: h(0xfab387), // Peach
        yellow: h(0xf9e2af), // Yellow
        green: h(0xa6e3a1),  // Green
        cyan: h(0x94e2d5),   // Teal
        blue: h(0x89b4fa),   // Blue
        purple: h(0xcba6f7), // Mauve
    },
};

/// Catppuccin Latte.
pub(super) const CATPPUCCIN_LIGHT: Roles = Roles {
    bar_bg: h(0xe6e9ef),            // Mantle
    pill_bg: h(0xccd0da),           // Surface0
    popup_bg: h(0xeff1f5),          // Base
    raised: h(0xbcc0cc),            // Surface1
    selection: h(0xacb0be),         // Surface2
    fg: h(0x4c4f69),                // Text
    fg_dim: h(0x6c6f85),            // Subtext0 (Overlay1 is 2.63:1 on Mantle)
    on_accent: h(0xeff1f5),         // Base
    accent: h(0x8839ef),            // Mauve
    accent_container: h(0x8839ef),  // Mauve
    accent2: h(0x1e66f5),           // Blue
    accent2_container: h(0x1e66f5), // Blue
    ok: h(0x3c9628),                // Green darkened to 94 % (official #40a02b is 2.75:1 on Mantle)
    warn: h(0xb77418),              // Yellow darkened to 82 % (official #df8e1d is 2.15:1)
    error: h(0xd20f39),             // Red
    urgent: h(0xe0580a),            // Peach darkened to 88 % (official #fe640b is 2.45:1)
    hues: Hues {
        red: h(0xd20f39),    // Red
        orange: h(0xe0580a), // Peach darkened to 88 % (official #fe640b is 2.45:1)
        yellow: h(0xb77418), // Yellow darkened to 82 % (official #df8e1d is 2.15:1)
        green: h(0x3c9628),  // Green darkened to 94 % (official #40a02b is 2.75:1)
        cyan: h(0x179299),   // Teal
        blue: h(0x1e66f5),   // Blue
        purple: h(0x8839ef), // Mauve
    },
};

/// Gruvbox dark (morhetz).
const GRUVBOX_DARK: Roles = Roles {
    bar_bg: h(0x1d2021),            // bg0_h
    pill_bg: h(0x3c3836),           // bg1
    popup_bg: h(0x282828),          // bg0
    raised: h(0x504945),            // bg2
    selection: h(0x665c54),         // bg3
    fg: h(0xebdbb2),                // fg1
    fg_dim: h(0xa89984),            // fg4
    on_accent: h(0x282828),         // bg0
    accent: h(0xfe8019),            // bright_orange
    accent_container: h(0xfe8019),  // bright_orange
    accent2: h(0x8ec07c),           // bright_aqua
    accent2_container: h(0x8ec07c), // bright_aqua
    ok: h(0xb8bb26),                // bright_green
    warn: h(0xfabd2f),              // bright_yellow
    error: h(0xfb4934),             // bright_red
    urgent: h(0xfb4934),            // bright_red
    hues: Hues {
        red: h(0xfb4934),    // bright_red
        orange: h(0xfe8019), // bright_orange
        yellow: h(0xfabd2f), // bright_yellow
        green: h(0xb8bb26),  // bright_green
        cyan: h(0x8ec07c),   // bright_aqua
        blue: h(0x83a598),   // bright_blue
        purple: h(0xd3869b), // bright_purple
    },
};

/// Gruvbox light (morhetz).
const GRUVBOX_LIGHT: Roles = Roles {
    bar_bg: h(0xf2e5bc),            // bg0_s
    pill_bg: h(0xebdbb2),           // bg1
    popup_bg: h(0xfbf1c7),          // bg0
    raised: h(0xd5c4a1),            // bg2
    selection: h(0xbdae93),         // bg3
    fg: h(0x3c3836),                // fg1
    fg_dim: h(0x7c6f64),            // fg4
    on_accent: h(0xfbf1c7),         // bg0
    accent: h(0xaf3a03),            // faded_orange
    accent_container: h(0xaf3a03),  // faded_orange
    accent2: h(0x427b58),           // faded_aqua
    accent2_container: h(0x427b58), // faded_aqua
    ok: h(0x79740e),                // faded_green
    warn: h(0xb57614),              // faded_yellow
    error: h(0x9d0006),             // faded_red
    urgent: h(0x9d0006),            // faded_red
    hues: Hues {
        red: h(0x9d0006),    // faded_red
        orange: h(0xaf3a03), // faded_orange
        yellow: h(0xb57614), // faded_yellow
        green: h(0x79740e),  // faded_green
        cyan: h(0x427b58),   // faded_aqua
        blue: h(0x076678),   // faded_blue
        purple: h(0x8f3f71), // faded_purple
    },
};

/// Tokyo Night (Night).
const TOKYO_NIGHT_DARK: Roles = Roles {
    bar_bg: h(0x16161e),            // bg_dark
    pill_bg: h(0x292e42),           // bg_highlight
    popup_bg: h(0x1a1b26),          // bg
    raised: h(0x3b4261),            // fg_gutter
    selection: h(0x394b70),         // blue7
    fg: h(0xc0caf5),                // fg
    fg_dim: h(0x737aa2),            // dark5
    on_accent: h(0x1a1b26),         // bg
    accent: h(0x7aa2f7),            // blue
    accent_container: h(0x7aa2f7),  // blue
    accent2: h(0xbb9af7),           // magenta
    accent2_container: h(0xbb9af7), // magenta
    ok: h(0x9ece6a),                // green
    warn: h(0xe0af68),              // yellow
    error: h(0xf7768e),             // red
    urgent: h(0xff9e64),            // orange
    hues: Hues {
        red: h(0xf7768e),    // red
        orange: h(0xff9e64), // orange
        yellow: h(0xe0af68), // yellow
        green: h(0x9ece6a),  // green
        cyan: h(0x7dcfff),   // cyan
        blue: h(0x7aa2f7),   // blue
        purple: h(0xbb9af7), // magenta
    },
};

/// Tokyo Night (Day).
const TOKYO_NIGHT_LIGHT: Roles = Roles {
    bar_bg: h(0xe1e2e7),            // bg (fg on bg_dark is 3.99:1)
    pill_bg: h(0xd0d5e3),           // bg_dark
    popup_bg: h(0xe1e2e7),          // bg
    raised: h(0xc1c9df),            // a step off bg_dark, fg ≥ 3.5:1 (Mae, 2026-10-08)
    selection: h(0xc4c8da),         // bg_highlight
    fg: h(0x3760bf),                // fg
    fg_dim: h(0x68709a),            // dark5
    on_accent: h(0xe1e2e7),         // bg
    accent: h(0x2e7de9),            // blue
    accent_container: h(0x3760bf),  // fg, the darker blue (bg on blue is 3.11:1)
    accent2: h(0x9854f1),           // magenta
    accent2_container: h(0x7847bd), // purple
    ok: h(0x587539),                // green
    warn: h(0x8c6c3e),              // yellow
    error: h(0xf52a65),             // red
    urgent: h(0xb15c00),            // orange
    hues: Hues {
        red: h(0xf52a65),    // red
        orange: h(0xb15c00), // orange
        yellow: h(0x8c6c3e), // yellow
        green: h(0x587539),  // green
        cyan: h(0x007197),   // cyan
        blue: h(0x2e7de9),   // blue
        purple: h(0x9854f1), // magenta
    },
};

/// Rose Pine (main).
const ROSE_PINE_DARK: Roles = Roles {
    bar_bg: h(0x191724),            // base
    pill_bg: h(0x1f1d2e),           // surface
    popup_bg: h(0x26233a),          // overlay
    raised: h(0x403d52),            // highlightMed
    selection: h(0x524f67),         // highlightHigh
    fg: h(0xe0def4),                // text
    fg_dim: h(0x908caa),            // subtle
    on_accent: h(0x191724),         // base
    accent: h(0xc4a7e7),            // iris
    accent_container: h(0xc4a7e7),  // iris
    accent2: h(0xebbcba),           // rose
    accent2_container: h(0xebbcba), // rose
    ok: h(0x9ccfd8),                // foam
    warn: h(0xf6c177),              // gold
    error: h(0xeb6f92),             // love
    urgent: h(0xeb6f92),            // love
    hues: Hues {
        red: h(0xeb6f92),    // love
        orange: h(0xebbcba), // rose (no orange in the palette)
        yellow: h(0xf6c177), // gold
        green: h(0x31748f),  // pine (no green in the palette)
        cyan: h(0x9ccfd8),   // foam
        blue: h(0xc4a7e7),   // iris (no blue in the palette; also the purple)
        purple: h(0xc4a7e7), // iris
    },
};

/// Rose Pine Dawn.
const ROSE_PINE_LIGHT: Roles = Roles {
    bar_bg: h(0xfaf4ed),            // base
    pill_bg: h(0xfffaf3),           // surface
    popup_bg: h(0xf2e9e1),          // overlay
    raised: h(0xdfdad9),            // highlightMed
    selection: h(0xcecacd),         // highlightHigh
    fg: h(0x575279),                // text
    fg_dim: h(0x797593),            // subtle
    on_accent: h(0xfaf4ed),         // base
    accent: h(0x907aa9),            // iris
    accent_container: h(0x286983),  // pine (base on iris is 3.47:1)
    accent2: h(0xd7827e),           // rose
    accent2_container: h(0xd7827e), // rose
    ok: h(0x56949f),                // foam
    warn: h(0xbe7f2a),              // gold darkened to 81 % (official #ea9d34 is 2.05:1)
    error: h(0xb4637a),             // love
    urgent: h(0xb4637a),            // love
    hues: Hues {
        red: h(0xb4637a),    // love
        orange: h(0xc67874), // rose darkened to 92 % (official #d7827e is 2.60:1; no orange in the palette)
        yellow: h(0xbe7f2a), // gold darkened to 81 % (official #ea9d34 is 2.05:1)
        green: h(0x286983),  // pine (no green in the palette)
        cyan: h(0x56949f),   // foam
        blue: h(0x907aa9),   // iris (no blue in the palette; also the purple)
        purple: h(0x907aa9), // iris
    },
};

/// Everforest dark medium.
const EVERFOREST_DARK: Roles = Roles {
    bar_bg: h(0x232a2e),            // bg_dim
    pill_bg: h(0x343f44),           // bg1
    popup_bg: h(0x2d353b),          // bg0
    raised: h(0x3d484d),            // bg2
    selection: h(0x543a48),         // bg_visual
    fg: h(0xd3c6aa),                // fg
    fg_dim: h(0x859289),            // grey1
    on_accent: h(0x2d353b),         // bg0
    accent: h(0xa7c080),            // green
    accent_container: h(0xa7c080),  // green
    accent2: h(0x7fbbb3),           // blue
    accent2_container: h(0x7fbbb3), // blue
    ok: h(0x83c092),                // aqua
    warn: h(0xdbbc7f),              // yellow
    error: h(0xe67e80),             // red
    urgent: h(0xe69875),            // orange
    hues: Hues {
        red: h(0xe67e80),    // red
        orange: h(0xe69875), // orange
        yellow: h(0xdbbc7f), // yellow
        green: h(0xa7c080),  // green
        cyan: h(0x83c092),   // aqua
        blue: h(0x7fbbb3),   // blue
        purple: h(0xd699b6), // purple
    },
};

/// Everforest light medium.
const EVERFOREST_LIGHT: Roles = Roles {
    bar_bg: h(0xefebd4),            // bg_dim
    pill_bg: h(0xe6e2cc),           // bg3
    popup_bg: h(0xfdf6e3),          // bg0
    raised: h(0xe0dcc7),            // bg4
    selection: h(0xeaedc8),         // bg_visual
    fg: h(0x5c6a72),                // fg
    fg_dim: h(0x708089),            // statusline2 (grey2 is 2.77:1)
    on_accent: h(0xfdf6e3),         // bg0
    accent: h(0x8da101),            // green
    accent_container: h(0x5c6a72),  // fg (bg0 on green is 2.69:1)
    accent2: h(0x3a94c5),           // blue
    accent2_container: h(0x3a94c5), // blue
    ok: h(0x309670),                // aqua darkened to 90 % (official #35a77c is 2.51:1)
    warn: h(0xae7d00),              // yellow darkened to 78 % (official #dfa000 is 1.91:1)
    error: h(0xe9504d),             // red darkened to 94 % (official #f85552 is 2.73:1)
    urgent: h(0xce6920),            // orange darkened to 84 % (official #f57d26 is 2.23:1)
    hues: Hues {
        red: h(0xe9504d),    // red darkened to 94 % (official #f85552 is 2.73:1)
        orange: h(0xce6920), // orange darkened to 84 % (official #f57d26 is 2.23:1)
        yellow: h(0xae7d00), // yellow darkened to 78 % (official #dfa000 is 1.91:1)
        green: h(0x7d8f01),  // green darkened to 89 % (official #8da101 is 2.42:1)
        cyan: h(0x309670),   // aqua darkened to 90 % (official #35a77c is 2.51:1)
        blue: h(0x388ebd),   // blue darkened to 96 % (official #3a94c5 is 2.81:1)
        purple: h(0xcb60a9), // purple darkened to 91 % (official #df69ba is 2.55:1)
    },
};

/// Kanagawa Wave.
const KANAGAWA_DARK: Roles = Roles {
    bar_bg: h(0x16161d),            // sumiInk0
    pill_bg: h(0x2a2a37),           // sumiInk4
    popup_bg: h(0x1f1f28),          // sumiInk3
    raised: h(0x363646),            // sumiInk5
    selection: h(0x223249),         // waveBlue1
    fg: h(0xdcd7ba),                // fujiWhite
    fg_dim: h(0x727169),            // fujiGray
    on_accent: h(0x1f1f28),         // sumiInk3
    accent: h(0x7e9cd8),            // crystalBlue
    accent_container: h(0x7e9cd8),  // crystalBlue
    accent2: h(0x957fb8),           // oniViolet
    accent2_container: h(0x957fb8), // oniViolet
    ok: h(0x98bb6c),                // springGreen
    warn: h(0xff9e3b),              // roninYellow
    error: h(0xe82424),             // samuraiRed
    urgent: h(0xffa066),            // surimiOrange
    hues: Hues {
        red: h(0xc34043),    // autumnRed
        orange: h(0xffa066), // surimiOrange
        yellow: h(0xe6c384), // carpYellow
        green: h(0x98bb6c),  // springGreen
        cyan: h(0x6a9589),   // waveAqua1
        blue: h(0x7e9cd8),   // crystalBlue
        purple: h(0x957fb8), // oniViolet
    },
};

/// Kanagawa Lotus.
const KANAGAWA_LIGHT: Roles = Roles {
    bar_bg: h(0xd5cea3),            // lotusWhite0
    pill_bg: h(0xe7dba0),           // lotusWhite4
    popup_bg: h(0xf2ecbc),          // lotusWhite3
    raised: h(0xe4d794),            // lotusWhite5
    selection: h(0xc9cbd1),         // lotusViolet3
    fg: h(0x545464),                // lotusInk1
    fg_dim: h(0x716e61),            // lotusGray2
    on_accent: h(0xf2ecbc),         // lotusWhite3
    accent: h(0x4d699b),            // lotusBlue4
    accent_container: h(0x4d699b),  // lotusBlue4
    accent2: h(0x624c83),           // lotusViolet4
    accent2_container: h(0x624c83), // lotusViolet4
    ok: h(0x617744),                // lotusGreen darkened to 87 % (official #6f894e is 2.46:1)
    warn: h(0xa36100),              // lotusOrange2 darkened to 70 % (official #e98a00 is 1.63:1)
    error: h(0xdc2222),             // lotusRed3 darkened to 95 % (official #e82424 is 2.80:1)
    urgent: h(0xab5c00),            // lotusOrange darkened to 84 % (official #cc6d00 is 2.29:1)
    hues: Hues {
        red: h(0xdc2222),    // lotusRed3 darkened to 95 % (official #e82424 is 2.80:1)
        orange: h(0xab5c00), // lotusOrange darkened to 84 % (official #cc6d00 is 2.29:1)
        yellow: h(0x77713f), // lotusYellow
        green: h(0x617744),  // lotusGreen darkened to 87 % (official #6f894e is 2.46:1)
        cyan: h(0x577973),   // lotusAqua darkened to 98 % (official #597b75 is 2.92:1)
        blue: h(0x4d699b),   // lotusBlue4
        purple: h(0x624c83), // lotusViolet4
    },
};

/// Solarized dark.
const SOLARIZED_DARK: Roles = Roles {
    bar_bg: h(0x002b36),            // base03
    pill_bg: h(0x073642),           // base02
    popup_bg: h(0x002b36),          // base03
    raised: h(0x133f4a),            // a step up from base02, fg ≥ 3.5:1 (Mae, 2026-10-08)
    selection: h(0x073642),         // base02
    fg: h(0x839496),                // base0
    fg_dim: h(0x657b83),            // base00
    on_accent: h(0x002b36),         // base03
    accent: h(0x268bd2),            // blue
    accent_container: h(0x2aa198),  // cyan (base03 on blue is 4.08:1)
    accent2: h(0x6c71c4),           // violet
    accent2_container: h(0x6c71c4), // violet
    ok: h(0x859900),                // green
    warn: h(0xb58900),              // yellow
    error: h(0xdc322f),             // red
    urgent: h(0xcb4b16),            // orange
    hues: Hues {
        red: h(0xdc322f),    // red
        orange: h(0xcb4b16), // orange
        yellow: h(0xb58900), // yellow
        green: h(0x859900),  // green
        cyan: h(0x2aa198),   // cyan
        blue: h(0x268bd2),   // blue
        purple: h(0x6c71c4), // violet
    },
};

/// Solarized light.
const SOLARIZED_LIGHT: Roles = Roles {
    bar_bg: h(0xfdf6e3),            // base3
    pill_bg: h(0xeee8d5),           // base2
    popup_bg: h(0xfdf6e3),          // base3
    raised: h(0xdfdccb),            // a step down from base2, fg ≥ 3.5:1 (Mae, 2026-10-08)
    selection: h(0xeee8d5),         // base2
    fg: h(0x586e75),                // base01 (base00 is 4.13:1)
    fg_dim: h(0x657b83),            // base00
    on_accent: h(0x002b36),         // base03
    accent: h(0x268bd2),            // blue
    accent_container: h(0x2aa198),  // cyan (base03 on blue is 4.08:1)
    accent2: h(0x6c71c4),           // violet
    accent2_container: h(0x6c71c4), // violet
    ok: h(0x829600),                // green darkened to 98 % (official #859900 is 2.97:1 on base3)
    warn: h(0xb18600),              // yellow darkened to 98 % (official #b58900 is 2.98:1)
    error: h(0xdc322f),             // red
    urgent: h(0xcb4b16),            // orange
    hues: Hues {
        red: h(0xdc322f),    // red
        orange: h(0xcb4b16), // orange
        yellow: h(0xb18600), // yellow darkened to 98 % (official #b58900 is 2.98:1)
        green: h(0x829600),  // green darkened to 98 % (official #859900 is 2.97:1)
        cyan: h(0x299e95),   // cyan darkened to 98 % (official #2aa198 is 2.93:1)
        blue: h(0x268bd2),   // blue
        purple: h(0x6c71c4), // violet
    },
};

/// Nord.
const NORD_DARK: Roles = Roles {
    bar_bg: h(0x2e3440),            // nord0 (nord11 error is 2.46:1 on nord1)
    pill_bg: h(0x434c5e),           // nord2
    popup_bg: h(0x3b4252),          // nord1
    raised: h(0x4c566a),            // nord3
    selection: h(0x434c5e),         // nord2
    fg: h(0xd8dee9),                // nord4
    fg_dim: h(0x81a1c1),            // nord9 (nord3 is 1.36:1 on nord1)
    on_accent: h(0x2e3440),         // nord0
    accent: h(0x88c0d0),            // nord8
    accent_container: h(0x88c0d0),  // nord8
    accent2: h(0xb48ead),           // nord15
    accent2_container: h(0xb48ead), // nord15
    ok: h(0xa3be8c),                // nord14
    warn: h(0xebcb8b),              // nord13
    error: h(0xbf616a),             // nord11
    urgent: h(0xd08770),            // nord12
    hues: Hues {
        red: h(0xbf616a),    // nord11
        orange: h(0xd08770), // nord12
        yellow: h(0xebcb8b), // nord13
        green: h(0xa3be8c),  // nord14
        cyan: h(0x88c0d0),   // nord8
        blue: h(0x81a1c1),   // nord9 (nord10 #5e81ac is too dark on nord0)
        purple: h(0xb48ead), // nord15
    },
};

/// Dracula.
const DRACULA_DARK: Roles = Roles {
    bar_bg: h(0x21222c),            // BGDark
    pill_bg: h(0x343746),           // BGLight
    popup_bg: h(0x282a36),          // Background
    raised: h(0x424450),            // BGLighter
    selection: h(0x44475a),         // Selection
    fg: h(0xf8f8f2),                // Foreground
    fg_dim: h(0x6272a4),            // Comment
    on_accent: h(0x282a36),         // Background
    accent: h(0xbd93f9),            // Purple
    accent_container: h(0xbd93f9),  // Purple
    accent2: h(0xff79c6),           // Pink
    accent2_container: h(0xff79c6), // Pink
    ok: h(0x50fa7b),                // Green
    warn: h(0xf1fa8c),              // Yellow
    error: h(0xff5555),             // Red
    urgent: h(0xffb86c),            // Orange
    hues: Hues {
        red: h(0xff5555),    // Red
        orange: h(0xffb86c), // Orange
        yellow: h(0xf1fa8c), // Yellow
        green: h(0x50fa7b),  // Green
        cyan: h(0x8be9fd),   // Cyan
        blue: h(0x6272a4),   // Comment (the palette has no blue; its bluish grey is the nearest)
        purple: h(0xbd93f9), // Purple
    },
};

/// Ember dark (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const EMBER_DARK: Roles = Roles {
    bar_bg: h(0x1a1412),
    pill_bg: h(0x2c221d),
    popup_bg: h(0x211a16),
    raised: h(0x3b2e26),
    selection: h(0x4d3c31),
    fg: h(0xecdfd0),
    fg_dim: h(0xa8927f),
    on_accent: h(0x1a1412),
    accent: h(0xf0a04b),
    accent_container: h(0xf0a04b),
    accent2: h(0xd9694a),
    accent2_container: h(0xd9694a),
    ok: h(0xa9c26a),
    warn: h(0xf2c14e),
    error: h(0xe5604d),
    urgent: h(0xff8a3d),
    hues: Hues {
        red: h(0xe5604d),
        orange: h(0xff8a3d),
        yellow: h(0xf2c14e),
        green: h(0xa9c26a),
        cyan: h(0x6fbfa8),
        blue: h(0x7ea6c9),
        purple: h(0xc58aa8),
    },
};

/// Ember light (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const EMBER_LIGHT: Roles = Roles {
    bar_bg: h(0xefe4d3),
    pill_bg: h(0xe0d0b9),
    popup_bg: h(0xf6eddf),
    raised: h(0xd2bfa4),
    selection: h(0xc2ad90),
    fg: h(0x3a2a20),
    fg_dim: h(0x6f5a49),
    on_accent: h(0xf6eddf),
    accent: h(0xa84f12),
    accent_container: h(0xa84f12),
    accent2: h(0x8f3a22),
    accent2_container: h(0x8f3a22),
    ok: h(0x4f6b1f),
    warn: h(0x8f6200),
    error: h(0xb3261e),
    urgent: h(0xb04a08),
    hues: Hues {
        red: h(0xb3261e),
        orange: h(0xb04a08),
        yellow: h(0x8f6200),
        green: h(0x4f6b1f),
        cyan: h(0x1f6b5c),
        blue: h(0x2f5d86),
        purple: h(0x7e3f66),
    },
};

/// Glacier dark (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const GLACIER_DARK: Roles = Roles {
    bar_bg: h(0x0e1418),
    pill_bg: h(0x1a242b),
    popup_bg: h(0x121b21),
    raised: h(0x263640),
    selection: h(0x33495a),
    fg: h(0xdbe9f1),
    fg_dim: h(0x7f98a8),
    on_accent: h(0x0e1418),
    accent: h(0x7fd6f2),
    accent_container: h(0x7fd6f2),
    accent2: h(0x9bb8ff),
    accent2_container: h(0x9bb8ff),
    ok: h(0x7fe0b0),
    warn: h(0xf0d889),
    error: h(0xff8b9a),
    urgent: h(0xffae7a),
    hues: Hues {
        red: h(0xff8b9a),
        orange: h(0xffae7a),
        yellow: h(0xf0d889),
        green: h(0x7fe0b0),
        cyan: h(0x7fd6f2),
        blue: h(0x8fb0ff),
        purple: h(0xb79cf5),
    },
};

/// Glacier light (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const GLACIER_LIGHT: Roles = Roles {
    bar_bg: h(0xe4eef3),
    pill_bg: h(0xd0e0e9),
    popup_bg: h(0xeef5f8),
    raised: h(0xbcd1dd),
    selection: h(0xa8c1d0),
    fg: h(0x1d3340),
    fg_dim: h(0x4a6a79),
    on_accent: h(0xeef5f8),
    accent: h(0x00718f),
    accent_container: h(0x00718f),
    accent2: h(0x3a54b8),
    accent2_container: h(0x3a54b8),
    ok: h(0x1b7a50),
    warn: h(0x86650a),
    error: h(0xb8203c),
    urgent: h(0xb04c0c),
    hues: Hues {
        red: h(0xb8203c),
        orange: h(0xb04c0c),
        yellow: h(0x86650a),
        green: h(0x1b7a50),
        cyan: h(0x00718f),
        blue: h(0x3a54b8),
        purple: h(0x6b45b0),
    },
};

/// Ink dark (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const INK_DARK: Roles = Roles {
    bar_bg: h(0x101010),
    pill_bg: h(0x1e1e1e),
    popup_bg: h(0x161616),
    raised: h(0x2b2b2b),
    selection: h(0x3b3b3b),
    fg: h(0xe8e6e1),
    fg_dim: h(0x8f8d88),
    on_accent: h(0x101010),
    accent: h(0xff5a3c),
    accent_container: h(0xff5a3c),
    accent2: h(0xc9c6bf),
    accent2_container: h(0xc9c6bf),
    ok: h(0xb5c9a0),
    warn: h(0xe6c56b),
    error: h(0xff5a3c),
    urgent: h(0xff8a5c),
    hues: Hues {
        red: h(0xff5a3c),
        orange: h(0xe8975c),
        yellow: h(0xd9c27a),
        green: h(0xa3b895),
        cyan: h(0x8fb5b5),
        blue: h(0x8da2c4),
        purple: h(0xb49bb8),
    },
};

/// Ink light (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const INK_LIGHT: Roles = Roles {
    bar_bg: h(0xecebe6),
    pill_bg: h(0xdddbd4),
    popup_bg: h(0xf4f3ef),
    raised: h(0xcbc9c1),
    selection: h(0xb9b7ae),
    fg: h(0x1a1a1a),
    fg_dim: h(0x5c5a55),
    on_accent: h(0xf4f3ef),
    accent: h(0xc2381d),
    accent_container: h(0xc2381d),
    accent2: h(0x3d3d3d),
    accent2_container: h(0x3d3d3d),
    ok: h(0x4a6b3a),
    warn: h(0x8a6a10),
    error: h(0xc2381d),
    urgent: h(0xb5481a),
    hues: Hues {
        red: h(0xc2381d),
        orange: h(0xb0521a),
        yellow: h(0x84650e),
        green: h(0x4a6b3a),
        cyan: h(0x2f6a6a),
        blue: h(0x3d5a8a),
        purple: h(0x75507a),
    },
};

/// High contrast dark (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const HICON_DARK: Roles = Roles {
    bar_bg: h(0x000000),
    pill_bg: h(0x1c1c1c),
    popup_bg: h(0x0a0a0a),
    raised: h(0x333333),
    selection: h(0x4d4d4d),
    fg: h(0xffffff),
    fg_dim: h(0xb3b3b3),
    on_accent: h(0x000000),
    accent: h(0xffe600),
    accent_container: h(0xffe600),
    accent2: h(0x00e5ff),
    accent2_container: h(0x00e5ff),
    ok: h(0x3dff7a),
    warn: h(0xffc400),
    error: h(0xff4d4d),
    urgent: h(0xff8a00),
    hues: Hues {
        red: h(0xff4d4d),
        orange: h(0xff8a00),
        yellow: h(0xffe600),
        green: h(0x3dff7a),
        cyan: h(0x00e5ff),
        blue: h(0x5c9dff),
        purple: h(0xd58aff),
    },
};

/// High contrast light (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const HICON_LIGHT: Roles = Roles {
    bar_bg: h(0xffffff),
    pill_bg: h(0xe6e6e6),
    popup_bg: h(0xf5f5f5),
    raised: h(0xcccccc),
    selection: h(0xb3b3b3),
    fg: h(0x000000),
    fg_dim: h(0x404040),
    on_accent: h(0xffffff),
    accent: h(0x0033cc),
    accent_container: h(0x0033cc),
    accent2: h(0x7a00cc),
    accent2_container: h(0x7a00cc),
    ok: h(0x006b2b),
    warn: h(0x7a5200),
    error: h(0xb00020),
    urgent: h(0xa64000),
    hues: Hues {
        red: h(0xb00020),
        orange: h(0xa64000),
        yellow: h(0x6e5200),
        green: h(0x006b2b),
        cyan: h(0x006a73),
        blue: h(0x0033cc),
        purple: h(0x7a00cc),
    },
};

/// Sorbet dark (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const SORBET_DARK: Roles = Roles {
    bar_bg: h(0x1f1a26),
    pill_bg: h(0x2f2839),
    popup_bg: h(0x272030),
    raised: h(0x3d3449),
    selection: h(0x4e4460),
    fg: h(0xf1e9f7),
    fg_dim: h(0xa99bb8),
    on_accent: h(0x1f1a26),
    accent: h(0xffb3d1),
    accent_container: h(0xffb3d1),
    accent2: h(0xa8e6cf),
    accent2_container: h(0xa8e6cf),
    ok: h(0xb5ead7),
    warn: h(0xffe5a0),
    error: h(0xff9aa2),
    urgent: h(0xffc8a0),
    hues: Hues {
        red: h(0xff9aa2),
        orange: h(0xffc8a0),
        yellow: h(0xffe5a0),
        green: h(0xb5ead7),
        cyan: h(0xa0e7f0),
        blue: h(0xa9c4ff),
        purple: h(0xd4b3ff),
    },
};

/// Sorbet light (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const SORBET_LIGHT: Roles = Roles {
    bar_bg: h(0xf6eef6),
    pill_bg: h(0xe9dcea),
    popup_bg: h(0xfbf5fb),
    raised: h(0xdccbde),
    selection: h(0xcdb8d0),
    fg: h(0x3a2f45),
    fg_dim: h(0x6f5f7c),
    on_accent: h(0xfbf5fb),
    accent: h(0xb4306d),
    accent_container: h(0xb4306d),
    accent2: h(0x1f8a6a),
    accent2_container: h(0x1f8a6a),
    ok: h(0x1f7a55),
    warn: h(0x8a6410),
    error: h(0xc2334a),
    urgent: h(0xb05418),
    hues: Hues {
        red: h(0xc2334a),
        orange: h(0xb05418),
        yellow: h(0x8a6410),
        green: h(0x1f7a55),
        cyan: h(0x17707c),
        blue: h(0x3a5cc0),
        purple: h(0x7a45b8),
    },
};

/// Phosphor dark (UI refresh design, docs/specs/2026-11-15-alpheus-ui-refresh-design.md).
const PHOSPHOR_DARK: Roles = Roles {
    bar_bg: h(0x050a05),
    pill_bg: h(0x0f1a0f),
    popup_bg: h(0x081008),
    raised: h(0x1a2a1a),
    selection: h(0x254025),
    fg: h(0xb8f5b0),
    fg_dim: h(0x5fa060),
    on_accent: h(0x050a05),
    accent: h(0x3dff5a),
    accent_container: h(0x3dff5a),
    accent2: h(0xffb000),
    accent2_container: h(0xffb000),
    ok: h(0x3dff5a),
    warn: h(0xffb000),
    error: h(0xff5a4a),
    urgent: h(0xff8c33),
    hues: Hues {
        red: h(0xff6a55),
        orange: h(0xff9a3d),
        yellow: h(0xe8d44a),
        green: h(0x3dff5a),
        cyan: h(0x46e8b0),
        blue: h(0x5fb4ff),
        purple: h(0xb894ff),
    },
};

const CATPPUCCIN: Preset = Preset { name: "catppuccin", dark: CATPPUCCIN_DARK, light: Some(CATPPUCCIN_LIGHT) };
const GRUVBOX: Preset = Preset { name: "gruvbox", dark: GRUVBOX_DARK, light: Some(GRUVBOX_LIGHT) };
const TOKYO_NIGHT: Preset = Preset { name: "tokyo-night", dark: TOKYO_NIGHT_DARK, light: Some(TOKYO_NIGHT_LIGHT) };
const ROSE_PINE: Preset = Preset { name: "rose-pine", dark: ROSE_PINE_DARK, light: Some(ROSE_PINE_LIGHT) };
const EVERFOREST: Preset = Preset { name: "everforest", dark: EVERFOREST_DARK, light: Some(EVERFOREST_LIGHT) };
const KANAGAWA: Preset = Preset { name: "kanagawa", dark: KANAGAWA_DARK, light: Some(KANAGAWA_LIGHT) };
const SOLARIZED: Preset = Preset { name: "solarized", dark: SOLARIZED_DARK, light: Some(SOLARIZED_LIGHT) };
const NORD: Preset = Preset { name: "nord", dark: NORD_DARK, light: None };
const DRACULA: Preset = Preset { name: "dracula", dark: DRACULA_DARK, light: None };
const EMBER: Preset = Preset { name: "ember", dark: EMBER_DARK, light: Some(EMBER_LIGHT) };
const GLACIER: Preset = Preset { name: "glacier", dark: GLACIER_DARK, light: Some(GLACIER_LIGHT) };
const INK: Preset = Preset { name: "ink", dark: INK_DARK, light: Some(INK_LIGHT) };
const HICON: Preset = Preset { name: "hicon", dark: HICON_DARK, light: Some(HICON_LIGHT) };
const SORBET: Preset = Preset { name: "sorbet", dark: SORBET_DARK, light: Some(SORBET_LIGHT) };
const PHOSPHOR: Preset = Preset { name: "phosphor", dark: PHOSPHOR_DARK, light: None };

/// Every compiled-in theme, in the order the docs (and delivery 3's tab) list them.
pub static PRESETS: &[Preset] = &[
    CATPPUCCIN,
    GRUVBOX,
    TOKYO_NIGHT,
    ROSE_PINE,
    EVERFOREST,
    KANAGAWA,
    SOLARIZED,
    NORD,
    DRACULA,
    EMBER,
    GLACIER,
    INK,
    HICON,
    SORBET,
    PHOSPHOR,
];

/// The default theme (spec § 2), and every fallback's.
pub(crate) const DEFAULT_PRESET: &Preset = &CATPPUCCIN;

/// The preset called exactly `name` (case-sensitive), if any.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::contrast::ratio;

    #[test]
    fn the_fifteen_families_in_order() {
        let names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            [
                "catppuccin",
                "gruvbox",
                "tokyo-night",
                "rose-pine",
                "everforest",
                "kanagawa",
                "solarized",
                "nord",
                "dracula",
                "ember",
                "glacier",
                "ink",
                "hicon",
                "sorbet",
                "phosphor"
            ]
        );
    }

    #[test]
    fn lookup_is_by_exact_name() {
        assert_eq!(preset("tokyo-night").map(|p| p.name), Some("tokyo-night"));
        assert_eq!(preset("Catppuccin"), None);
        assert_eq!(preset("wallpaper"), None);
        assert_eq!(preset(""), None);
    }

    #[test]
    fn dark_only_presets() {
        for p in PRESETS {
            let dark_only = matches!(p.name, "nord" | "dracula" | "phosphor");
            assert_eq!(p.light.is_none(), dark_only, "{}", p.name);
        }
    }

    #[test]
    fn the_default_roles_are_the_default_preset_dark() {
        assert_eq!(DEFAULT_PRESET.name, "catppuccin");
        assert_eq!(Roles::default(), DEFAULT_PRESET.dark);
        assert_eq!(Roles::default(), CATPPUCCIN_DARK);
    }

    /// One known value per half, against the official palettes (a typo guard).
    #[test]
    fn values_come_from_the_official_palettes() {
        let half = |name: &str, light: bool| {
            let p = preset(name).unwrap();
            if light { p.light.unwrap() } else { p.dark }
        };
        assert_eq!(half("catppuccin", true).accent, Rgb(0x88, 0x39, 0xef)); // Latte Mauve
        assert_eq!(half("catppuccin", false).bar_bg, Rgb(0x18, 0x18, 0x25)); // Mocha Mantle
        assert_eq!(half("gruvbox", false).bar_bg, Rgb(0x1d, 0x20, 0x21)); // bg0_h
        assert_eq!(half("gruvbox", true).fg, Rgb(0x3c, 0x38, 0x36)); // light fg1
        assert_eq!(half("tokyo-night", false).fg, Rgb(0xc0, 0xca, 0xf5)); // Night fg
        assert_eq!(half("tokyo-night", true).accent, Rgb(0x2e, 0x7d, 0xe9)); // Day blue
        assert_eq!(half("rose-pine", false).accent, Rgb(0xc4, 0xa7, 0xe7)); // main iris
        assert_eq!(half("rose-pine", true).fg, Rgb(0x57, 0x52, 0x79)); // Dawn text
        assert_eq!(half("everforest", false).popup_bg, Rgb(0x2d, 0x35, 0x3b)); // dark medium bg0
        assert_eq!(half("everforest", true).popup_bg, Rgb(0xfd, 0xf6, 0xe3)); // light medium bg0
        assert_eq!(half("kanagawa", false).fg, Rgb(0xdc, 0xd7, 0xba)); // fujiWhite
        assert_eq!(half("kanagawa", true).fg, Rgb(0x54, 0x54, 0x64)); // lotusInk1
        assert_eq!(half("solarized", false).bar_bg, Rgb(0x00, 0x2b, 0x36)); // base03
        assert_eq!(half("solarized", true).bar_bg, Rgb(0xfd, 0xf6, 0xe3)); // base3
        assert_eq!(half("nord", false).fg, Rgb(0xd8, 0xde, 0xe9)); // nord4
        assert_eq!(half("dracula", false).accent, Rgb(0xbd, 0x93, 0xf9)); // Purple
    }

    /// One known hue per half, against the official palettes (a typo guard), and the hues the
    /// families lack (spec § 9: documented in the comments of each half).
    #[test]
    fn hues_come_from_the_official_palettes() {
        let hues = |name: &str, light: bool| {
            let p = preset(name).unwrap();
            if light { p.light.unwrap().hues } else { p.dark.hues }
        };
        assert_eq!(hues("catppuccin", false).cyan, Rgb(0x94, 0xe2, 0xd5)); // Mocha Teal
        assert_eq!(hues("catppuccin", true).purple, Rgb(0x88, 0x39, 0xef)); // Latte Mauve
        assert_eq!(hues("gruvbox", false).blue, Rgb(0x83, 0xa5, 0x98)); // bright_blue
        assert_eq!(hues("gruvbox", true).blue, Rgb(0x07, 0x66, 0x78)); // faded_blue
        assert_eq!(hues("tokyo-night", false).cyan, Rgb(0x7d, 0xcf, 0xff));
        assert_eq!(hues("tokyo-night", true).cyan, Rgb(0x00, 0x71, 0x97));
        assert_eq!(hues("rose-pine", false).green, Rgb(0x31, 0x74, 0x8f)); // pine
        assert_eq!(hues("rose-pine", true).red, Rgb(0xb4, 0x63, 0x7a)); // love
        assert_eq!(hues("everforest", false).purple, Rgb(0xd6, 0x99, 0xb6));
        assert_eq!(hues("kanagawa", false).red, Rgb(0xc3, 0x40, 0x43)); // autumnRed
        assert_eq!(hues("kanagawa", true).blue, Rgb(0x4d, 0x69, 0x9b)); // lotusBlue4
        assert_eq!(hues("solarized", false).purple, Rgb(0x6c, 0x71, 0xc4)); // violet
        assert_eq!(hues("nord", false).blue, Rgb(0x81, 0xa1, 0xc1)); // nord9
        assert_eq!(hues("dracula", false).cyan, Rgb(0x8b, 0xe9, 0xfd));
    }

    /// One role and one hue per new half, against the design tables (a typo guard).
    #[test]
    fn new_values_come_from_the_design_tables() {
        let dark = |name: &str| preset(name).unwrap().dark;
        let light = |name: &str| preset(name).unwrap().light.unwrap();
        assert_eq!(dark("ember").bar_bg, Rgb(0x1a, 0x14, 0x12));
        assert_eq!(light("ember").accent, Rgb(0xa8, 0x4f, 0x12));
        assert_eq!(dark("glacier").accent, Rgb(0x7f, 0xd6, 0xf2));
        assert_eq!(light("glacier").fg, Rgb(0x1d, 0x33, 0x40));
        assert_eq!(dark("ink").hues.red, Rgb(0xff, 0x5a, 0x3c));
        assert_eq!(light("ink").bar_bg, Rgb(0xec, 0xeb, 0xe6));
        assert_eq!(dark("hicon").accent, Rgb(0xff, 0xe6, 0x00));
        assert_eq!(light("hicon").accent, Rgb(0x00, 0x33, 0xcc));
        assert_eq!(dark("sorbet").hues.purple, Rgb(0xd4, 0xb3, 0xff));
        assert_eq!(light("sorbet").accent2, Rgb(0x1f, 0x8a, 0x6a));
        assert_eq!(dark("phosphor").fg, Rgb(0xb8, 0xf5, 0xb0));
        assert_eq!(dark("phosphor").hues.cyan, Rgb(0x46, 0xe8, 0xb0));
    }

    #[test]
    fn no_two_halves_are_the_same() {
        let halves: Vec<(String, Roles)> = PRESETS
            .iter()
            .flat_map(|p| std::iter::once((format!("{} dark", p.name), p.dark)).chain(p.light.map(|l| (format!("{} light", p.name), l))))
            .collect();
        assert_eq!(halves.len(), 27);
        for (i, (a, ra)) in halves.iter().enumerate() {
            for (b, rb) in &halves[i + 1..] {
                assert_ne!(ra, rb, "{a} == {b}");
            }
        }
    }

    #[test]
    fn a_preset_is_a_few_hundred_bytes() {
        assert_eq!(std::mem::size_of::<Roles>(), 69);
        assert!(std::mem::size_of::<Preset>() <= 160, "{}", std::mem::size_of::<Preset>());
    }

    /// Spec § 7 plus the state-colour amendment: every shipped theme is
    /// legible, in both modes (a dark-only preset is checked again as the
    /// light mode's fallback).
    #[test]
    fn every_preset_is_legible_in_both_modes() {
        /// What is checked, how to pick (text, background), the minimum ratio.
        type Pair = (&'static str, fn(&Roles) -> (Rgb, Rgb), f32);
        let pairs: [Pair; 15] = [
            ("fg on bar_bg", |r| (r.fg, r.bar_bg), 4.5),
            ("fg on popup_bg", |r| (r.fg, r.popup_bg), 4.5),
            ("on_accent on accent_container", |r| (r.on_accent, r.accent_container), 4.5),
            ("fg_dim on bar_bg", |r| (r.fg_dim, r.bar_bg), 3.0),
            ("ok on bar_bg", |r| (r.ok, r.bar_bg), 3.0),
            ("warn on bar_bg", |r| (r.warn, r.bar_bg), 3.0),
            ("error on bar_bg", |r| (r.error, r.bar_bg), 3.0),
            ("urgent on bar_bg", |r| (r.urgent, r.bar_bg), 3.0),
            ("red on bar_bg", |r| (r.hues.red, r.bar_bg), 3.0),
            ("orange on bar_bg", |r| (r.hues.orange, r.bar_bg), 3.0),
            ("yellow on bar_bg", |r| (r.hues.yellow, r.bar_bg), 3.0),
            ("green on bar_bg", |r| (r.hues.green, r.bar_bg), 3.0),
            ("cyan on bar_bg", |r| (r.hues.cyan, r.bar_bg), 3.0),
            ("blue on bar_bg", |r| (r.hues.blue, r.bar_bg), 3.0),
            ("purple on bar_bg", |r| (r.hues.purple, r.bar_bg), 3.0),
        ];
        let mut failures = Vec::new();
        let mut checked = 0;
        for p in PRESETS {
            for (mode, roles) in [("dark", p.dark), ("light", p.light.unwrap_or(p.dark))] {
                for (what, pick, min) in &pairs {
                    let (text, bg) = pick(&roles);
                    let r = ratio(text, bg);
                    checked += 1;
                    if r < *min {
                        failures.push(format!("{} {mode}: {what} = {r:.2} < {min}", p.name));
                    }
                }
            }
        }
        assert_eq!(checked, 15 * 2 * 15);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// `fg` stays legible on the two block fills (shapes delivery): `raised` (a visible workspace
    /// inside a block, powerline's alternate blocks) and `pill_bg`, at 3.5:1 in every preset half
    /// (Mae, 2026-10-08). Every ratio is printed (`--nocapture`), the worst pair first in a failure.
    #[test]
    fn fg_is_legible_on_raised_and_pill_bg() {
        const MIN: f32 = 3.5;
        let mut all = Vec::new();
        for p in PRESETS {
            for (mode, roles) in std::iter::once(("dark", p.dark)).chain(p.light.map(|l| ("light", l))) {
                for (what, bg) in [("raised", roles.raised), ("pill_bg", roles.pill_bg)] {
                    all.push((ratio(roles.fg, bg), format!("{} {mode}: fg on {what}", p.name)));
                }
            }
        }
        assert_eq!(all.len(), 27 * 2);
        for (r, what) in &all {
            println!("{what} = {r:.2}");
        }
        let mut failures: Vec<_> = all.iter().filter(|(r, _)| *r < MIN).collect();
        failures.sort_by(|a, b| a.0.total_cmp(&b.0));
        let worst = failures.first().map(|(r, what)| format!("worst: {what} = {r:.2} < {MIN}"));
        assert!(failures.is_empty(), "{worst:?}\n{failures:#?}");
    }

    /// The six presets of the UI refresh also keep secondary text and every module hue at 3:1 on
    /// the cells they sit on: `fg_dim` and the seven hues on `pill_bg`, `fg_dim` on `raised`.
    /// Every ratio is printed, the worst first in a failure.
    #[test]
    fn the_new_presets_pass_the_extra_ratios() {
        const MIN: f32 = 3.0;
        let mut all = Vec::new();
        for p in PRESETS.iter().filter(|p| matches!(p.name, "ember" | "glacier" | "ink" | "hicon" | "sorbet" | "phosphor")) {
            for (mode, r) in std::iter::once(("dark", p.dark)).chain(p.light.map(|l| ("light", l))) {
                let hues = [
                    ("red", r.hues.red),
                    ("orange", r.hues.orange),
                    ("yellow", r.hues.yellow),
                    ("green", r.hues.green),
                    ("cyan", r.hues.cyan),
                    ("blue", r.hues.blue),
                    ("purple", r.hues.purple),
                ];
                all.push((ratio(r.fg_dim, r.pill_bg), format!("{} {mode}: fg_dim on pill_bg", p.name)));
                all.push((ratio(r.fg_dim, r.raised), format!("{} {mode}: fg_dim on raised", p.name)));
                for (hue, c) in hues {
                    all.push((ratio(c, r.pill_bg), format!("{} {mode}: {hue} on pill_bg", p.name)));
                }
            }
        }
        assert_eq!(all.len(), 11 * 9);
        for (r, what) in &all {
            println!("{what} = {r:.2}");
        }
        let mut failures: Vec<_> = all.iter().filter(|(r, _)| *r < MIN).collect();
        failures.sort_by(|a, b| a.0.total_cmp(&b.0));
        let worst = failures.first().map(|(r, what)| format!("worst: {what} = {r:.2} < {MIN}"));
        assert!(failures.is_empty(), "{worst:?}\n{failures:#?}");
    }

    /// Review Focus 5: the text on an accent fill reads on every hue and on `warn` (a dark-only
    /// family is checked once).
    #[test]
    fn on_accent_is_legible_on_every_hue() {
        const MIN: f32 = 3.0;
        let mut all = Vec::new();
        for p in PRESETS {
            for (mode, r) in std::iter::once(("dark", p.dark)).chain(p.light.map(|l| ("light", l))) {
                let fills = [
                    ("red", r.hues.red),
                    ("orange", r.hues.orange),
                    ("yellow", r.hues.yellow),
                    ("green", r.hues.green),
                    ("cyan", r.hues.cyan),
                    ("blue", r.hues.blue),
                    ("purple", r.hues.purple),
                    ("warn", r.warn),
                ];
                for (what, fill) in fills {
                    all.push((ratio(r.on_accent, fill), format!("{} {mode}: on_accent on {what}", p.name)));
                }
            }
        }
        assert_eq!(all.len(), 27 * 8);
        for (r, what) in &all {
            println!("{what} = {r:.2}");
        }
        let mut failures: Vec<_> = all.iter().filter(|(r, _)| *r < MIN).collect();
        failures.sort_by(|a, b| a.0.total_cmp(&b.0));
        let worst = failures.first().map(|(r, what)| format!("worst: {what} = {r:.2} < {MIN}"));
        assert!(failures.is_empty(), "{worst:?}\n{failures:#?}");
    }
}
