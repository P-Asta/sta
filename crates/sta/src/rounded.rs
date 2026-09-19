//! Rounded corners for the native chrome [owner: chrome] (ARCHITECTURE §4.4).
//!
//! CEF Views can neither clip a BrowserView nor give an overlay widget transparent corners, so
//! the rounded look is assembled from small pieces the compositor blends:
//! - **Content corner masks**: [`SLOTS`] × 4 non-activatable CUSTOM overlays (the lowest overlay
//!   level, created before the overlay hosts) sit on the corners of every visible pane (tab
//!   wrapper; the empty-state view counts as one pane). Each shows a runtime-generated image
//!   ([`Tile`]): the frame color outside the arc, the wrapper ring color (frame, the accent of the
//!   focused split pane, or the agent frame of automation/frame.rs; `tabs::visible_pane_rects`) in
//!   the 2 DIP ring, transparent inside, so the page shows through with
//!   a [`CONTENT_RADIUS`] corner. A mask extends [`MASK_BLEED`] DIP into native frame gaps (right,
//!   bottom, split gaps, the left inset while the sidebar is hidden), never over the top bar or
//!   the docked sidebar: overlay widgets and painted views snap to device pixels differently at
//!   fractional scales. No masks in page fullscreen (square presentation) or for a tab in Peek.
//!   Showing a mask raises it above every overlay, so [`layout_masks`] restacks the visible
//!   overlays afterwards (`overlays::restack_all_visible`).
//! - **Peek page masks**: 4 more overlays right above the Peek host round the peeked page inside
//!   its card ([`PEEK_PAGE_RADIUS`], surface outside, reaching 4 DIP into the card's fill). They
//!   are shown right after Peek itself (overlays.rs restacks the overlays above Peek afterwards).
//! - **Overlay cards** ([`Card`]): an overlay host's contents are a transparent box of corner
//!   images (fill inside, 1 DIP border arc, soft shadow outside), 1 DIP border and shadow strips
//!   (translucent panels) and, in the middle, a border-colored host with a fill-colored inner panel
//!   that parents the BrowserView(s). `Rows` puts the corners in top and bottom rows (tall
//!   surfaces), `Columns` in left and right columns (short bars: the height isn't padded).
//!
//! Pixel rules (measured in the spike, docs/ARCHITECTURE §4.4): box rows and columns need an
//! explicit preferred size; every image is a multiple of 4 DIP (exact representations at 125, 150
//! and 175 %); every chrome thickness is a multiple of 4 DIP and an overlay host's rect is snapped
//! to the device-pixel grid ([`snap_unit`]); fill panels re-apply their color in
//! `on_theme_changed` (CEF resets backgrounds there).
//!
//! Known limits: an overlay swallows mouse input in its whole rectangle (no pass-through in CEF):
//! about 10×10 DIP at each page corner (plus the bleed over the frame) and a card's shadow ring.
//!
//! Public API:
//! - tokens: `CONTENT_RADIUS`, `CONTENT_RING`, `MASK_BLEED`, `OVERLAY_RADIUS`, `OVERLAY_SHADOW`,
//!   `OVERLAY_PAD`, `FIND_RADIUS`, `TOAST_RADIUS`, `SIDEBAR_SHADOW`, `PEEK_PAD`
//! - `pub struct ChromeColors`, `pub fn set_colors(colors: ChromeColors)`, `pub fn colors() -> ChromeColors`
//! - masks: `pub fn create_masks(window: &Window)`, `pub fn layout_masks()`, `pub fn clear()`
//! - Peek page masks: `pub fn create_peek_masks(window: &Window)`, `pub fn layout_peek_masks(page, allow_show) -> bool`,
//!   `pub fn reshow_peek_masks()`
//! - cards: `pub enum Orientation`, `pub enum Palette`, `pub struct CardSpec`, `pub fn card_root(spec) -> Option<Panel>`,
//!   `pub fn build_card(root: &Panel, spec) -> Option<Card>`, `Card::recolor()`
//! - geometry: `pub fn snap_unit(scale: f32) -> i32`, `pub fn window_snap_unit() -> i32`,
//!   `pub fn snap_rect(r, unit, grow) -> Rect`, `pub fn host_rect(card, spec, unit, grow) -> Rect`,
//!   `pub fn inset(r, dx, dy) -> Rect`
//! - pixels (unit-tested): `pub struct Tile`, `pub enum Corner`, `pub fn tile_pixels(tile, scale, corner)`
//! - `pub fn debug_snapshot() -> serde_json::Value`

use crate::{overlays, tabs, window};
use cef::*;
use std::cell::{Cell, RefCell};

// ----------------------------------------------------------------------------------- tokens

/// Content card radius (DIP), measured at the web page edge (inside the wrapper ring). Mirrored as
/// `--content-radius` in ui/common/tokens.css.
pub const CONTENT_RADIUS: i32 = 10;
/// Wrapper ring width (tabs.rs `WRAPPER_BORDER`): the focused split pane's accent ring.
pub const CONTENT_RING: i32 = 2;
/// How far a content mask reaches into a native frame gap (DIP).
pub const MASK_BLEED: i32 = 4;
/// Command bar, permission prompt, agent overlay, switcher, Peek and floating sidebar radius
/// (`--overlay-radius`).
pub const OVERLAY_RADIUS: i32 = 12;
/// Soft shadow around overlay cards (`--overlay-shadow`).
pub const OVERLAY_SHADOW: i32 = 8;
/// Fill between a card's side border and its page (with the border: 4 DIP).
pub const OVERLAY_PAD: i32 = 3;
/// Find bar radius (`--find-radius`).
pub const FIND_RADIUS: i32 = 8;
/// Toast radius (`--toast-radius`).
pub const TOAST_RADIUS: i32 = 16;
/// The floating sidebar's shadow: it stays clear of the window's 4 DIP resize bands.
pub const SIDEBAR_SHADOW: i32 = 4;
/// Peek's side fill: with the border, 12 DIP beside the page, like above and below it.
pub const PEEK_PAD: i32 = 11;
/// Radius of the web page inside the Peek card (`--peek-page-radius`).
pub const PEEK_PAGE_RADIUS: i32 = 8;

/// Pane slots with corner masks (split view has at most 4 panes).
const SLOTS: usize = 4;
/// Scale factors an image carries a representation for (only where the pixel size is whole).
const SCALES: [f32; 9] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0, 3.5];
/// Peak shadow alpha at a card's edge, light and dark.
const SHADOW_PEAK_LIGHT: f32 = 0.16;
const SHADOW_PEAK_DARK: f32 = 0.34;
const IMAGE_CACHE_MAX: usize = 96;

// ----------------------------------------------------------------------------------- colors

