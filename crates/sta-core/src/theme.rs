//! Space theme color derivation (OKLCH → sRGB) shared by the native shell (ARGB) and the HTML UI
//! (CSS strings in [`crate::ThemeColors`]). Token recipe: `docs/research/arc_spec.md` §5 color table.
//!
//! Conversion: OKLCH → OKLab (`a = C·cos h`, `b = C·sin h`) → LMS (cubed) → linear sRGB (Björn
//! Ottosson's matrices) → gamut clip by clamping each channel to [0, 1] → sRGB transfer curve →
//! 8-bit rounding. The `frame` token is the gradient mixed at 60% in OKLab, so the native frame
//! and the HTML gradient agree.

use crate::model::Theme;
use crate::view::{ThemeColors, ThemePreset};

/// Built-in presets (name, h1, h2, C) from arc_spec §5.
pub const PRESETS: &[(&str, f32, f32, f32)] = &[
    ("Dusk", 300.0, 340.0, 0.06),
    ("Lagoon", 190.0, 230.0, 0.06),
    ("Ember", 50.0, 20.0, 0.07),
    ("Moss", 130.0, 150.0, 0.05),
    ("Slate", 255.0, 255.0, 0.015),
    ("Rose", 10.0, 350.0, 0.06),
    ("Sky", 245.0, 275.0, 0.06),
    ("Sand", 80.0, 60.0, 0.05),
];

/// Maximum user chroma accepted by [`sanitize_theme`].
pub const MAX_CHROMA: f32 = 0.08;

/// Clamp a theme into its valid domain: hue wraps into [0,360) (non-finite → 0), chroma into
/// [0, 0.08] (non-finite → 0).
pub fn sanitize_theme(theme: &Theme) -> Theme {
    fn hue(h: f32) -> f32 {
        if h.is_finite() {
            let w = h.rem_euclid(360.0);
            if w >= 360.0 { 0.0 } else { w }
        } else {
            0.0
        }
    }
    let chroma = if theme.chroma.is_finite() { theme.chroma.clamp(0.0, MAX_CHROMA) } else { 0.0 };
    Theme { hue: hue(theme.hue), hue2: hue(theme.hue2), chroma }
}

/// OKLab coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

/// OKLCH (lightness 0..1, chroma, hue in degrees) → OKLab.
pub fn oklch_to_oklab(l: f64, c: f64, h_deg: f64) -> Oklab {
    let h = h_deg.to_radians();
    Oklab { l, a: c * h.cos(), b: c * h.sin() }
}

/// OKLab → linear sRGB (unclamped).
pub fn oklab_to_linear_srgb(lab: Oklab) -> [f64; 3] {
    let l_ = lab.l + 0.396_337_777_4 * lab.a + 0.215_803_757_3 * lab.b;
    let m_ = lab.l - 0.105_561_345_8 * lab.a - 0.063_854_172_8 * lab.b;
    let s_ = lab.l - 0.089_484_177_5 * lab.a - 1.291_485_548_0 * lab.b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ]
}

