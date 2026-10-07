//! Compiled-in themes (spec § 2): nine families, each a dark half and (but
//! for Nord and Dracula) a light half of the 16 roles, taken from the
//! official palettes. Role choices follow Catppuccin's UI style guide and
//! map the other families by analogy (plan, task 2). Where an official
//! colour failed the contrast test, the nearest passing palette colour is
//! used and the comment says so. Each half also carries the seven named hues
//! (spec § 9) from the same palette. About 1.2 KB of read-only data in all.

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
const CATPPUCCIN_LIGHT: Roles = Roles {
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
    raised: h(0xd0d5e3),            // bg_dark
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
    raised: h(0x073642),            // base02
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
    raised: h(0xeee8d5),            // base2
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

const CATPPUCCIN: Preset = Preset { name: "catppuccin", dark: CATPPUCCIN_DARK, light: Some(CATPPUCCIN_LIGHT) };
const GRUVBOX: Preset = Preset { name: "gruvbox", dark: GRUVBOX_DARK, light: Some(GRUVBOX_LIGHT) };
const TOKYO_NIGHT: Preset = Preset { name: "tokyo-night", dark: TOKYO_NIGHT_DARK, light: Some(TOKYO_NIGHT_LIGHT) };
const ROSE_PINE: Preset = Preset { name: "rose-pine", dark: ROSE_PINE_DARK, light: Some(ROSE_PINE_LIGHT) };
const EVERFOREST: Preset = Preset { name: "everforest", dark: EVERFOREST_DARK, light: Some(EVERFOREST_LIGHT) };
const KANAGAWA: Preset = Preset { name: "kanagawa", dark: KANAGAWA_DARK, light: Some(KANAGAWA_LIGHT) };
const SOLARIZED: Preset = Preset { name: "solarized", dark: SOLARIZED_DARK, light: Some(SOLARIZED_LIGHT) };
const NORD: Preset = Preset { name: "nord", dark: NORD_DARK, light: None };
const DRACULA: Preset = Preset { name: "dracula", dark: DRACULA_DARK, light: None };

/// Every compiled-in theme, in the order the docs (and delivery 3's tab) list them.
pub static PRESETS: &[Preset] = &[CATPPUCCIN, GRUVBOX, TOKYO_NIGHT, ROSE_PINE, EVERFOREST, KANAGAWA, SOLARIZED, NORD, DRACULA];

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
    fn the_nine_families_in_order() {
        let names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(names, ["catppuccin", "gruvbox", "tokyo-night", "rose-pine", "everforest", "kanagawa", "solarized", "nord", "dracula"]);
    }

    #[test]
    fn lookup_is_by_exact_name() {
        assert_eq!(preset("tokyo-night").map(|p| p.name), Some("tokyo-night"));
        assert_eq!(preset("Catppuccin"), None);
        assert_eq!(preset("wallpaper"), None);
        assert_eq!(preset(""), None);
    }

    #[test]
    fn nord_and_dracula_are_dark_only() {
        for p in PRESETS {
            let dark_only = matches!(p.name, "nord" | "dracula");
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

    #[test]
    fn no_two_halves_are_the_same() {
        let halves: Vec<(String, Roles)> = PRESETS
            .iter()
            .flat_map(|p| std::iter::once((format!("{} dark", p.name), p.dark)).chain(p.light.map(|l| (format!("{} light", p.name), l))))
            .collect();
        assert_eq!(halves.len(), 16);
        for (i, (a, ra)) in halves.iter().enumerate() {
            for (b, rb) in &halves[i + 1..] {
                assert_ne!(ra, rb, "{a} == {b}");
            }
        }
    }

    #[test]
    fn a_preset_is_a_few_hundred_bytes() {
        assert_eq!(std::mem::size_of::<Roles>(), 69);
        assert!(std::mem::size_of::<Preset>() <= 192, "{}", std::mem::size_of::<Preset>());
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
        assert_eq!(checked, 9 * 2 * 15);
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