/// The native chrome colors (opaque ARGB) of the active space (`Effect::SetChrome`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChromeColors {
    pub frame: u32,
    pub accent: u32,
    pub surface: u32,
    /// Border over surface.
    pub border: u32,
    /// Border over frame.
    pub frame_border: u32,
    pub dark: bool,
}

impl ChromeColors {
    const DEFAULT: ChromeColors = ChromeColors {
        frame: 0xFF26_222E,
        accent: 0xFF7B_5CD6,
        surface: 0xFFFF_FFFF,
        border: 0xFFE6_E6E6,
        frame_border: 0xFFD8_D2DC,
        dark: false,
    };

    fn shadow_peak(&self) -> f32 {
        if self.dark { SHADOW_PEAK_DARK } else { SHADOW_PEAK_LIGHT }
    }
}

thread_local! {
    static COLORS: Cell<ChromeColors> = const { Cell::new(ChromeColors::DEFAULT) };
}

/// `SetChrome`: remembers the colors; window.rs then recolors the masks (via tabs) and cards.
pub fn set_colors(colors: ChromeColors) {
    COLORS.set(colors);
}

/// The colors of the last `SetChrome`.
pub fn colors() -> ChromeColors {
    COLORS.get()
}

// ----------------------------------------------------------------------------------- pixels

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft, Corner::BottomRight];
}

/// One corner image, described in the top-left orientation in DIP: `w`×`h`, arcs centered at the
/// bottom-right corner (`w`, `h`). From the center outwards: `inside` for d ≤ `r_in`, `ring` for
/// d ≤ `r_out`, then `outside`, or (when `shadow` > 0) a black falloff over `shadow` DIP that
/// starts at `shadow_peak` alpha. Colors are ARGB (not premultiplied).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tile {
    pub w: i32,
    pub h: i32,
    pub r_in: f32,
    pub r_out: f32,
    pub inside: u32,
    pub ring: u32,
    pub outside: u32,
    pub shadow: i32,
    pub shadow_peak: f32,
}

/// Premultiplied BGRA of an ARGB color scaled by `coverage` (0..1).
fn premul(color: u32, coverage: f32) -> [f32; 4] {
    let a = ((color >> 24) & 0xFF) as f32 / 255.0 * coverage;
    let r = ((color >> 16) & 0xFF) as f32 / 255.0;
    let g = ((color >> 8) & 0xFF) as f32 / 255.0;
    let b = (color & 0xFF) as f32 / 255.0;
    [b * a, g * a, r * a, a]
}

/// Shadow alpha `d` DIP outside a card edge: quadratic falloff from `peak` to 0 at `width`.
fn shadow_alpha(d: f32, width: f32, peak: f32) -> f32 {
    if width <= 0.0 || d >= width {
        return 0.0;
    }
    let t = 1.0 - (d / width).clamp(0.0, 1.0);
    peak * t * t
}

/// Alpha (0..255) of shadow strip `i` (0 = outermost) of a `width` DIP shadow: the strip covers
/// [width-1-i, width-i) DIP outside the edge; averaged like the corner images' supersampling.
fn strip_alpha(i: i32, width: i32, peak: f32) -> u32 {
    const N: i32 = 4;
    let start = (width - 1 - i) as f32;
    let sum: f32 = (0..N).map(|s| shadow_alpha(start + (s as f32 + 0.5) / N as f32, width as f32, peak)).sum();
    (sum / N as f32 * 255.0).round().clamp(0.0, 255.0) as u32
}

/// Premultiplied BGRA pixels of `tile` at `scale` for `corner` (4×4 supersampling per pixel).
/// Returns `(pixel width, pixel height, bytes)`.
pub fn tile_pixels(tile: &Tile, scale: f32, corner: Corner) -> (i32, i32, Vec<u8>) {
    const N: i32 = 4;
    let pw = (tile.w as f32 * scale).round() as i32;
    let ph = (tile.h as f32 * scale).round() as i32;
    let mut out = vec![0u8; (pw.max(0) * ph.max(0) * 4) as usize];
    let (cx, cy) = (tile.w as f32, tile.h as f32);
    for y in 0..ph {
        for x in 0..pw {
            let mut acc = [0f32; 4];
            for sy in 0..N {
                for sx in 0..N {
                    let fx = (x as f32 + (sx as f32 + 0.5) / N as f32) / scale;
                    let fy = (y as f32 + (sy as f32 + 0.5) / N as f32) / scale;
                    let d = ((cx - fx).max(0.0).powi(2) + (cy - fy).max(0.0).powi(2)).sqrt();
                    let p = if d <= tile.r_in {
                        premul(tile.inside, 1.0)
                    } else if d <= tile.r_out {
                        premul(tile.ring, 1.0)
                    } else if tile.shadow > 0 {
                        premul(0xFF00_0000, shadow_alpha(d - tile.r_out, tile.shadow as f32, tile.shadow_peak))
                    } else {
                        premul(tile.outside, 1.0)
                    };
                    for (a, v) in acc.iter_mut().zip(p) {
                        *a += v;
                    }
                }
            }
            let (ox, oy) = match corner {
                Corner::TopLeft => (x, y),
                Corner::TopRight => (pw - 1 - x, y),
                Corner::BottomLeft => (x, ph - 1 - y),
                Corner::BottomRight => (pw - 1 - x, ph - 1 - y),
            };
            let i = ((oy * pw + ox) * 4) as usize;
            for (k, a) in acc.iter().enumerate() {
                out[i + k] = (a / (N * N) as f32 * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    (pw, ph, out)
}

/// The scale factors `tile` gets an exact bitmap for.
fn exact_scales(tile: &Tile) -> impl Iterator<Item = f32> + '_ {
    SCALES.into_iter().filter(|s| [tile.w, tile.h].iter().all(|v| ((*v as f32 * s) - (*v as f32 * s).round()).abs() < 0.01))
}

#[derive(Default, Debug, Clone, Copy)]
#[cfg_attr(not(debug_assertions), allow(dead_code))] // read by debug_snapshot only
struct Stats {
    images_built: u64,
    image_build_us: u64,
    cache_hits: u64,
    mask_layouts: u64,
    mask_shows: u64,
    mask_hides: u64,
    restacks: u64,
    cards_built: u64,
    card_build_us: u64,
}

thread_local! {
    static IMAGE_CACHE: RefCell<Vec<(Tile, Corner, Image)>> = const { RefCell::new(Vec::new()) };
    static STATS: Cell<Stats> = Cell::new(Stats::default());
}

fn stat(f: impl FnOnce(&mut Stats)) {
    let mut s = STATS.get();
    f(&mut s);
    STATS.set(s);
}

/// A CEF image of `tile` with a representation per exact scale factor (cached).
fn tile_image(tile: &Tile, corner: Corner) -> Option<Image> {
    let cached = IMAGE_CACHE.with(|c| c.borrow().iter().find(|(t, k, _)| t == tile && *k == corner).map(|(_, _, i)| i.clone()));
    if let Some(image) = cached {
        stat(|s| s.cache_hits += 1);
        return Some(image);
    }
    let started = std::time::Instant::now();
    let image = image_create()?;
    for scale in exact_scales(tile) {
        let (pw, ph, pixels) = tile_pixels(tile, scale, corner);
        if image.add_bitmap(scale, pw, ph, ColorType::BGRA_8888, AlphaType::PREMULTIPLIED, Some(&pixels)) == 0 {
            log_warn!("corner image: add_bitmap({scale}, {pw}x{ph}) failed");
        }
    }
    let us = started.elapsed().as_micros() as u64;
    stat(|s| {
        s.images_built += 1;
        s.image_build_us += us;
    });
    IMAGE_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= IMAGE_CACHE_MAX {
            c.remove(0);
        }
        c.push((*tile, corner, image.clone()));
    });
    Some(image)
}

wrap_button_delegate! {
    struct InertButtonDelegate {}

    impl ViewDelegate {}

    impl ButtonDelegate {}
}

/// A non-focusable LabelButton that only shows `image` at its origin (no insets, no ink drop;
/// hovered and pressed states fall back to the normal image, so nothing changes on hover).
fn image_view(image: &mut Image, w: i32, h: i32) -> Option<LabelButton> {
    let mut delegate = InertButtonDelegate::new();
    let button = label_button_create(Some(&mut delegate), None)?;
    button.set_insets(Some(&Insets { top: 0, left: 0, bottom: 0, right: 0 }));
    button.set_focusable(0);
    button.set_ink_drop_enabled(0);
    button.set_image(ButtonState::NORMAL, Some(image));
    set_fixed_size(&button, w, h);
    Some(button)
}

fn set_fixed_size(button: &LabelButton, w: i32, h: i32) {
    button.set_minimum_size(Some(&Size { width: w, height: h }));
    button.set_maximum_size(Some(&Size { width: w, height: h }));
}

// ----------------------------------------------------------------------------------- geometry

/// The multiple of DIP that lands on whole device pixels at `scale`: 1 at 100/200/300 %, 2 at
/// 150/250/350 %, else 4 (125, 175, 225 %; approximate at other scales).
pub fn snap_unit(scale: f32) -> i32 {
    let whole = |k: f32| ((scale * k) - (scale * k).round()).abs() < 0.01;
    if whole(1.0) {
        1
    } else if whole(2.0) {
        2
    } else {
        4
    }
}

/// [`snap_unit`] of the main window's display.
pub fn window_snap_unit() -> i32 {
    window::main_window().and_then(|w| w.display()).map(|d| snap_unit(d.device_scale_factor())).unwrap_or(4)
}

/// `r` with every edge moved to a multiple of `unit`: outwards when `grow`, else inwards.
pub fn snap_rect(r: &Rect, unit: i32, grow: bool) -> Rect {
    let unit = unit.max(1);
    let floor = |v: i32| v.div_euclid(unit) * unit;
    let ceil = |v: i32| (v + unit - 1).div_euclid(unit) * unit;
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.width, r.y + r.height);
    let (x0, y0, x1, y1) = if grow { (floor(x0), floor(y0), ceil(x1), ceil(y1)) } else { (ceil(x0), ceil(y0), floor(x1), floor(y1)) };
    Rect { x: x0, y: y0, width: (x1 - x0).max(0), height: (y1 - y0).max(0) }
}

