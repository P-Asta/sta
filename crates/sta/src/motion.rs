//! Motion timing [owner: chrome] (ARCHITECTURE §4.7 "Motion", docs/PROTOCOL.md §14).
//!
//! Core decides *what* may animate (`sta_core::motion`), the HTML surfaces do the animating. Native
//! geometry is instant — an `OverlayController` has no opacity — so the shell's only part in motion
//! is **timing**, and this module owns all of it:
//!
//! - **Acknowledged exits.** A surface presents a blank frame *before* the shell hides the widget it
//!   lives in, because a hidden page renders no frames and Chromium keeps showing its last one (the
//!   stale-frame rule). The shell asks the page to blank (`surface.exit {gen}`, or the sidebar's
//!   `sidebar.hover {visible:false, gen}`), the page answers `surface.exited {gen}` once that frame
//!   has been rendered, and the widget goes away on the ack — never before [`HIDE_FLOOR_MS`] /
//!   [`PARK_FLOOR_MS`], never later than the cap. The wait between those two is a **correctness**
//!   delay, not an animation: turning the animation off only drops the page's fade, it never makes
//!   the wait 0 (critique issues 1-3).
//! - **The `SetChrome` midpoint.** While a page cross-fades its own colours (`theme.crossFade`), the
//!   native card fill, border and corner tiles can only *snap*; snapping at the middle of the fade
//!   keeps the seam from being a 3-4 DIP ring of mismatched colour for the whole fade (issue 12).
//!   [`chrome_delay_ms`] is that midpoint; window.rs applies it generation-guarded and skips it at
//!   startup and while the window cannot show it.
//!
//! An exit **lingers**: the widget stays visible while the page blanks. That is deliberate and
//! accepted (FINAL PLAN §4.5) — a lingering overlay is still restacked above a corner mask and still
//! a no-drag hole, exactly as a visible one is.
//!
//! Only non-activatable surfaces linger. Activatable overlays (command bar, find bar, permission
//! prompt, Peek) close instantly: the page blanks synchronously and dispatches after one frame, and
//! `CloseCommandBar {seq}` keeps a close meant for a bar that is already gone from closing the one
//! that replaced it.
//!
//! Public API:
//! - `pub const EXIT_FADE_MS`, `ACK_MS`, `HIDE_FLOOR_MS`, `PARK_FLOOR_MS`, `WAIT_CAP_MS`, `THEME_FADE_MS`
//! - `pub fn exit_fade_ms(key: &str) -> i64`, `pub fn hide_delay_ms(key) -> i64`, `pub fn park_delay_ms(key) -> i64`
//! - `pub fn begin(browser_id, key, floor, ask, done)` — start an acknowledged exit
//! - `pub fn on_exited(browser_id: i32, gen: u64)` — the page's ack (`surface.exited`)
//! - `pub fn cancel(browser_id: i32) -> bool` — a show during the linger (generation check)
//! - `pub fn note_early_hide()` — a hide that could not ask any page
//! - `pub fn chrome_delay_ms() -> i64`
//! - `pub fn lingering() -> usize`, `pub fn debug_counters() -> serde_json::Value`
//! - `pub fn clear()` — shutdown (drop pending exits without running them)

use crate::{controller, task};
use sta_core::motion::MotionLevel;
use std::cell::{Cell, RefCell};
use std::time::Instant;