/// Linear-light channel → gamma-encoded sRGB channel (both 0..1).
pub fn linear_to_srgb_channel(v: f64) -> f64 {
    if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// OKLab → 8-bit sRGB with gamut clipping by per-channel clamping.
pub fn oklab_to_srgb8(lab: Oklab) -> [u8; 3] {
    let lin = oklab_to_linear_srgb(lab);
    lin.map(|v| {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        (linear_to_srgb_channel(v) * 255.0).round().clamp(0.0, 255.0) as u8
    })
}

/// OKLCH → 8-bit sRGB (clamped).
pub fn oklch_to_srgb8(l: f64, c: f64, h_deg: f64) -> [u8; 3] {
    oklab_to_srgb8(oklch_to_oklab(l, c, h_deg))
}

fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn mix(a: Oklab, b: Oklab, t: f64) -> Oklab {
    Oklab { l: a.l + (b.l - a.l) * t, a: a.a + (b.a - a.a) * t, b: a.b + (b.b - a.b) * t }
}

/// Resolve all theme colors for light or dark mode. Non-finite or out-of-range theme values are
/// clamped (hue wraps into [0,360), chroma into [0, 0.08]).
pub fn colors(theme: &Theme, dark: bool) -> ThemeColors {
    let t = sanitize_theme(theme);
    let (h1, h2) = (t.hue as f64, t.hue2 as f64);
    let user_c = t.chroma as f64;
    let (l1, l2, c) = if dark { (0.25, 0.20, user_c.min(0.06)) } else { (0.93, 0.89, user_c.min(0.07)) };
    let start = oklch_to_oklab(l1, c, h1);
    let end = oklch_to_oklab(l2, c, h2);
    let frame = mix(start, end, 0.6);
    let accent = if dark { oklch_to_oklab(0.74, 0.13, h1) } else { oklch_to_oklab(0.58, 0.15, h1) };
    if dark {
        ThemeColors {
            frame: hex(oklab_to_srgb8(frame)),
            gradient_start: hex(oklab_to_srgb8(start)),
            gradient_end: hex(oklab_to_srgb8(end)),
            accent: hex(oklab_to_srgb8(accent)),
            text: "#f3f2f7".into(),
            text_muted: "rgba(243,242,247,0.62)".into(),
            hover: "rgba(255,255,255,0.07)".into(),
            pressed: "rgba(255,255,255,0.11)".into(),
            active_row: "rgba(255,255,255,0.13)".into(),
            divider: "rgba(255,255,255,0.08)".into(),
            surface: "#232228".into(),
            border: "rgba(255,255,255,0.10)".into(),
        }
    } else {
        ThemeColors {
            frame: hex(oklab_to_srgb8(frame)),
            gradient_start: hex(oklab_to_srgb8(start)),
            gradient_end: hex(oklab_to_srgb8(end)),
            accent: hex(oklab_to_srgb8(accent)),
            text: "#1c1b20".into(),
            text_muted: "rgba(28,27,32,0.62)".into(),
            hover: "rgba(0,0,0,0.05)".into(),
            pressed: "rgba(0,0,0,0.09)".into(),
            active_row: "rgba(255,255,255,0.78)".into(),
            divider: "rgba(0,0,0,0.08)".into(),
            surface: "#ffffff".into(),
            border: "rgba(0,0,0,0.10)".into(),
        }
    }
}

/// `frame` color as opaque ARGB for CEF `set_background_color` (must equal `colors().frame`).
pub fn frame_argb(theme: &Theme, dark: bool) -> u32 {
    hex_to_argb(&colors(theme, dark).frame).unwrap_or(0xFF80_8080)
}

/// `accent` color as opaque ARGB (focused split pane ring).
pub fn accent_argb(theme: &Theme, dark: bool) -> u32 {
    hex_to_argb(&colors(theme, dark).accent).unwrap_or(0xFF80_8080)
}

/// Native chrome colors of a theme as opaque ARGB, for CEF `set_background_color` and the shell's
/// rounded-corner images (content corner masks, overlay cards). Every value equals the matching
/// [`ThemeColors`] string; translucent tokens are blended over the color they sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChromeArgb {
    /// `colors().frame`: window, content frame, pane gaps.
    pub frame: u32,
    /// `colors().accent`: the focused split pane's ring.
    pub accent: u32,
    /// `colors().surface`: overlay pages (command bar, find bar, …) and their cards.
    pub surface: u32,
    /// `colors().border` over `surface`: the 1 DIP edge of overlay cards.
    pub border: u32,
    /// `colors().border` over `frame`: the edge of the floating sidebar card.
    pub frame_border: u32,
}

/// [`ChromeArgb`] for `theme` in light or dark mode.
pub fn chrome_argb(theme: &Theme, dark: bool) -> ChromeArgb {
    let c = colors(theme, dark);
    let opaque = |css: &str| hex_to_argb(css).unwrap_or(0xFF80_8080);
    let (frame, surface) = (opaque(&c.frame), opaque(&c.surface));
    ChromeArgb {
        frame,
        accent: opaque(&c.accent),
        surface,
        border: blend_css_over(surface, &c.border).unwrap_or(surface),
        frame_border: blend_css_over(frame, &c.border).unwrap_or(frame),
    }
}