/// `r` shrunk by `dx` on the left and right and `dy` on the top and bottom (negative grows).
pub fn inset(r: &Rect, dx: i32, dy: i32) -> Rect {
    Rect { x: r.x + dx, y: r.y + dy, width: (r.width - 2 * dx).max(0), height: (r.height - 2 * dy).max(0) }
}

// ----------------------------------------------------------------------------------- content masks

struct Mask {
    slot: usize,
    corner: Corner,
    button: LabelButton,
    controller: OverlayController,
    /// The tile its image shows.
    tile: Tile,
}

thread_local! {
    static MASKS: RefCell<Vec<Mask>> = const { RefCell::new(Vec::new()) };
}

fn mask_tile(outside: u32, ring: u32, bleed_x: i32, bleed_y: i32) -> Tile {
    let size = CONTENT_RADIUS + CONTENT_RING;
    Tile {
        w: size + bleed_x,
        h: size + bleed_y,
        r_in: CONTENT_RADIUS as f32,
        r_out: size as f32,
        inside: 0,
        ring,
        outside,
        shadow: 0,
        shadow_peak: 0.0,
    }
}

/// Where the `corner` mask of a pane goes and what it shows. `pane`: the wrapper rect (window DIP);
/// `ring`: its wrapper color as shown (frame, the focused split pane's accent, or the agent frame
/// of automation/frame.rs); `content`: the content panel rect; `left_inset_is_frame`: the sidebar
/// is hidden, so the content frame's left inset is native frame. `None` when the pane is too small
/// for its corners.
fn mask_placement(pane: &Rect, ring: u32, content: &Rect, left_inset_is_frame: bool, corner: Corner, colors: ChromeColors) -> Option<(Rect, Tile)> {
    let size = CONTENT_RADIUS + CONTENT_RING;
    if pane.width < 2 * size || pane.height < 2 * size {
        return None;
    }
    // Bleed only into native frame: right and bottom insets, split gaps, the left inset while the
    // sidebar is hidden. Never over the top bar or the docked sidebar (HTML, not frame).
    let left_frame = pane.x > content.x || left_inset_is_frame;
    let top_frame = pane.y > content.y;
    let bleed = |on: bool| if on { MASK_BLEED } else { 0 };
    let (bx, by) = match corner {
        Corner::TopLeft => (bleed(left_frame), bleed(top_frame)),
        Corner::TopRight => (MASK_BLEED, bleed(top_frame)),
        Corner::BottomLeft => (bleed(left_frame), MASK_BLEED),
        Corner::BottomRight => (MASK_BLEED, MASK_BLEED),
    };
    let tile = mask_tile(colors.frame, ring, bx, by);
    let (x, y) = match corner {
        Corner::TopLeft => (pane.x - bx, pane.y - by),
        Corner::TopRight => (pane.x + pane.width - size, pane.y - by),
        Corner::BottomLeft => (pane.x - bx, pane.y + pane.height - size),
        Corner::BottomRight => (pane.x + pane.width - size, pane.y + pane.height - size),
    };
    Some((Rect { x, y, width: tile.w, height: tile.h }, tile))
}

/// One hidden, non-activatable mask overlay showing `tile`.
fn create_mask(window: &Window, slot: usize, corner: Corner, tile: Tile) -> Option<Mask> {
    let mut image = tile_image(&tile, corner)?;
    let button = image_view(&mut image, tile.w, tile.h)?;
    let Some(controller) = window.add_overlay_view(Some(&mut View::from(&button)), DockingMode::CUSTOM, 0) else {
        log_error!("corner mask overlay failed");
        return None;
    };
    controller.set_visible(0);
    Some(Mask { slot, corner, button, controller, tile })
}