/// The longest exit fade a page may play before the shell hides it: `tokens.css --t-surface-exit`.
/// Every key that owns an acknowledged exit has a duration token at least this long, so the fade the
/// page actually plays is `min(token, --t-surface-exit)` = this (checked by `tools/check-motion.mjs`).
pub const EXIT_FADE_MS: i64 = 60;
/// Allowance past the fade for the ack to arrive: one 30 Hz frame for the blank frame plus the IPC
/// hop back. The ack normally arrives well inside it; the cap only covers a page that cannot answer.
pub const ACK_MS: i64 = 48;
/// A hidden overlay is never hidden sooner than this after its page was asked to blank.
pub const HIDE_FLOOR_MS: i64 = 50;
/// A parked sidebar is never parked sooner than this (the view is re-parented, so the frame the
/// floating host would show is the docked one).
pub const PARK_FLOOR_MS: i64 = 60;
/// Neither wait ever exceeds this, whatever the fade is.
pub const WAIT_CAP_MS: i64 = 120;
/// The theme cross-fade, as `tokens.css --t-theme-cross-fade` spells it.
pub const THEME_FADE_MS: i64 = 300;
/// The floating sidebar slides in over this long (`tokens.css --t-sidebar-hover-reveal`).
pub const SLIDE_IN_MS: i64 = 160;
/// …and back out over this long. Leaving is the longer half here: nothing waits for it (the pointer
/// has already moved on) and it starts the moment the pointer leaves, so it is the whole of what
/// "the sidebar goes away" looks like.
pub const SLIDE_OUT_MS: i64 = 240;
/// The card waits this long, still outside the window, before it starts coming in: the page is
/// told to show its contents at the same moment, and this is the frame or two it needs to paint
/// them. Nothing is on screen meanwhile — the card is entirely outside the window.
pub const SLIDE_LEAD_MS: i64 = 24;
/// How often a slide asks where the card should be. **Not** a frame time: Windows runs delayed
/// tasks on its 15.6 ms timer tick, so a 16 ms delay lands on every *second* tick — measured, a
/// 160 ms slide got 7 steps with 31 ms gaps, which is what "it drops frames" looked like. A delay
/// well under one tick lands on every tick (and on every high-resolution wake-up when Chromium has
/// them on): 20 steps, no gap over 16 ms. A step that would not move the card is dropped in
/// `overlays::set_sidebar_hover_slide`, and a step that does is cheap — the card is moved and
/// clipped, its page is never resized (`overlays::layout_overlay`).
const SLIDE_STEP_MS: i64 = 4;

/// The animation key whose exit fade the toast plays.
pub const TOAST_KEY: &str = "overlays.toast";
/// …the switcher.
pub const SWITCHER_KEY: &str = "overlays.switcher";
/// …the floating sidebar (hide and park).
pub const SIDEBAR_KEY: &str = "sidebar.hoverReveal";
/// The theme cross-fade key.
pub const THEME_KEY: &str = "theme.crossFade";

/// Whether `key` may animate at all right now (the master switch, the level and the key's own
/// switch). Unknown to the store (not ready yet) counts as on, like the pages' own default.
fn key_on(key: &str) -> bool {
    let Some(view) = controller::with_store(|s| s.motion_view()) else { return true };
    view.level != MotionLevel::Off && !view.off.iter().any(|k| k == key)
}

/// The exit fade `key`'s page will play: [`EXIT_FADE_MS`], or 0 when the key (or all motion) is off.
/// The fade is the page's; the wait around it is the shell's and is never 0 ([`hide_delay_ms`]).
pub fn exit_fade_ms(key: &str) -> i64 {
    if key_on(key) { EXIT_FADE_MS } else { 0 }
}

/// How long the shell waits for a page that was asked to blank: `max(floor, fade + ack)`, capped at
/// [`WAIT_CAP_MS`] and never below `floor`. An ack ends the wait early (but never before `floor`).
fn wait_ms(key: &str, floor: i64) -> i64 {
    let floor = floor_of(floor);
    (exit_fade_ms(key) + ACK_MS).clamp(floor, WAIT_CAP_MS.max(floor))
}

/// The floor a wait must respect. Debug builds let an e2e check raise it (`debug.motion {floorMs}`)
/// so it can act *during* a linger — which is exactly the window the generation checks exist for —
/// without racing a 108 ms timer over IPC.
fn floor_of(floor: i64) -> i64 {
    #[cfg(debug_assertions)]
    if let Some(ms) = FLOOR_OVERRIDE.get() {
        return ms;
    }
    floor
}

/// Wait before hiding a non-activatable overlay or the floating sidebar.
pub fn hide_delay_ms(key: &str) -> i64 {
    wait_ms(key, HIDE_FLOOR_MS)
}

/// Wait before parking the sidebar view in the floating host.
pub fn park_delay_ms(key: &str) -> i64 {
    wait_ms(key, PARK_FLOOR_MS)
}