/// Parse `#rrggbb` into opaque ARGB.
pub fn hex_to_argb(hex: &str) -> Option<u32> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok().map(|rgb| 0xFF00_0000 | rgb)
}

/// Parse a CSS color as produced by [`colors`] (`#rrggbb` or `rgba(r,g,b,a)`) into `(rgb, alpha)`.
pub fn parse_css_color(css: &str) -> Option<(u32, f64)> {
    let css = css.trim();
    if let Some(argb) = hex_to_argb(css) {
        return Some((argb & 0x00FF_FFFF, 1.0));
    }
    let inner = css.strip_prefix("rgba(")?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let [r, g, b, a] = parts.as_slice() else { return None };
    let channel = |v: &str| v.parse::<u8>().ok().map(u32::from);
    let alpha = a.parse::<f64>().ok().filter(|a| (0.0..=1.0).contains(a))?;
    Some((channel(r)? << 16 | channel(g)? << 8 | channel(b)?, alpha))
}

/// `css` (possibly translucent) composited over the opaque ARGB `base`, as opaque ARGB.
pub fn blend_css_over(base: u32, css: &str) -> Option<u32> {
    let (rgb, alpha) = parse_css_color(css)?;
    let mix = |shift: u32| {
        let (b, o) = (((base >> shift) & 0xFF) as f64, ((rgb >> shift) & 0xFF) as f64);
        ((b + (o - b) * alpha).round().clamp(0.0, 255.0) as u32) << shift
    };
    Some(0xFF00_0000 | mix(16) | mix(8) | mix(0))
}