/// Creates the corner mask overlays, hidden. Call before `overlays::create_hosts`: overlays
/// created later stack above them.
pub fn create_masks(window: &Window) {
    let colors = COLORS.get();
    let masks: Vec<Mask> = (0..SLOTS)
        .flat_map(|slot| Corner::ALL.map(|corner| (slot, corner)))
        .filter_map(|(slot, corner)| create_mask(window, slot, corner, mask_tile(colors.frame, colors.frame, 0, 0)))
        .collect();
    MASKS.with(|m| *m.borrow_mut() = masks);
}

/// Positions, recolors, shows and hides the masks for the panes on screen (content layout, window
/// layout, sidebar and color changes). Restacks the overlays when a mask was shown.
pub fn layout_masks() {
    if window::is_closing() {
        return;
    }
    #[allow(clippy::type_complexity)]
    let masks: Vec<(usize, Corner, LabelButton, OverlayController, Tile)> =
        MASKS.with(|m| m.borrow().iter().map(|k| (k.slot, k.corner, k.button.clone(), k.controller.clone(), k.tile)).collect());
    if masks.is_empty() {
        return;
    }
    let panes = tabs::visible_pane_rects();
    let content = window::content_rect();
    let left_inset_is_frame = window::sidebar_parked();
    let colors = COLORS.get();
    let mut shown = false;
    let mut retiled: Vec<(usize, Corner, Tile)> = Vec::new();
    for (slot, corner, button, controller, current) in masks {
        let placement = panes.get(slot).and_then(|(pane, ring)| {
            mask_placement(pane, *ring, content.as_ref().unwrap_or(pane), left_inset_is_frame, corner, colors)
        });
        let Some((rect, tile)) = placement else {
            if controller.is_visible() != 0 {
                controller.set_visible(0);
                stat(|s| s.mask_hides += 1);
            }
            continue;
        };
        if tile != current
            && let Some(mut image) = tile_image(&tile, corner)
        {
            button.set_image(ButtonState::NORMAL, Some(&mut image));
            set_fixed_size(&button, tile.w, tile.h);
            retiled.push((slot, corner, tile));
        }
        let b = controller.bounds();
        if (b.x, b.y, b.width, b.height) != (rect.x, rect.y, rect.width, rect.height) {
            controller.set_bounds(Some(&rect));
        }
        if controller.is_visible() == 0 {
            controller.set_visible(1);
            shown = true;
            stat(|s| s.mask_shows += 1);
        }
    }
    if !retiled.is_empty() {
        MASKS.with(|m| {
            for mask in m.borrow_mut().iter_mut() {
                if let Some((_, _, tile)) = retiled.iter().find(|(s, c, _)| *s == mask.slot && *c == mask.corner) {
                    mask.tile = *tile;
                }
            }
        });
    }
    stat(|s| s.mask_layouts += 1);
    if shown {
        // Showing a widget raises it above every other overlay.
        stat(|s| s.restacks += 1);
        overlays::restack_all_visible();
    }
}

// ----------------------------------------------------------------------------------- Peek page masks

thread_local! {
    static PEEK_MASKS: RefCell<Vec<Mask>> = const { RefCell::new(Vec::new()) };
}

/// The Peek page's `corner` mask: surface outside a [`PEEK_PAGE_RADIUS`] arc, reaching
/// [`MASK_BLEED`] DIP into the card's fill around the page.
fn peek_mask_placement(page: &Rect, corner: Corner, surface: u32) -> Option<(Rect, Tile)> {
    let (r, b) = (PEEK_PAGE_RADIUS, MASK_BLEED);
    if page.width < 2 * r || page.height < 2 * r {
        return None;
    }
    let tile = Tile { w: r + b, h: r + b, r_in: r as f32, r_out: r as f32, inside: 0, ring: 0, outside: surface, shadow: 0, shadow_peak: 0.0 };
    let (x, y) = match corner {
        Corner::TopLeft => (page.x - b, page.y - b),
        Corner::TopRight => (page.x + page.width - r, page.y - b),
        Corner::BottomLeft => (page.x - b, page.y + page.height - r),
        Corner::BottomRight => (page.x + page.width - r, page.y + page.height - r),
    };
    Some((Rect { x, y, width: tile.w, height: tile.h }, tile))
}

/// Creates the Peek page masks, hidden. Call right after creating the Peek host: they stack above
/// it and below every other overlay host.
pub fn create_peek_masks(window: &Window) {
    let surface = COLORS.get().surface;
    let page = Rect { x: 0, y: 0, width: 100, height: 100 };
    let masks: Vec<Mask> = Corner::ALL
        .into_iter()
        .filter_map(|corner| {
            let (_, tile) = peek_mask_placement(&page, corner, surface)?;
            create_mask(window, 0, corner, tile)
        })
        .collect();
    PEEK_MASKS.with(|m| *m.borrow_mut() = masks);
}

/// Places the Peek page masks on `page` (window coordinates; `None` hides them) and recolors them.
/// A hidden mask is only shown when `allow_show` (right after Peek itself was shown). Returns
/// whether one was shown: it rose above every overlay, so the caller restacks the ones above Peek.
pub fn layout_peek_masks(page: Option<&Rect>, allow_show: bool) -> bool {
    let masks: Vec<(Corner, LabelButton, OverlayController, Tile)> =
        PEEK_MASKS.with(|m| m.borrow().iter().map(|k| (k.corner, k.button.clone(), k.controller.clone(), k.tile)).collect());
    let surface = COLORS.get().surface;
    let mut shown = false;
    let mut retiled: Vec<(Corner, Tile)> = Vec::new();
    for (corner, button, controller, current) in masks {
        let Some((rect, tile)) = page.and_then(|p| peek_mask_placement(p, corner, surface)) else {
            if controller.is_visible() != 0 {
                controller.set_visible(0);
            }
            continue;
        };
        if tile != current
            && let Some(mut image) = tile_image(&tile, corner)
        {
            button.set_image(ButtonState::NORMAL, Some(&mut image));
            retiled.push((corner, tile));
        }
        let b = controller.bounds();
        if (b.x, b.y, b.width, b.height) != (rect.x, rect.y, rect.width, rect.height) {
            controller.set_bounds(Some(&rect));
        }
        if controller.is_visible() == 0 && allow_show {
            controller.set_visible(1);
            shown = true;
        }
    }
    PEEK_MASKS.with(|m| {
        for mask in m.borrow_mut().iter_mut() {
            if let Some((_, tile)) = retiled.iter().find(|(c, _)| *c == mask.corner) {
                mask.tile = *tile;
            }
        }
    });
    shown
}