/// Delay for `Effect::SetChrome` while a page-side theme cross-fade is running: half the fade, so
/// the native colours snap in its middle. 0 when the fade is not running at all. window.rs decides
/// the rest (startup, minimized, closing) and guards it by generation.
pub fn chrome_delay_ms() -> i64 {
    if key_on(THEME_KEY) { THEME_FADE_MS / 2 } else { 0 }
}

// ------------------------------------------------------------------------------ sidebar slide

/// Whether the floating sidebar may *travel* rather than appear: its key is on and the level is
/// `full`. At `reduced` every position animation snaps (`motion.js moves()`), and the card is shown
/// and hidden where it belongs, exactly as it was before it could slide.
pub fn slides() -> bool {
    key_on(SIDEBAR_KEY) && controller::with_store(|s| s.motion_view().level).unwrap_or(MotionLevel::Full) == MotionLevel::Full
}

/// Where the card is `elapsed` ms into a slide from `from` to `to` over `ms`: ease-out cubic, the
/// end points exact. Pure, so the curve is unit-tested rather than watched.
pub fn slide_offset(from: i32, to: i32, elapsed: i64, ms: i64) -> i32 {
    if ms <= 0 || elapsed >= ms {
        return to;
    }
    if elapsed <= 0 {
        return from;
    }
    let t = elapsed as f64 / ms as f64;
    let eased = 1.0 - (1.0 - t).powi(3);
    from + ((to - from) as f64 * eased).round() as i32
}

struct Slide {
    generation: u64,
    from: i32,
    to: i32,
    ms: i64,
    started: Instant,
    /// When the last step ran, how many ran, and the longest wait between two of them: what
    /// `debug.info.motion.lastSlide` reports, because "did it stutter" is a number, not an opinion.
    last_step: Instant,
    steps: u32,
    max_gap_ms: i64,
    /// Runs when the card has arrived (never when the slide is cancelled).
    done: Option<Box<dyn FnOnce()>>,
}

thread_local! {
    static SLIDE: RefCell<Option<Slide>> = const { RefCell::new(None) };
    static SLIDE_GEN: Cell<u64> = const { Cell::new(0) };
    /// The last slide that arrived: `(steps, ms, longest gap between two steps in ms)`.
    static LAST_SLIDE: Cell<(u32, i64, i64)> = const { Cell::new((0, 0, 0)) };
}

/// Slides the floating sidebar host from `from` to `to` over `ms`, then runs `done`. The offset is
/// how far left of its home rect the card is drawn; the window clips whatever hangs past its left
/// edge (`overlays::set_sidebar_hover_slide`). A slide that
/// is already running is dropped (its `done` never runs): the card is wherever it got to, and the
/// new slide starts from there — an arrival that interrupts a leave never jumps.
pub fn slide_sidebar(from: i32, to: i32, ms: i64, done: impl FnOnce() + 'static) {
    let ms = slide_ms(ms);
    cancel_slide();
    crate::overlays::set_sidebar_hover_slide(from);
    if ms <= 0 || from == to {
        crate::overlays::set_sidebar_hover_slide(to);
        done();
        return;
    }
    let generation = SLIDE_GEN.get() + 1;
    SLIDE_GEN.set(generation);
    let now = Instant::now();
    SLIDE.with(|s| {
        *s.borrow_mut() = Some(Slide { generation, from, to, ms, started: now, last_step: now, steps: 0, max_gap_ms: 0, done: Some(Box::new(done)) })
    });
    task::post_ui_delayed(SLIDE_STEP_MS, move || slide_step(generation));
}