/// Built-in presets (Dusk, Lagoon, Ember, Moss, Slate, Rose, Sky, Sand) resolved for `dark`.
pub fn presets(dark: bool) -> Vec<ThemePreset> {
    PRESETS
        .iter()
        .map(|&(name, hue, hue2, chroma)| {
            let theme = Theme { hue, hue2, chroma };
            ThemePreset { name: name.into(), colors: colors(&theme, dark), theme }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (*x as i32 - *y as i32).abs() <= 1)
    }

    #[test]
    fn known_conversions() {
        assert_eq!(oklch_to_srgb8(1.0, 0.0, 0.0), [255, 255, 255]);
        assert_eq!(oklch_to_srgb8(0.0, 0.0, 0.0), [0, 0, 0]);
        // CSS Color 4: oklch(50% 0 0) = rgb(99 99 99)
        assert_eq!(oklch_to_srgb8(0.5, 0.0, 123.0), [99, 99, 99]);
        // sRGB primaries in OKLCH.
        assert!(close(oklch_to_srgb8(0.627_955, 0.257_683, 29.233_885), [255, 0, 0]));
        assert!(close(oklch_to_srgb8(0.866_440, 0.294_827, 142.495_339), [0, 255, 0]));
        assert!(close(oklch_to_srgb8(0.452_014, 0.313_214, 264.052_021), [0, 0, 255]));
        // #7b5cd6-ish purple: oklch(0.55 0.15 290)
        let p = oklch_to_srgb8(0.55, 0.15, 290.0);
        assert!(p[2] > p[0] && p[0] > p[1], "{p:?}");
    }

    #[test]
    fn gamut_clipping_clamps() {
        // Very high chroma is out of gamut; must still produce valid bytes without panicking.
        let c = oklch_to_srgb8(0.7, 0.5, 150.0);
        assert!(c.iter().any(|v| *v == 0 || *v == 255));
        let lin = oklab_to_linear_srgb(oklch_to_oklab(0.7, 0.5, 150.0));
        assert!(lin.iter().any(|v| !(0.0..=1.0).contains(v)));
    }

    #[test]
    fn frame_argb_matches_colors() {
        for dark in [false, true] {
            for p in presets(dark) {
                let c = colors(&p.theme, dark);
                assert_eq!(frame_argb(&p.theme, dark), hex_to_argb(&c.frame).unwrap());
                assert_eq!(c, p.colors);
                for v in [&c.frame, &c.accent, &c.surface, &c.gradient_start, &c.gradient_end] {
                    assert!(v.len() == 7 && v.starts_with('#'), "{v}");
                }
            }
        }
    }

    #[test]
    fn frame_is_light_or_dark() {
        let t = Theme::default();
        let light = hex_to_argb(&colors(&t, false).frame).unwrap();
        let dark = hex_to_argb(&colors(&t, true).frame).unwrap();
        let lum = |argb: u32| ((argb >> 16) & 255) + ((argb >> 8) & 255) + (argb & 255);
        assert!(lum(light) > 600, "{light:x}");
        assert!(lum(dark) < 200, "{dark:x}");
        // The frame lies between the two gradient stops in lightness.
        let c = colors(&t, false);
        let (s, e, f) = (hex_to_argb(&c.gradient_start).unwrap(), hex_to_argb(&c.gradient_end).unwrap(), light);
        assert!(lum(s) >= lum(f) && lum(f) >= lum(e));
    }

    #[test]
    fn presets_complete() {
        let names: Vec<String> = presets(false).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Dusk", "Lagoon", "Ember", "Moss", "Slate", "Rose", "Sky", "Sand"]);
        assert_eq!(presets(true)[0].theme, Theme::default());
    }

    #[test]
    fn sanitize_wraps_and_clamps() {
        let t = sanitize_theme(&Theme { hue: -30.0, hue2: 725.0, chroma: 0.5 });
        assert_eq!(t, Theme { hue: 330.0, hue2: 5.0, chroma: 0.08 });
        let t = sanitize_theme(&Theme { hue: f32::NAN, hue2: f32::INFINITY, chroma: f32::NAN });
        assert_eq!(t, Theme { hue: 0.0, hue2: 0.0, chroma: 0.0 });
        // Identical colors for equivalent themes.
        assert_eq!(colors(&Theme { hue: 660.0, hue2: -20.0, chroma: 1.0 }, true), colors(&Theme { hue: 300.0, hue2: 340.0, chroma: 0.08 }, true));
    }

    #[test]
    fn chrome_argb_matches_colors() {
        for dark in [false, true] {
            for p in presets(dark) {
                let c = colors(&p.theme, dark);
                let argb = chrome_argb(&p.theme, dark);
                assert_eq!(argb.frame, frame_argb(&p.theme, dark));
                assert_eq!(argb.accent, accent_argb(&p.theme, dark));
                assert_eq!(argb.surface, hex_to_argb(&c.surface).unwrap());
                assert_eq!(argb.border, blend_css_over(argb.surface, &c.border).unwrap());
                assert_eq!(argb.frame_border, blend_css_over(argb.frame, &c.border).unwrap());
                assert_ne!(argb.border, argb.surface, "the card edge is visible");
            }
        }
        // Known values: dark surface #232228 + 10% white, light #ffffff + 10% black.
        assert_eq!(chrome_argb(&Theme::default(), true).border, 0xFF39_383E);
        assert_eq!(chrome_argb(&Theme::default(), false).border, 0xFFE6_E6E6);
    }

    #[test]
    fn css_color_parsing_and_blending() {
        assert_eq!(parse_css_color("#123456"), Some((0x123456, 1.0)));
        assert_eq!(parse_css_color("rgba(255, 255, 255, 0.10)"), Some((0xFFFFFF, 0.1)));
        assert_eq!(parse_css_color("rgba(0,0,0,0.5)"), Some((0, 0.5)));
        for bad in ["rgba(0,0,0)", "rgba(256,0,0,1)", "rgba(0,0,0,2)", "rgb(1,2,3)", "#12345"] {
            assert_eq!(parse_css_color(bad), None, "{bad}");
        }
        assert_eq!(blend_css_over(0xFF00_0000, "rgba(255,255,255,0.5)"), Some(0xFF80_8080));
        assert_eq!(blend_css_over(0xFF12_3456, "#abcdef"), Some(0xFFAB_CDEF));
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(hex_to_argb("#123456"), Some(0xFF12_3456));
        assert_eq!(hex_to_argb("123456"), None);
        assert_eq!(hex_to_argb("#12345"), None);
        assert_eq!(hex_to_argb("#zzzzzz"), None);
    }
}