/// Re-shows the visible Peek page masks right after Peek was re-shown (a restack).
pub fn reshow_peek_masks() {
    let controllers: Vec<OverlayController> = PEEK_MASKS.with(|m| m.borrow().iter().map(|k| k.controller.clone()).collect());
    for controller in controllers.into_iter().filter(|c| c.is_visible() != 0) {
        controller.set_visible(0);
        controller.set_visible(1);
    }
}

/// Drops the masks and cached images (window destroyed).
pub fn clear() {
    let masks = MASKS.with(|m| std::mem::take(&mut *m.borrow_mut()));
    drop(masks);
    let peek = PEEK_MASKS.with(|m| std::mem::take(&mut *m.borrow_mut()));
    drop(peek);
    let images = IMAGE_CACHE.with(|c| std::mem::take(&mut *c.borrow_mut()));
    drop(images);
}

// ----------------------------------------------------------------------------------- cards

/// Where a card's corner images go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    /// Top and bottom rows (the page is padded by the radius vertically).
    Rows,
    /// Left and right columns (the page is padded by the radius horizontally): short bars.
    Columns,
}

/// A card's fill and border colors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Palette {
    /// Overlay pages: surface with border over surface.
    Surface,
    /// The floating sidebar: frame with border over frame.
    Frame,
}

impl Palette {
    fn colors(self, c: ChromeColors) -> (u32, u32) {
        match self {
            Palette::Surface => (c.surface, c.border),
            Palette::Frame => (c.frame, c.frame_border),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CardSpec {
    pub radius: i32,
    pub shadow: i32,
    /// Fill between the side border (Rows) or the top/bottom border (Columns) and the page.
    pub pad: i32,
    pub orientation: Orientation,
    /// The inner panel stacks its views vertically (Peek: header over page).
    pub vertical_content: bool,
    pub palette: Palette,
}

impl CardSpec {
    /// Host rect → page rect: DIP per side, `(horizontal, vertical)`.
    pub fn chrome(&self) -> (i32, i32) {
        let edge = self.shadow + 1 + self.pad;
        let arc = self.shadow + self.radius;
        match self.orientation {
            Orientation::Rows => (edge, arc),
            Orientation::Columns => (arc, edge),
        }
    }

    /// Card rect (outer border edge) → page rect: DIP per side, `(horizontal, vertical)`.
    pub fn inner(&self) -> (i32, i32) {
        let (h, v) = self.chrome();
        (h - self.shadow, v - self.shadow)
    }

    fn arc(&self) -> i32 {
        self.shadow + self.radius
    }

    fn tile(&self, colors: ChromeColors) -> Tile {
        let (fill, border) = self.palette.colors(colors);
        Tile {
            w: self.arc(),
            h: self.arc(),
            r_in: self.radius as f32 - 1.0,
            r_out: self.radius as f32,
            inside: fill,
            ring: border,
            outside: 0,
            shadow: self.shadow,
            shadow_peak: colors.shadow_peak(),
        }
    }
}

/// The overlay host rect (card plus shadow) for a card rect, snapped to `unit` (grown, or shrunk
/// when the host must stay inside a limit such as the window's resize bands).
pub fn host_rect(card: &Rect, spec: &CardSpec, unit: i32, grow: bool) -> Rect {
    snap_rect(&inset(card, -spec.shadow, -spec.shadow), unit, grow)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fill {
    /// The card color (surface or frame).
    Interior,
    Border,
    /// Shadow strip `i` (0 = outermost).
    Shadow(i32),
}

fn fill_color(fill: Fill, spec: &CardSpec, colors: ChromeColors) -> u32 {
    let (fill_argb, border) = spec.palette.colors(colors);
    match fill {
        Fill::Interior => fill_argb,
        Fill::Border => border,
        Fill::Shadow(i) => strip_alpha(i, spec.shadow, colors.shadow_peak()) << 24,
    }
}

wrap_panel_delegate! {
    struct FillPanelDelegate {
        fill: Fill,
        spec: CardSpec,
        size: Size,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            self.size.clone()
        }

        fn on_theme_changed(&self, view: Option<&mut View>) {
            // CEF resets background colors whenever the theme is (re)applied.
            if let Some(view) = view {
                view.set_background_color(fill_color(self.fill, &self.spec, COLORS.get()));
            }
        }
    }

    impl PanelDelegate {}
}

// No background: transparent inside an overlay widget.
wrap_panel_delegate! {
    struct ClearPanelDelegate {
        size: Option<Size>,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // Rows and columns need an explicit size: with a derived one the root box gave the
            // first row all the height and never laid out the rest (measured in the spike).
            self.size.clone().unwrap_or(Size { width: 1, height: 1 })
        }
    }

    impl PanelDelegate {}
}

fn fill_panel(fill: Fill, spec: &CardSpec, width: i32, height: i32) -> Option<Panel> {
    let mut delegate = FillPanelDelegate::new(fill, *spec, Size { width, height });
    let panel = panel_create(Some(&mut delegate))?;
    panel.set_background_color(fill_color(fill, spec, COLORS.get()));
    Some(panel)
}

fn clear_box(horizontal: bool, size: Option<Size>, insets: Insets) -> Option<(Panel, BoxLayout)> {
    let mut delegate = ClearPanelDelegate::new(size);
    let panel = panel_create(Some(&mut delegate))?;
    let layout = panel.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: horizontal as i32,
        inside_border_insets: insets,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    }))?;
    Some((panel, layout))
}

fn no_insets() -> Insets {
    Insets { top: 0, left: 0, bottom: 0, right: 0 }
}

/// The pieces of a built card.
pub struct Card {
    pub spec: CardSpec,
    /// Parent of the card's BrowserView(s) (flex 1; Peek: the header first).
    pub inner: Panel,
    corners: Vec<(Corner, LabelButton)>,
    fills: Vec<(Fill, Panel)>,
}

/// The transparent overlay contents panel a card is built into (created with the overlay host, so
/// the host exists in the z-order before the card's views do).
pub fn card_root(spec: &CardSpec) -> Option<Panel> {
    clear_box(spec.orientation == Orientation::Columns, None, no_insets()).map(|(panel, _)| panel)
}

/// A transparent panel with `root` as its only child, which it can show a **slice** of: `root`
/// keeps its full width and is placed `cut` DIP left of the panel ([`set_clip_cut`]), where Views
/// clips it to the panel. That is how the floating sidebar slides in from outside the window without
/// its page ever being resized (`overlays::layout_overlay`).
pub fn clip_root(root: &Panel) -> Option<Panel> {
    let mut delegate = ClearPanelDelegate::new(None);
    let clip = panel_create(Some(&mut delegate))?;
    clip.add_child_view(Some(&mut View::from(root)));
    set_clip_cut(&clip, root, 0);
    Some(clip)
}