fn slide_step(generation: u64) {
    let step = SLIDE.with(|s| {
        let mut slide = s.borrow_mut();
        let slide = slide.as_mut().filter(|x| x.generation == generation)?;
        let elapsed = slide.started.elapsed().as_millis() as i64;
        slide.max_gap_ms = slide.max_gap_ms.max(slide.last_step.elapsed().as_millis() as i64);
        slide.last_step = Instant::now();
        slide.steps += 1;
        Some((slide_offset(slide.from, slide.to, elapsed, slide.ms), elapsed >= slide.ms))
    });
    let Some((dx, arrived)) = step else { return };
    crate::overlays::set_sidebar_hover_slide(dx);
    if !arrived {
        task::post_ui_delayed(SLIDE_STEP_MS, move || slide_step(generation));
        return;
    }
    let done = SLIDE.with(|s| {
        let mut finished = s.borrow_mut().take()?;
        LAST_SLIDE.set((finished.steps, finished.started.elapsed().as_millis() as i64, finished.max_gap_ms));
        finished.done.take()
    });
    crate::window::schedule_draggable_regions(); // once, where the card came to rest
    if let Some(done) = done {
        done();
    }
}

/// Drops a running slide (its `done` never runs) and leaves the card where it is.
pub fn cancel_slide() {
    SLIDE_GEN.set(SLIDE_GEN.get() + 1);
    let dropped = SLIDE.with(|s| s.borrow_mut().take());
    drop(dropped);
}

/// Nothing is sliding right now (`debug.info.sidebarHover.sliding`).
#[cfg_attr(not(debug_assertions), allow(dead_code))] // reported by the hover snapshot only
pub fn slide_settled() -> bool {
    SLIDE.with(|s| s.borrow().is_none())
}

// --------------------------------------------------------------------------- acknowledged exits

struct Exit {
    generation: u64,
    browser_id: i32,
    key: &'static str,
    /// The page must have had at least this long, whatever the ack says.
    floor: i64,
    asked_at: Instant,
    /// The page has answered (the wait may still be running out its floor).
    acked: bool,
    /// Run when the surface may be hidden (the ack, or the cap).
    done: Box<dyn FnOnce()>,
}

#[derive(Default, Clone, Copy)]
struct Counters {
    /// Exits that entered the protocol.
    exits: u64,
    /// Exits that ended on the page's ack.
    acks: u64,
    /// Exits that ended at the cap instead (the page never answered in time).
    ack_timeouts: u64,
    /// Exits cancelled by a show during the linger.
    cancels: u64,
    /// Hides that went through with no blanking wait at all: there was no live page to ask.
    early_hides: u64,
    /// Acks for an exit that is no longer pending (a cancelled or already finished one).
    stale_acks: u64,
    /// Longest wait an exit actually took, in ms.
    slowest_ms: i64,
    /// The wait the last finished exit took, in ms.
    last_ms: i64,
}

#[cfg(debug_assertions)]
thread_local! {
    /// `debug.motion {floorMs}`: the exit floor an e2e check asked for, `None` = the real ones.
    static FLOOR_OVERRIDE: Cell<Option<i64>> = const { Cell::new(None) };
    /// `debug.motion {slideMs}`: the sidebar slide stretched so a check (or an eye) can see it
    /// part-way, `None` = [`SLIDE_IN_MS`] / [`SLIDE_OUT_MS`].
    static SLIDE_OVERRIDE: Cell<Option<i64>> = const { Cell::new(None) };
}

/// The duration a slide actually runs for: `ms`, or the debug override.
fn slide_ms(ms: i64) -> i64 {
    #[cfg(debug_assertions)]
    if let Some(over) = SLIDE_OVERRIDE.get() {
        return over;
    }
    ms
}

/// `debug.motion {slideMs}` (debug builds).
#[cfg(debug_assertions)]
pub fn debug_set_slide_ms(ms: Option<i64>) {
    SLIDE_OVERRIDE.set(ms);
}