/// Lays `root` out `cut` DIP wider than `clip` and that far to its left (0 = it fills the panel).
///
/// A box layout with a **negative left inset**: the layout's child area then starts `cut` DIP left
/// of the panel and is that much wider, and the one flexed child gets all of it. Setting the child's
/// bounds by hand does not survive — a panel without a layout manager fills itself with its child on
/// the next layout pass (measured: the card came back as `[0, 0, clip width, h]` every step).
///
/// Only *replaces the layout manager*, which invalidates the panel's layout; the pass itself runs
/// when the panel is resized (or on the caller's `layout()`).
pub fn set_clip_cut(clip: &Panel, root: &Panel, cut: i32) {
    let layout = clip.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: 1,
        inside_border_insets: Insets { top: 0, left: -cut.max(0), bottom: 0, right: 0 },
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    }));
    if let Some(layout) = layout {
        layout.set_flex_for_view(Some(&mut View::from(root)), 1);
    }
    // No `layout()` here: the caller resizes the panel next, and the cut and the width only make
    // sense together. Laying out in between gave the card one pass at the old width with the new
    // cut — a page resize per step, which is exactly what the clip exists to avoid.
}

/// Builds the card's views into `root` (from [`card_root`]).
pub fn build_card(root: &Panel, spec: CardSpec) -> Option<Card> {
    let started = std::time::Instant::now();
    let card = build_card_inner(root, spec);
    let us = started.elapsed().as_micros() as u64;
    stat(|s| {
        s.cards_built += 1;
        s.card_build_us += us;
    });
    if card.is_none() {
        log_error!("rounded card {spec:?} could not be built");
    }
    card
}

fn build_card_inner(root: &Panel, spec: CardSpec) -> Option<Card> {
    let colors = COLORS.get();
    let columns = spec.orientation == Orientation::Columns;
    let arc = spec.arc();
    let tile = spec.tile(colors);
    let mut corners = Vec::new();
    let mut fills = Vec::new();
    // A strip `t` DIP thick across the card's edge: Rows stack strips vertically (1 wide, `t`
    // high), Columns side by side.
    let across = |t: i32| if columns { (t, 1) } else { (1, t) };
    let along = |t: i32| if columns { (1, t) } else { (t, 1) };

    // The rows (Columns: columns) holding the corners.
    let mut end = |first: bool| -> Option<Panel> {
        let size = if columns { Size { width: arc, height: 1 } } else { Size { width: 1, height: arc } };
        let (panel, layout) = clear_box(!columns, Some(size), no_insets())?;
        let (a, b) = match (spec.orientation, first) {
            (Orientation::Rows, true) => (Corner::TopLeft, Corner::TopRight),
            (Orientation::Rows, false) => (Corner::BottomLeft, Corner::BottomRight),
            (Orientation::Columns, true) => (Corner::TopLeft, Corner::BottomLeft),
            (Orientation::Columns, false) => (Corner::TopRight, Corner::BottomRight),
        };
        let mut image = tile_image(&tile, a)?;
        let start = image_view(&mut image, arc, arc)?;
        panel.add_child_view(Some(&mut View::from(&start)));
        corners.push((a, start));
        // Between the corners: shadow strips, the 1 DIP border, then fill up to the page.
        let (edge, edge_layout) = clear_box(columns, None, no_insets())?;
        let mut parts: Vec<(Fill, i32)> = (0..spec.shadow).map(|i| (Fill::Shadow(i), 1)).collect();
        parts.push((Fill::Border, 1));
        parts.push((Fill::Interior, spec.radius - 1));
        if !first {
            parts.reverse();
        }
        for (fill, t) in parts {
            let (w, h) = across(t);
            let p = fill_panel(fill, &spec, w, h)?;
            edge.add_child_view(Some(&mut View::from(&p)));
            if fill == Fill::Interior {
                edge_layout.set_flex_for_view(Some(&mut View::from(&p)), 1);
            }
            fills.push((fill, p));
        }
        panel.add_child_view(Some(&mut View::from(&edge)));
        layout.set_flex_for_view(Some(&mut View::from(&edge)), 1);
        let mut image = tile_image(&tile, b)?;
        let end = image_view(&mut image, arc, arc)?;
        panel.add_child_view(Some(&mut View::from(&end)));
        corners.push((b, end));
        Some(panel)
    };
    let first = end(true)?;
    let last = end(false)?;

    // The middle: shadow strips, the border host (1 DIP insets = the side borders), the fill
    // panel (pad insets) that parents the views, shadow strips. The BrowserView layer snaps to
    // device pixels on its own, so what may show beside it is fill and border, never the page
    // underneath (spike pitfalls 2 and 3).
    let (middle, middle_layout) = clear_box(!columns, None, no_insets())?;
    let strips = |middle: &Panel, fills: &mut Vec<(Fill, Panel)>, outer_first: bool| -> Option<()> {
        let mut order: Vec<i32> = (0..spec.shadow).collect();
        if !outer_first {
            order.reverse();
        }
        for i in order {
            let (w, h) = along(1);
            let p = fill_panel(Fill::Shadow(i), &spec, w, h)?;
            middle.add_child_view(Some(&mut View::from(&p)));
            fills.push((Fill::Shadow(i), p));
        }
        Some(())
    };
    strips(&middle, &mut fills, true)?;
    let host = fill_panel(Fill::Border, &spec, 1, 1)?;
    let border_insets = if columns { Insets { top: 1, left: 0, bottom: 1, right: 0 } } else { Insets { top: 0, left: 1, bottom: 0, right: 1 } };
    let host_layout = host.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: !columns as i32,
        inside_border_insets: border_insets,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    }))?;
    let inner = fill_panel(Fill::Interior, &spec, 1, 1)?;
    let pad_insets = if columns {
        Insets { top: spec.pad, left: 0, bottom: spec.pad, right: 0 }
    } else {
        Insets { top: 0, left: spec.pad, bottom: 0, right: spec.pad }
    };
    inner.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: !spec.vertical_content as i32,
        inside_border_insets: pad_insets,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    }))?;
    host.add_child_view(Some(&mut View::from(&inner)));
    host_layout.set_flex_for_view(Some(&mut View::from(&inner)), 1);
    middle.add_child_view(Some(&mut View::from(&host)));
    middle_layout.set_flex_for_view(Some(&mut View::from(&host)), 1);
    strips(&middle, &mut fills, false)?;
    fills.push((Fill::Border, host));
    fills.push((Fill::Interior, inner.clone()));

    let root_layout = root.get_layout().and_then(|l| l.as_box_layout())?;
    root.add_child_view(Some(&mut View::from(&first)));
    root.add_child_view(Some(&mut View::from(&middle)));
    root_layout.set_flex_for_view(Some(&mut View::from(&middle)), 1);
    root.add_child_view(Some(&mut View::from(&last)));
    root.layout();
    Some(Card { spec, inner, corners, fills })
}

impl Card {
    /// Theme change: recolor the fills and swap the corner images.
    pub fn recolor(&self) {
        let colors = COLORS.get();
        for (fill, panel) in &self.fills {
            panel.set_background_color(fill_color(*fill, &self.spec, colors));
        }
        let tile = self.spec.tile(colors);
        for (corner, button) in &self.corners {
            if let Some(mut image) = tile_image(&tile, *corner) {
                button.set_image(ButtonState::NORMAL, Some(&mut image));
            }
        }
    }
}

// ----------------------------------------------------------------------------------- debug

#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by debug.rs only
pub fn debug_snapshot() -> serde_json::Value {
    #[allow(clippy::type_complexity)]
    let masks: Vec<(usize, Corner, OverlayController, Tile)> =
        MASKS.with(|m| m.borrow().iter().map(|k| (k.slot, k.corner, k.controller.clone(), k.tile)).collect());
    let list: Vec<serde_json::Value> = masks
        .into_iter()
        .map(|(slot, corner, controller, tile)| {
            let b = controller.bounds();
            serde_json::json!({
                "slot": slot,
                "corner": format!("{corner:?}"),
                "visible": controller.is_visible() != 0,
                "bounds": [b.x, b.y, b.width, b.height],
                "outside": format!("#{:08x}", tile.outside),
                "ring": format!("#{:08x}", tile.ring),
            })
        })
        .collect();
    let peek: Vec<serde_json::Value> = PEEK_MASKS
        .with(|m| m.borrow().iter().map(|k| (k.corner, k.controller.clone())).collect::<Vec<_>>())
        .into_iter()
        .map(|(corner, controller)| {
            let b = controller.bounds();
            serde_json::json!({ "corner": format!("{corner:?}"), "visible": controller.is_visible() != 0, "bounds": [b.x, b.y, b.width, b.height] })
        })
        .collect();
    let s = STATS.get();
    let c = COLORS.get();
    serde_json::json!({
        "masks": list,
        "peekMasks": peek,
        "snapUnit": window_snap_unit(),
        "colors": {
            "frame": format!("#{:08x}", c.frame),
            "accent": format!("#{:08x}", c.accent),
            "surface": format!("#{:08x}", c.surface),
            "border": format!("#{:08x}", c.border),
            "frameBorder": format!("#{:08x}", c.frame_border),
            "dark": c.dark,
        },
        "stats": {
            "imagesBuilt": s.images_built,
            "imageBuildUs": s.image_build_us,
            "cacheHits": s.cache_hits,
            "maskLayouts": s.mask_layouts,
            "maskShows": s.mask_shows,
            "maskHides": s.mask_hides,
            "restacks": s.restacks,
            "cardsBuilt": s.cards_built,
            "cardBuildUs": s.card_build_us,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLORS_T: ChromeColors =
        ChromeColors { frame: 0xFF11_2233, accent: 0xFF44_5566, surface: 0xFF23_2228, border: 0xFF39_383E, frame_border: 0xFF30_3040, dark: true };

    fn px(bytes: &[u8], pw: i32, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * pw + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    }

    #[test]
    fn mask_tile_regions() {
        let t = mask_tile(0xFF11_2233, 0xFF44_5566, 0, 0);
        let (pw, ph, p) = tile_pixels(&t, 1.0, Corner::TopLeft);
        assert_eq!((pw, ph), (12, 12));
        // Far outside the arc: frame, opaque (premultiplied BGRA).
        assert_eq!(px(&p, pw, 0, 0), [0x33, 0x22, 0x11, 0xFF]);
        // At the center corner: inside, transparent.
        assert_eq!(px(&p, pw, 11, 11), [0, 0, 0, 0]);
        // Where the arc meets the pane's straight edges: the 2 DIP ring, then the page.
        let ring = [0x66, 0x55, 0x44, 0xFF];
        assert_eq!(px(&p, pw, 0, 11), ring);
        assert_eq!(px(&p, pw, 1, 11), ring);
        assert_eq!(px(&p, pw, 11, 0), ring);
        assert_eq!(px(&p, pw, 2, 11), [0, 0, 0, 0], "page");
        // The arc is antialiased: some partially covered pixels.
        assert!(p.chunks(4).any(|c| c[3] > 0 && c[3] < 0xFF));
        // Mirrored corners and exact sizes at fractional scales.
        let (bw, _, br) = tile_pixels(&t, 1.0, Corner::BottomRight);
        assert_eq!(px(&br, bw, 11, 11), [0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(px(&br, bw, 0, 0), [0, 0, 0, 0]);
        assert_eq!(tile_pixels(&t, 1.25, Corner::TopRight).0, 15);
        assert_eq!(tile_pixels(&t, 1.5, Corner::BottomLeft).1, 18);
    }

    #[test]
    fn mask_tile_ring_color() {
        let t = mask_tile(0xFF00_0000, 0xFFFF_0000, 4, 0);
        let (pw, ph, p) = tile_pixels(&t, 2.0, Corner::TopLeft);
        assert_eq!((pw, ph), (32, 24));
        // The ring on the bottom row (TL orientation: arc center at the bottom-right corner) is red.
        assert_eq!(px(&p, pw, 2 * 4 + 1, 23), [0, 0, 0xFF, 0xFF]);
        // The bleed column is outside: black.
        assert_eq!(px(&p, pw, 0, 23), [0, 0, 0, 0xFF]);
    }

    #[test]
    fn card_tile_shadow_and_fill() {
        let spec = CardSpec { radius: 12, shadow: 8, pad: 3, orientation: Orientation::Rows, vertical_content: false, palette: Palette::Surface };
        let t = spec.tile(COLORS_T);
        let (pw, ph, p) = tile_pixels(&t, 1.0, Corner::TopLeft);
        assert_eq!((pw, ph), (20, 20));
        assert_eq!(px(&p, pw, 0, 0), [0, 0, 0, 0], "beyond the shadow: transparent");
        assert_eq!(px(&p, pw, 19, 19), [0x28, 0x22, 0x23, 0xFF], "inside: surface");
        let s = px(&p, pw, 19, 7);
        assert!(s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] > 0, "shadow just outside the top edge: {s:?}");
        assert_eq!(px(&p, pw, 19, 8), [0x3E, 0x38, 0x39, 0xFF], "border row");
        // The straight shadow in the corner image matches the strips beside it.
        for i in 0..8 {
            let tile_alpha = px(&p, pw, 19, i)[3] as i32;
            let strip = strip_alpha(i, 8, COLORS_T.shadow_peak()) as i32;
            assert!((tile_alpha - strip).abs() <= 1, "strip {i}: tile {tile_alpha} vs strip {strip}");
        }
        // Monotonic falloff.
        assert!((0..7).all(|i| strip_alpha(i, 8, 0.2) <= strip_alpha(i + 1, 8, 0.2)));
        assert_eq!(strip_alpha(0, 0, 0.2), 0);
    }

    #[test]
    fn every_card_is_on_the_4_dip_grid() {
        let specs = [
            CardSpec { radius: OVERLAY_RADIUS, shadow: OVERLAY_SHADOW, pad: OVERLAY_PAD, orientation: Orientation::Rows, vertical_content: false, palette: Palette::Surface },
            CardSpec { radius: OVERLAY_RADIUS, shadow: OVERLAY_SHADOW, pad: PEEK_PAD, orientation: Orientation::Rows, vertical_content: true, palette: Palette::Surface },
            CardSpec { radius: FIND_RADIUS, shadow: OVERLAY_SHADOW, pad: OVERLAY_PAD, orientation: Orientation::Columns, vertical_content: false, palette: Palette::Surface },
            CardSpec { radius: TOAST_RADIUS, shadow: OVERLAY_SHADOW, pad: OVERLAY_PAD, orientation: Orientation::Columns, vertical_content: false, palette: Palette::Surface },
            CardSpec { radius: OVERLAY_RADIUS, shadow: SIDEBAR_SHADOW, pad: OVERLAY_PAD, orientation: Orientation::Rows, vertical_content: false, palette: Palette::Frame },
        ];
        for spec in specs {
            let (h, v) = spec.chrome();
            assert!(h % 4 == 0 && v % 4 == 0 && spec.shadow % 4 == 0, "{spec:?}: chrome {h}x{v}");
            let tile = spec.tile(COLORS_T);
            assert_eq!(exact_scales(&tile).count(), SCALES.len(), "{spec:?}: exact images at every scale");
        }
        let peek = peek_mask_placement(&Rect { x: 0, y: 0, width: 100, height: 100 }, Corner::TopLeft, 0).unwrap().1;
        assert_eq!(exact_scales(&peek).count(), SCALES.len());
        let mask = mask_tile(0, 0, MASK_BLEED, 0);
        assert_eq!(exact_scales(&mask).count(), SCALES.len());
        assert_eq!(exact_scales(&mask_tile(0, 0, 0, 0)).count(), SCALES.len());
        // A 13 DIP image would have no exact 150 % bitmap (the spike's ringing).
        let odd = Tile { w: 13, ..mask };
        assert!(!exact_scales(&odd).any(|s| s == 1.5));
    }

    #[test]
    fn snapping() {
        assert_eq!(snap_unit(1.0), 1);
        assert_eq!(snap_unit(2.0), 1);
        assert_eq!(snap_unit(1.5), 2);
        assert_eq!(snap_unit(2.5), 2);
        assert_eq!(snap_unit(1.25), 4);
        assert_eq!(snap_unit(1.75), 4);
        assert_eq!(snap_unit(1.1), 4);
        let r = Rect { x: 5, y: 6, width: 11, height: 13 };
        let g = snap_rect(&r, 4, true);
        assert_eq!((g.x, g.y, g.width, g.height), (4, 4, 12, 16));
        let s = snap_rect(&r, 4, false);
        assert_eq!((s.x, s.y, s.width, s.height), (8, 8, 8, 8));
        let n = snap_rect(&Rect { x: -3, y: 0, width: 2, height: 1 }, 4, true);
        assert_eq!((n.x, n.width), (-4, 4));
        let same = snap_rect(&r, 1, true);
        assert_eq!((same.x, same.y, same.width, same.height), (5, 6, 11, 13));
        for scale in [1.25f32, 1.5, 1.75, 2.25] {
            let u = snap_unit(scale);
            let g = snap_rect(&Rect { x: 293, y: 117, width: 587, height: 301 }, u, true);
            for v in [g.x, g.y, g.x + g.width, g.y + g.height] {
                assert!(((v as f32 * scale) - (v as f32 * scale).round()).abs() < 0.001, "{scale}: {v}");
            }
        }
        // Host rects: the card grows by its shadow, then snaps.
        let spec = CardSpec { radius: 12, shadow: 8, pad: 3, orientation: Orientation::Rows, vertical_content: false, palette: Palette::Surface };
        let h = host_rect(&Rect { x: 301, y: 113, width: 587, height: 97 }, &spec, 4, true);
        assert_eq!((h.x, h.y, h.width, h.height), (292, 104, 604, 116));
        let i = inset(&h, spec.chrome().0, spec.chrome().1);
        assert_eq!((i.x, i.y, i.width, i.height), (304, 124, 580, 76));
    }

    #[test]
    fn mask_placement_bleeds_only_into_frame() {
        let content = Rect { x: 248, y: 40, width: 1000, height: 700 };
        let c = COLORS_T;
        let at = |pane: &Rect, ring, hidden, corner| mask_placement(pane, ring, &content, hidden, corner, c).map(|(r, t)| ([r.x, r.y, r.width, r.height], t.ring));
        // A single pane with the sidebar docked: no bleed to the left or top.
        let single = content.clone();
        assert_eq!(at(&single, c.frame, false, Corner::TopLeft), Some(([248, 40, 12, 12], c.frame)));
        assert_eq!(at(&single, c.frame, false, Corner::TopRight), Some(([1236, 40, 16, 12], c.frame)));
        assert_eq!(at(&single, c.frame, false, Corner::BottomLeft), Some(([248, 728, 12, 16], c.frame)));
        assert_eq!(at(&single, c.frame, false, Corner::BottomRight), Some(([1236, 728, 16, 16], c.frame)));
        // Sidebar hidden: the left inset is frame.
        assert_eq!(at(&single, c.frame, true, Corner::TopLeft), Some(([244, 40, 16, 12], c.frame)));
        // The right pane of a split bleeds into the gap; the focused one gets the accent ring.
        let right = Rect { x: 751, y: 40, width: 497, height: 700 };
        assert_eq!(at(&right, c.accent, false, Corner::TopLeft), Some(([747, 40, 16, 12], c.accent)));
        // An agent-framed pane: the ring takes the agent color, outside stays frame.
        let agent = 0xFFE8_641B;
        assert_eq!(at(&right, agent, false, Corner::BottomRight), Some(([1236, 728, 16, 16], agent)));
        assert_eq!(mask_placement(&right, agent, &content, false, Corner::TopRight, c).map(|(_, t)| t.outside), Some(c.frame));
        // The bottom pane of a vertical split bleeds upwards.
        let bottom = Rect { x: 248, y: 393, width: 1000, height: 347 };
        assert_eq!(at(&bottom, c.frame, false, Corner::TopRight), Some(([1236, 389, 16, 16], c.frame)));
        // Too small for its corners.
        assert_eq!(at(&Rect { x: 248, y: 40, width: 23, height: 100 }, c.frame, false, Corner::TopLeft), None);
    }
}