thread_local! {
    static EXITS: RefCell<Vec<Exit>> = const { RefCell::new(Vec::new()) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static COUNTERS: Cell<Counters> = const { Cell::new(Counters { exits: 0, acks: 0, ack_timeouts: 0, cancels: 0, early_hides: 0, stale_acks: 0, slowest_ms: 0, last_ms: 0 }) };
}

/// Starts an acknowledged exit for the page in `browser_id`.
///
/// `ask` sends the request to that page and is given the generation to echo back; `floor` is the
/// shortest the page may have ([`HIDE_FLOOR_MS`] / [`PARK_FLOOR_MS`]); `done` hides the surface and
/// runs exactly once — on the page's `surface.exited {gen}` (never before `floor`) or at the cap.
/// A show during the linger cancels it with [`cancel`], and then `done` never runs.
///
/// One exit per surface at a time. Every caller already guarantees that — an overlay's `Hide*` effect
/// is emitted once per `Show*`, the hover machine only returns `Hide` while it thinks it is visible,
/// and the park is guarded by `PARK_PENDING` — so a second `begin` for the same browser would mean
/// two pages' worth of `done` for one surface.
pub fn begin(browser_id: i32, key: &'static str, floor: i64, ask: impl FnOnce(u64), done: impl FnOnce() + 'static) {
    let floor = floor_of(floor);
    // A page that cannot be asked cannot blank: hide at once rather than stall the surface for the
    // whole cap. Forced hides (docking, page fullscreen, shutdown) never come through here at all.
    if !crate::ipc::has_subscriber(browser_id) {
        note_early_hide();
        done();
        return;
    }
    let generation = GENERATION.get() + 1;
    GENERATION.set(generation);
    let mut c = COUNTERS.get();
    c.exits += 1;
    COUNTERS.set(c);
    EXITS.with(|e| {
        e.borrow_mut().push(Exit { generation, browser_id, key, floor, asked_at: Instant::now(), acked: false, done: Box::new(done) })
    });
    ask(generation);
    let cap = wait_ms(key, floor);
    task::post_ui_delayed(cap, move || finish(generation, true));
    log_debug!("surface exit {generation} ({key}, browser {browser_id}): floor {floor} ms, cap {cap} ms");
}

/// `surface.exited {gen}` from a page: the blank frame has been rendered. Finishes the exit — at
/// once when the floor has already passed, else exactly at the floor.
pub fn on_exited(browser_id: i32, generation: u64) {
    let found = EXITS.with(|e| {
        // The answer is remembered even when the floor still has time to run: what `ackTimeouts`
        // counts is a page that never answered, not which of the two timers got there first.
        e.borrow_mut()
            .iter_mut()
            .find(|x| x.generation == generation && x.browser_id == browser_id)
            .map(|x| {
                x.acked = true;
                (x.floor - x.asked_at.elapsed().as_millis() as i64, x.key)
            })
    });
    let Some((left, key)) = found else {
        let mut c = COUNTERS.get();
        c.stale_acks += 1;
        COUNTERS.set(c);
        return;
    };
    if left <= 0 {
        finish(generation, false);
    } else {
        // The page answered inside the correctness floor: wait out the rest of it.
        log_debug!("surface exit {generation} ({key}) acked {left} ms before its floor");
        task::post_ui_delayed(left, move || finish(generation, false));
    }
}

/// Ends the exit `generation` and hides its surface. `timed_out`: the cap ran out — which only counts
/// as a timeout if the page never answered at all (when a raised floor makes the cap and the floor
/// land on the same millisecond, the cap timer may simply get there first).
fn finish(generation: u64, timed_out: bool) {
    let exit = EXITS.with(|e| {
        let mut list = e.borrow_mut();
        let at = list.iter().position(|x| x.generation == generation)?;
        Some(list.remove(at))
    });
    let Some(exit) = exit else { return };
    let waited = exit.asked_at.elapsed().as_millis() as i64;
    let timed_out = timed_out && !exit.acked;
    let mut c = COUNTERS.get();
    if timed_out {
        c.ack_timeouts += 1;
    } else {
        c.acks += 1;
    }
    c.last_ms = waited;
    c.slowest_ms = c.slowest_ms.max(waited);
    COUNTERS.set(c);
    log_debug!(
        "surface exit {generation} ({}) finished after {waited} ms ({})",
        exit.key,
        if timed_out { "cap" } else { "ack" }
    );
    (exit.done)();
}

/// Drops every pending exit of `browser_id` without hiding anything: the surface was shown again
/// during the linger, so the widget must simply stay up. `true` if one was pending.
pub fn cancel(browser_id: i32) -> bool {
    let dropped: Vec<Exit> = EXITS.with(|e| {
        let mut list = e.borrow_mut();
        let (drop, keep): (Vec<Exit>, Vec<Exit>) = std::mem::take(&mut *list).into_iter().partition(|x| x.browser_id == browser_id);
        *list = keep;
        drop
    });
    if dropped.is_empty() {
        return false;
    }
    let mut c = COUNTERS.get();
    c.cancels += dropped.len() as u64;
    COUNTERS.set(c);
    for exit in &dropped {
        log_debug!("surface exit {} ({}) cancelled: shown again", exit.generation, exit.key);
    }
    drop(dropped);
    true
}

/// A hide that presented no blank frame because there was no page to ask (the surface's page is
/// gone, or it never subscribed). Counted so `debug.info.motion.earlyHides` stays 0 in normal runs.
pub fn note_early_hide() {
    let mut c = COUNTERS.get();
    c.early_hides += 1;
    COUNTERS.set(c);
}

/// How many surfaces are lingering (asked to blank, not hidden yet).
#[cfg_attr(not(debug_assertions), allow(dead_code))] // reported by `debug.info.motion` only
pub fn lingering() -> usize {
    EXITS.with(|e| e.borrow().len())
}

/// Drops pending exits (and a running slide) at shutdown: their `done` closures touch widgets that
/// are going away.
pub fn clear() {
    cancel_slide();
    let dropped = EXITS.with(|e| std::mem::take(&mut *e.borrow_mut()));
    drop(dropped);
}

/// `debug.motion {floorMs}` (debug builds): raise the exit floor — every exit then lingers that long
/// whatever the page answers, so an e2e check can act inside the linger instead of racing it. `None`
/// puts the real floors back. Clamped to 5 s so a forgotten override cannot wedge a surface.
#[cfg(debug_assertions)]
pub fn debug_set_floor(ms: Option<i64>) {
    FLOOR_OVERRIDE.set(ms.map(|m| m.clamp(0, 5000)));
    log_info!("motion: exit floor override = {:?}", FLOOR_OVERRIDE.get());
}

/// The floor override, if any (always `None` outside debug builds).
fn floor_override() -> Option<i64> {
    #[cfg(debug_assertions)]
    return FLOOR_OVERRIDE.get();
    #[cfg(not(debug_assertions))]
    None
}

/// The exit counters for `debug.info.motion` (debug builds).
#[cfg_attr(not(debug_assertions), allow(dead_code))] // used by window::motion_snapshot only
pub fn debug_counters() -> serde_json::Value {
    let c = COUNTERS.get();
    let pending: Vec<serde_json::Value> = EXITS.with(|e| {
        e.borrow()
            .iter()
            .map(|x| serde_json::json!({ "gen": x.generation, "key": x.key, "browserId": x.browser_id, "floorMs": x.floor, "waitedMs": x.asked_at.elapsed().as_millis() as i64 }))
            .collect()
    });
    serde_json::json!({
        "lingering": lingering(),
        "pending": pending,
        "exits": c.exits,
        "acks": c.acks,
        "ackTimeouts": c.ack_timeouts,
        "cancels": c.cancels,
        "earlyHides": c.early_hides,
        "staleAcks": c.stale_acks,
        "slowestExitMs": c.slowest_ms,
        "lastExitMs": c.last_ms,
        "floorOverrideMs": floor_override(),
        "lastSlide": { "steps": LAST_SLIDE.get().0, "ms": LAST_SLIDE.get().1, "maxGapMs": LAST_SLIDE.get().2, "stepMs": SLIDE_STEP_MS },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slide_eases_out_between_exact_end_points() {
        let (from, to, ms) = (-264, 0, SLIDE_IN_MS);
        assert_eq!(slide_offset(from, to, 0, ms), from);
        assert_eq!(slide_offset(from, to, -5, ms), from);
        assert_eq!(slide_offset(from, to, ms, ms), to);
        assert_eq!(slide_offset(from, to, ms + 500, ms), to);
        assert_eq!(slide_offset(from, to, 5, 0), to, "no duration: straight there");
        // Monotonic, never past either end, and ahead of a straight line (ease-out).
        let mut last = from;
        for t in 0..=ms {
            let x = slide_offset(from, to, t, ms);
            assert!((from..=to).contains(&x) && x >= last, "t = {t}: {x}");
            last = x;
        }
        let linear = |t: i64| from + ((to - from) as f64 * (t as f64 / ms as f64)) as i32;
        assert!(slide_offset(from, to, ms / 4, ms) > linear(ms / 4));
        assert!(slide_offset(from, to, ms / 2, ms) > linear(ms / 2));
        // Out is the same curve the other way.
        assert_eq!(slide_offset(0, from, 0, SLIDE_OUT_MS), 0);
        assert_eq!(slide_offset(0, from, SLIDE_OUT_MS, SLIDE_OUT_MS), from);
    }

    /// `wait_ms` with the fade given directly, so the delay arithmetic can be checked without a
    /// store (`exit_fade_ms` is the only thing that reads one).
    fn wait(fade: i64, floor: i64) -> i64 {
        (fade + ACK_MS).clamp(floor, WAIT_CAP_MS.max(floor))
    }

    #[test]
    fn the_waits_have_a_floor_a_cap_and_are_never_zero() {
        // The animation on: the fade plus the ack allowance, inside the cap.
        assert_eq!(wait(EXIT_FADE_MS, HIDE_FLOOR_MS), 108);
        assert_eq!(wait(EXIT_FADE_MS, PARK_FLOOR_MS), 108);
        // The animation off (fade 0): the correctness floor, never 0 — this is critique issue 1.
        assert_eq!(wait(0, HIDE_FLOOR_MS), HIDE_FLOOR_MS);
        assert_eq!(wait(0, PARK_FLOOR_MS), PARK_FLOOR_MS);
        // A long fade cannot push the wait past the cap.
        assert_eq!(wait(400, HIDE_FLOOR_MS), WAIT_CAP_MS);
        assert_eq!(wait(400, PARK_FLOOR_MS), WAIT_CAP_MS);
        for fade in [0, 1, 30, 60, 120, 5000] {
            for floor in [HIDE_FLOOR_MS, PARK_FLOOR_MS] {
                let w = wait(fade, floor);
                assert!(w >= floor && w <= WAIT_CAP_MS, "fade {fade}, floor {floor} -> {w}");
            }
        }
    }

    #[test]
    fn the_exit_fade_fits_inside_the_wait() {
        // The page's fade must finish with room for the ack, or every exit would hit the cap.
        const { assert!(EXIT_FADE_MS + ACK_MS <= WAIT_CAP_MS) };
        const { assert!(EXIT_FADE_MS < HIDE_FLOOR_MS + ACK_MS) };
        // And the fade must be shorter than the cap even at the tighter of the two floors.
        assert!(EXIT_FADE_MS < wait(EXIT_FADE_MS, HIDE_FLOOR_MS));
    }

    #[test]
    fn the_chrome_midpoint_is_half_the_fade() {
        // No store in a unit test: `key_on` says yes, which is the pages' own default.
        assert_eq!(chrome_delay_ms(), THEME_FADE_MS / 2);
        assert_eq!(exit_fade_ms(TOAST_KEY), EXIT_FADE_MS);
    }

    #[test]
    fn counters_start_empty() {
        assert_eq!(lingering(), 0);
        let c = debug_counters();
        for field in ["lingering", "exits", "acks", "ackTimeouts", "cancels", "earlyHides", "staleAcks"] {
            assert_eq!(c[field], 0, "{field}");
        }
    }

    #[test]
    fn a_stale_ack_is_counted_and_ignored() {
        let before = COUNTERS.get().stale_acks;
        on_exited(-1, 999_999);
        assert_eq!(COUNTERS.get().stale_acks, before + 1);
        assert_eq!(lingering(), 0);
        // Cancelling nothing is not an error either.
        assert!(!cancel(-1));
    }

    #[test]
    fn the_keys_are_registered_animations() {
        for key in [TOAST_KEY, SWITCHER_KEY, SIDEBAR_KEY, THEME_KEY] {
            assert!(sta_core::motion::spec(key).is_some(), "{key} is not in the registry");
        }
    }
}
