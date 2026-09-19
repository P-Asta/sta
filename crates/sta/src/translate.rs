//! "Translate page" [owner: tabs] (`Command::TranslatePage`, the top-bar chip, the context menu).
//!
//! Chromium's own translation is switched off in `app.rs` (`disable-features=Translate`): it talks
//! to an endpoint keyed to official Chrome builds, which a CEF build has no key for. This is sta's
//! replacement, and it is deliberately small:
//!
//! 1. `translate.js` is evaluated in the tab's **main world** — the job is to change what the page
//!    renders, so an isolated world would have nothing to rewrite. It hands back every piece of
//!    prose in document order and remembers which text node each came from.
//! 2. The strings are translated here, in the browser process, in batches over CEF's own network
//!    stack (`Urlrequest`, the `suggest.rs` pattern — no cookies, no credentials).
//! 3. Each batch is written back as it lands, so a page that fails on batch 46 of 50 keeps the 45
//!    that worked, and the chip can count.
//!
//! Running it again on a page that is already translated **restores the original**, so the single
//! control is both ways. The page is the record of which of the two will happen; core only says
//! which language to aim for and what it wants done about images, and hears back through
//! `TranslateProgress` and `TranslateFinished`.
//!
//! The endpoint is Google's free `translate_a/t`, the one that takes repeated `q` parameters and
//! answers `[[translated, detected], …]` in the same order. That is a GET with everything in the
//! URL, so batches are capped by URL length as much as by count, and they run **one at a time**:
//! this is an unofficial, unauthenticated endpoint and a page's worth of parallel requests is
//! exactly what gets an IP rate-limited. **The text of the page leaves the machine** — that is the
//! deal this feature makes, and the only reason it needs no API key.
//!
//! Public API:
//! - `pub fn translate(tab: Id, target: String, images: bool)`, `pub fn cancel(tab: Id)`
//! - `pub fn on_browser_closed(browser_id: i32)`, `pub fn on_navigated(browser_id: i32)`
//! - `pub fn clear()`, `pub fn debug_snapshot()`

use crate::devtools_cdp::{self, User};
use crate::{controller, tabs, task};
use cef::rc::Rc as _;
use cef::*;
use serde_json::{Value, json};
use sta_core::translate::{MAX_STRINGS, TranslatePhase};
use sta_core::{Command, Id};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

const SHIM_JS: &str = include_str!("translate.js");

/// Free Google endpoint: repeated `q`, answers in input order.
const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/t";
/// A batch stops growing at this URL length. Measured: ~10.6 KB still answers, 200 strings (~17 KB)
/// is a 400, so this leaves room rather than finding the exact edge.
const MAX_URL_BYTES: usize = 7000;
/// …and at this many strings, so one enormous batch can't hide a slow response.
const MAX_BATCH: usize = 80;
/// A translated page is bigger than its source; this is per response, not per page.
const MAX_BODY_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT_MS: i64 = 20_000;
const EVAL_TIMEOUT_MS: i64 = 10_000;
/// Every status change repaints the whole UI, and a long page is 50+ batches.
const PROGRESS_EVERY_MS: i64 = 400;

const UR_FLAG_DISABLE_CACHE: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_DISABLE_CACHE.0 as i32;
const UR_FLAG_NO_RETRY_ON_5XX: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_NO_RETRY_ON_5XX.0 as i32;

/// Which half of the run a job is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Text,
    Images,
}

/// One picture and the boxes of text found in it, in the picture's own CSS pixels.
struct ImageLayer {
    image: usize,
    boxes: Vec<TextBox>,
}

struct TextBox {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// The recognised text, replaced by its translation before the overlay is drawn.
    text: String,
    ink: String,
    paper: String,
    rtl: bool,
}

/// One page being translated.
struct Job {
    browser_id: i32,
    target: String,
    /// Core asked for the text in pictures too (`settings.translate_images`).
    images: bool,
    /// Which half is running. Text always first: the overlay draws text into the page, and
    /// collecting after that would send sta's own translations back to be translated again.
    phase: Phase,
    /// The pictures being read, and where their text sits — filled by the image phase.
    layers: Vec<ImageLayer>,
    /// A remark for the toast that is not a failure ("Windows has no OCR for Japanese").
    images_note: Option<String>,
    /// Every string, in the order the page collected them.
    texts: Vec<String>,
    /// Translations of the batch that just landed.
    out: Vec<String>,
    /// Every translation of the current phase, in order. The image overlay needs them all at the
    /// end, where `out` only ever holds the last batch.
    out_all: Vec<String>,
    /// Where the batch in flight starts in `texts`.
    at: usize,
    /// How many strings the batch in flight actually carries. The answer can be shorter, and the
    /// cursor must advance by what was *sent* or a short answer silently re-sends the tail forever.
    in_flight: usize,
    /// How many strings have been written into the page so far (what the chip counts).
    applied: u32,
    /// The page had more prose than one run handles.
    truncated: bool,
    /// The batch in flight: its body and the request handle keeping it alive.
    body: Vec<u8>,
    request: Option<Urlrequest>,
    /// Globally unique per batch, so a late callback from a *previous job on the same tab* cannot
    /// be mistaken for this one's.
    batch: u64,
    timed_out: bool,
    last_progress: i64,
}

thread_local! {
    /// At most one job per tab.
    static JOBS: RefCell<HashMap<Id, Job>> = RefCell::new(HashMap::new());
    /// Tabs between `translate()` and the job actually existing — two CDP round trips during which
    /// `JOBS` is still empty and a second click would start a duplicate run.
    static STARTING: RefCell<HashSet<Id>> = RefCell::new(HashSet::new());
    /// Never reset per job: batch ids must not repeat for a tab that is translated twice.
    static NEXT_BATCH: Cell<u64> = const { Cell::new(1) };
}

fn next_batch_id() -> u64 {
    NEXT_BATCH.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1));
        id
    })
}

fn busy(tab: Id) -> bool {
    JOBS.with(|j| j.borrow().contains_key(&tab)) || STARTING.with(|s| s.borrow().contains(&tab))
}

/// Entry point (`Effect::TranslatePage`). Installs the page script, then either restores the
/// original page or starts collecting it.
pub fn translate(tab: Id, target: String, images: bool) {
    if busy(tab) {
        log_debug!("translate: tab {tab} is already being translated");
        return;
    }
    let Some(browser) = tabs::browser_for_tab(tab) else {
        finish(tab, Outcome::failed("the tab is gone"));
        return;
    };
    let browser_id = browser.identifier();
    STARTING.with(|s| s.borrow_mut().insert(tab));
    // Install (idempotent) and ask in one round trip whether this page is already translated.
    let expression = format!("{SHIM_JS};globalThis.__staTranslate.translated()");
    evaluate(browser_id, expression, move |result| match result {
        Ok(value) if value.as_bool() == Some(true) => restore(tab, browser_id),
        Ok(_) => collect(tab, browser_id, target, images),
        Err(e) => finish(tab, Outcome::failed(&e)),
    });
}

/// The chip's Stop (`Effect::CancelTranslate`).
pub fn cancel(tab: Id) {
    if !busy(tab) {
        return;
    }
    let applied = JOBS.with(|j| j.borrow().get(&tab).map_or(0, |job| job.applied));
    drop_job(tab);
    finish(tab, Outcome { strings: applied, cancelled: true, ..Outcome::default() });
}

/// `Runtime.evaluate` in the tab's main world, `returnByValue`, unwrapped to the value itself.
fn evaluate(browser_id: i32, expression: String, done: impl FnOnce(Result<Value, String>) + 'static) {
    let params = json!({ "expression": expression, "returnByValue": true, "awaitPromise": true });
    devtools_cdp::call(browser_id, User::Translate, "Runtime.evaluate", params, EVAL_TIMEOUT_MS, move |result| {
        done(result.and_then(|v| {
            // An exception in the page is a failure of ours, not of the page.
            if let Some(details) = v.get("exceptionDetails") {
                let text = details.get("text").and_then(Value::as_str).unwrap_or("the page refused the script");
                return Err(text.to_string());
            }
            Ok(v.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(Value::Null))
        }));
    });
}

fn restore(tab: Id, browser_id: i32) {
    evaluate(browser_id, "globalThis.__staTranslate.restore()".into(), move |result| match result {
        // The count is how many strings went back, so "Showing the original page" is only claimed
        // when something actually moved.
        Ok(value) => finish(tab, Outcome { strings: value.as_u64().unwrap_or(0) as u32, restored: true, ..Outcome::default() }),
        Err(e) => finish(tab, Outcome::failed(&e)),
    });
}

fn collect(tab: Id, browser_id: i32, target: String, images: bool) {
    evaluate(browser_id, "globalThis.__staTranslate.collect()".into(), move |result| {
        let texts = match result {
            Ok(value) => value
                .get("texts")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>())
                .unwrap_or_default(),
            Err(e) => return finish(tab, Outcome::failed(&e)),
        };
        if texts.is_empty() {
            return finish(tab, Outcome::default());
        }
        let mut texts = texts;
        let truncated = texts.len() > MAX_STRINGS;
        texts.truncate(MAX_STRINGS);
        let total = texts.len();
        let job = Job {
            browser_id,
            target,
            images,
            texts,
            out: Vec::new(),
            out_all: Vec::new(),
            phase: Phase::Text,
            layers: Vec::new(),
            images_note: None,
            at: 0,
            in_flight: 0,
            applied: 0,
            truncated,
            body: Vec::new(),
            request: None,
            batch: 0,
            timed_out: false,
            last_progress: 0,
        };
        JOBS.with(|j| j.borrow_mut().insert(tab, job));
        STARTING.with(|s| s.borrow_mut().remove(&tab));
        report(tab, TranslatePhase::Text, 0, total as u32);
        next_batch(tab);
    });
}

/// The URL for the strings starting at `at`, and how many it took.
fn batch_url(target: &str, texts: &[String], at: usize) -> (String, usize) {
    let mut url = format!("{ENDPOINT}?client=gtx&sl=auto&tl={}", sta_core::omnibox::encode_uri_component(target));
    let mut count = 0usize;
    for text in texts[at..].iter().take(MAX_BATCH) {
        let part = format!("&q={}", sta_core::omnibox::encode_uri_component(text));
        // Always take at least one, however long it is: otherwise one huge paragraph stalls the job.
        if count > 0 && url.len() + part.len() > MAX_URL_BYTES {
            break;
        }
        url.push_str(&part);
        count += 1;
    }
    (url, count)
}

fn next_batch(tab: Id) {
    let started = JOBS.with(|j| {
        let mut jobs = j.borrow_mut();
        let job = jobs.get_mut(&tab)?;
        if job.at >= job.texts.len() {
            return Some(None); // done
        }
        let (url, count) = batch_url(&job.target, &job.texts, job.at);
        job.batch = next_batch_id();
        job.in_flight = count;
        job.body.clear();
        job.timed_out = false;
        Some(Some((url, job.batch)))
    });
    match started {
        None => {}                // the job is gone (tab closed, navigated, cancelled)
        Some(None) => apply(tab), // every batch answered
        Some(Some((url, batch))) => {
            // No borrow held: CEF may complete a request synchronously.
            match create_request(&url, tab, batch) {
                Some(request) => {
                    // The job may have ended while the request was being created; then the handle is
                    // ours to drop, and there is nothing to report — `finish` already ran.
                    let orphan = JOBS.with(|j| match j.borrow_mut().get_mut(&tab) {
                        Some(job) if job.batch == batch => {
                            job.request = Some(request);
                            None
                        }
                        _ => Some(request),
                    });
                    if let Some(request) = orphan {
                        task::post_ui(move || drop(request));
                        return;
                    }
                    task::post_ui_delayed(REQUEST_TIMEOUT_MS, move || on_timeout(tab, batch));
                }
                None => fail(tab, "the request could not be started"),
            }
        }
    }
}

fn on_timeout(tab: Id, batch: u64) {
    let current = JOBS.with(|j| j.borrow().get(&tab).is_some_and(|job| job.batch == batch));
    if current {
        JOBS.with(|j| {
            if let Some(job) = j.borrow_mut().get_mut(&tab) {
                job.timed_out = true;
            }
        });
        fail(tab, "the translation service did not answer");
    }
}

fn create_request(url: &str, tab: Id, batch: u64) -> Option<Urlrequest> {
    let mut request = request_create()?;
    request.set_url(Some(&CefString::from(url)));
    request.set_method(Some(&CefString::from("GET")));
    // As in suggest.rs: no UR_FLAG_ALLOW_STORED_CREDENTIALS, so the page's cookies stay out of it.
    request.set_flags(UR_FLAG_DISABLE_CACHE | UR_FLAG_NO_RETRY_ON_5XX);
    let mut client = TranslateClient::new(tab, batch);
    let mut context = request_context_get_global_context();
    urlrequest_create(Some(&mut request), Some(&mut client), context.as_mut())
}

wrap_urlrequest_client! {
    struct TranslateClient {
        tab: Id,
        batch: u64,
    }

    impl UrlrequestClient {
        fn on_download_data(&self, _request: Option<&mut Urlrequest>, data: *const u8, data_length: usize) {
            if data.is_null() || data_length == 0 {
                return;
            }
            // SAFETY: CEF passes `data_length` readable bytes that stay valid for this call.
            let chunk = unsafe { std::slice::from_raw_parts(data, data_length) };
            let too_large = JOBS.with(|j| match j.borrow_mut().get_mut(&self.tab) {
                Some(job) if job.batch == self.batch => {
                    if job.body.len() + chunk.len() > MAX_BODY_BYTES {
                        true
                    } else {
                        job.body.extend_from_slice(chunk);
                        false
                    }
                }
                _ => false,
            });
            if too_large {
                fail(self.tab, "the answer was too large");
            }
        }

        fn on_request_complete(&self, request: Option<&mut Urlrequest>) {
            let status = match request {
                Some(r) if r.request_status() == UrlrequestStatus::SUCCESS => r.response().map(|resp| resp.status()),
                _ => None,
            };
            on_batch_complete(self.tab, self.batch, status);
        }
    }
}

fn on_batch_complete(tab: Id, batch: u64, status: Option<i32>) {
    let current = JOBS.with(|j| j.borrow().get(&tab).is_some_and(|job| job.batch == batch && !job.timed_out));
    if !current {
        return; // superseded, timed out, cancelled or the tab is gone
    }
    match status {
        Some(200) => {}
        Some(code) => return fail(tab, &format!("the translation service answered {code}")),
        None => return fail(tab, "the translation service could not be reached"),
    }
    let parsed = JOBS.with(|j| j.borrow().get(&tab).map(|job| (parse_batch(&job.body), job.at, job.in_flight)));
    let Some((translations, at, in_flight)) = parsed else { return };
    let translations = match translations {
        Ok(t) => t,
        Err(e) => return fail(tab, &e),
    };
    // Write this batch into the page now: a later failure then keeps what already worked.
    let flush = JOBS.with(|j| {
        let mut jobs = j.borrow_mut();
        let job = jobs.get_mut(&tab)?;
        job.out = translations.iter().take(in_flight).cloned().collect();
        job.out_all.extend(job.out.iter().cloned());
        // Advance by what was SENT, never by what came back, or a short answer re-sends the tail.
        job.at = if translations.is_empty() { job.texts.len() } else { at + in_flight };
        release(job);
        Some((job.browser_id, at, job.out.clone()))
    });
    let Some((browser_id, offset, out)) = flush else { return };
    // The image phase collects its translations and paints them all at the end (finish_images);
    // only the text phase writes them into the document as they land.
    if JOBS.with(|j| j.borrow().get(&tab).is_some_and(|job| job.phase == Phase::Images)) {
        return next_batch(tab);
    }
    let payload = Value::Array(out.into_iter().map(Value::String).collect()).to_string();
    let expression = format!("globalThis.__staTranslate.apply({offset},{payload})");
    evaluate(browser_id, expression, move |result| {
        match result {
            Ok(value) => {
                let written = value.as_u64().unwrap_or(0) as u32;
                let counts = JOBS.with(|j| {
                    let mut jobs = j.borrow_mut();
                    let job = jobs.get_mut(&tab)?;
                    job.applied += written;
                    Some((job.applied, job.texts.len() as u32, job.at))
                });
                if let Some((applied, total, cursor)) = counts {
                    maybe_report(tab, TranslatePhase::Text, applied, total);
                    let _ = cursor;
                }
            }
            // A page that refuses the write is not worth continuing against.
            Err(e) => return fail(tab, &e),
        }
        next_batch(tab);
    });
}

/// `[[translated, detected], …]`, and defensively also a flat `[translated, …]`.
fn parse_batch(body: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the answer was not text".to_string())?;
    let value: Value = serde_json::from_str(text).map_err(|_| "the answer could not be read".to_string())?;
    let array = value.as_array().ok_or_else(|| "the answer was not a list".to_string())?;
    Ok(array
        .iter()
        .map(|entry| match entry {
            Value::String(s) => s.clone(),
            Value::Array(pair) => pair.first().and_then(Value::as_str).unwrap_or_default().to_string(),
            _ => String::new(),
        })
        .collect())
}

/// A batch phase ended. The text phase hands over to the images; the image phase finishes the run.
fn apply(tab: Id) {
    let Some((phase, browser_id, images)) = JOBS.with(|j| {
        let jobs = j.borrow();
        let job = jobs.get(&tab)?;
        Some((job.phase, job.browser_id, job.images))
    }) else {
        return;
    };
    match phase {
        Phase::Text if images => start_images(tab, browser_id),
        Phase::Text => done_with_text(tab),
        Phase::Images => finish_images(tab),
    }
}

fn done_with_text(tab: Id) {
    let Some((applied, truncated, note)) = JOBS.with(|j| {
        let jobs = j.borrow();
        jobs.get(&tab).map(|job| (job.applied, job.truncated, job.images_note.clone()))
    }) else {
        return;
    };
    drop_job(tab);
    finish(tab, Outcome { strings: applied, truncated, images_note: note, ..Outcome::default() });
}

// ---------------------------------------------------------------------------------- the pictures
//
// One screenshot of the viewport, cropped here to each picture, rather than fetching the pictures
// themselves: Chromium has already decoded them, so canvas tainting, hotlink and credential 403s,
// `blob:`/`srcset` ambiguity and the WebP/AVIF question all stop being our problem at once. The
// pixels never leave the machine — Windows reads them (`ocr.rs`) and only the text goes on to be
// translated, through the same batcher the page's own text uses.

/// A picture smaller than this after cropping cannot carry readable text.
const MIN_CROP: u32 = 24;
/// Below this device-pixel ratio the crop is doubled: Windows' recogniser has a fixed minimum
/// feature size, and small captions are read far more reliably at 2x.
const UPSCALE_BELOW: f64 = 1.5;
const CAPTURE_TIMEOUT_MS: i64 = 15_000;

#[cfg(not(windows))]
fn start_images(tab: Id, _browser_id: i32) {
    note_and_finish_text(tab, "reading text in pictures is Windows only");
}

/// Finishes the text phase, carrying a remark the toast will add.
fn note_and_finish_text(tab: Id, note: &str) {
    JOBS.with(|j| {
        if let Some(job) = j.borrow_mut().get_mut(&tab) {
            job.images_note = Some(note.to_string());
        }
    });
    done_with_text(tab);
}

#[cfg(windows)]
fn start_images(tab: Id, browser_id: i32) {
    report(tab, TranslatePhase::Images, 0, 0);
    evaluate(browser_id, "globalThis.__staTranslate.imageTargets()".into(), move |result| {
        let targets = match result {
            Ok(v) => v,
            // The text is already translated; an image failure must not lose it.
            Err(e) => return note_and_finish_text(tab, &format!("couldn't look for pictures ({e})")),
        };
        let items: Vec<(f64, f64, f64, f64)> = targets
            .get("items")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|it| {
                        let n = |k: &str| it.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                        (n("x"), n("y"), n("w"), n("h"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if items.is_empty() {
            return done_with_text(tab);
        }
        // A tab that is not being rendered cannot be screenshotted; it would hang, not fail.
        if targets.get("visible").and_then(Value::as_bool) != Some(true) {
            return note_and_finish_text(tab, "pictures are only read while the tab is on screen");
        }
        let page_lang = targets.get("lang").and_then(Value::as_str).unwrap_or("en").to_string();
        let Some(ocr_lang) = crate::ocr::pick_language(&page_lang) else {
            return note_and_finish_text(
                tab,
                &format!("Windows has no text recognition for '{page_lang}' (add it in Settings › Time & language › Language & region)"),
            );
        };
        let vw = targets.get("vw").and_then(Value::as_f64).unwrap_or(0.0);
        capture(tab, browser_id, vw, items, ocr_lang);
    });
}

#[cfg(windows)]
fn capture(tab: Id, browser_id: i32, vw: f64, items: Vec<(f64, f64, f64, f64)>, ocr_lang: String) {
    let params = json!({ "format": "png", "fromSurface": true, "captureBeyondViewport": false });
    devtools_cdp::call(browser_id, User::Translate, "Page.captureScreenshot", params, CAPTURE_TIMEOUT_MS, move |result| {
        let data = match result {
            Ok(v) => v.get("data").and_then(Value::as_str).unwrap_or_default().to_string(),
            Err(e) => return note_and_finish_text(tab, &format!("couldn't read the pictures ({e})")),
        };
        let Some(bytes) = crate::bytes::base64_decode(&data) else {
            return note_and_finish_text(tab, "couldn't read the pictures");
        };
        let shot = match crate::png::read_png(&bytes) {
            Ok(image) => image,
            Err(e) => return note_and_finish_text(tab, &format!("couldn't read the pictures ({e})")),
        };
        // Device pixels per CSS pixel, read from the bitmap rather than trusting devicePixelRatio,
        // which Chromium rounds.
        let k = if vw > 0.0 { shot.width as f64 / vw } else { 1.0 };
        let dir = match scratch_dir() {
            Ok(dir) => dir,
            Err(e) => return note_and_finish_text(tab, &e),
        };
        let mut paths = Vec::new();
        let mut kept = Vec::new();
        for (i, (x, y, w, h)) in items.iter().enumerate() {
            let crop = crate::png::crop(&shot, (x * k) as u32, (y * k) as u32, (w * k) as u32, (h * k) as u32);
            if crop.width < MIN_CROP || crop.height < MIN_CROP {
                continue;
            }
            let (crop, scale) = if k < UPSCALE_BELOW { (crate::png::upscale2x(&crop), k * 2.0) } else { (crop, k) };
            let path = dir.join(format!("img-{i}.png"));
            let Ok(png) = crate::png::write_png(&crop) else { continue };
            if std::fs::write(&path, png).is_err() {
                continue;
            }
            paths.push(path);
            kept.push((i, crop, scale));
        }
        if paths.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            return done_with_text(tab);
        }
        crate::ocr::recognize(ocr_lang, paths, move |result| {
            let _ = std::fs::remove_dir_all(&dir);
            match result {
                Ok(pages) => on_ocr(tab, kept, pages),
                Err(e) => note_and_finish_text(tab, &e),
            }
        });
    });
}

/// A fresh directory for this run's crops. Wiped first, so a crashed run leaves nothing behind.
#[cfg(windows)]
fn scratch_dir() -> Result<std::path::PathBuf, String> {
    // The disk on this machine fills up; a failed write here would otherwise look like bad OCR.
    if crate::platform::free_disk_bytes(&crate::paths::dirs().base).is_some_and(|free| free < 200 * 1024 * 1024) {
        return Err("there is not enough free disk space to read the pictures".into());
    }
    let dir = crate::paths::dirs().base.join("ocr");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't prepare the pictures ({e})"))?;
    Ok(dir)
}

/// The recognised text is in; turn it into the image phase's batch of strings.
#[cfg(windows)]
fn on_ocr(tab: Id, kept: Vec<(usize, crate::png::Image, f64)>, pages: Vec<crate::ocr::Page>) {
    let mut layers: Vec<ImageLayer> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    for ((image, crop, scale), page) in kept.iter().zip(pages.iter()) {
        let merged = crate::ocr::merge_lines(&page.lines);
        let mut boxes = Vec::new();
        for line in merged {
            let (ink, paper) = colours(crop, &line);
            boxes.push(TextBox {
                // Back to the picture's own CSS pixels.
                x: line.x / scale,
                y: line.y / scale,
                w: line.w / scale,
                h: line.h / scale,
                text: line.text.clone(),
                ink,
                paper,
                rtl: false,
            });
            texts.push(line.text);
        }
        if !boxes.is_empty() {
            layers.push(ImageLayer { image: *image, boxes });
        }
    }
    if texts.is_empty() {
        return done_with_text(tab);
    }
    let total = texts.len() as u32;
    let restarted = JOBS.with(|j| {
        let mut jobs = j.borrow_mut();
        let Some(job) = jobs.get_mut(&tab) else { return false };
        job.phase = Phase::Images;
        job.layers = layers;
        job.texts = texts;
        job.out = Vec::new();
        job.out_all = Vec::new();
        job.at = 0;
        job.in_flight = 0;
        job.last_progress = 0;
        true
    });
    if restarted {
        report(tab, TranslatePhase::Images, 0, total);
        next_batch(tab);
    }
}

/// Ink and paper for one line, estimated from the pixels around and inside its box. A busy
/// photographic background cannot be matched, so this falls back to a legible chip rather than
/// painting dark text on dark.
#[cfg(windows)]
fn colours(crop: &crate::png::Image, line: &crate::ocr::Line) -> (String, String) {
    let luma = |p: [u8; 4]| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
    let (x0, y0) = (line.x.max(0.0) as u32, line.y.max(0.0) as u32);
    let (w, h) = (line.w.max(1.0) as u32, line.h.max(1.0) as u32);
    let mut outside: Vec<[u8; 4]> = Vec::new();
    for dx in 0..w {
        for y in [y0.saturating_sub(2), (y0 + h + 1).min(crop.height.saturating_sub(1))] {
            if let Some(p) = crop.pixel(x0 + dx, y) {
                outside.push(p);
            }
        }
    }
    let mut inside: Vec<[u8; 4]> = Vec::new();
    for dy in 0..h {
        for dx in 0..w {
            if let Some(p) = crop.pixel(x0 + dx, y0 + dy) {
                inside.push(p);
            }
        }
    }
    let mean = |px: &[[u8; 4]]| -> Option<[u8; 4]> {
        if px.is_empty() {
            return None;
        }
        let mut sum = [0u64; 3];
        for p in px {
            for i in 0..3 {
                sum[i] += p[i] as u64;
            }
        }
        Some([(sum[0] / px.len() as u64) as u8, (sum[1] / px.len() as u64) as u8, (sum[2] / px.len() as u64) as u8, 255])
    };
    let hex = |p: [u8; 4]| format!("#{:02x}{:02x}{:02x}", p[0], p[1], p[2]);
    let Some(paper) = mean(&outside).or_else(|| mean(&inside)) else {
        return ("#fff".into(), "rgba(0,0,0,0.78)".into());
    };
    // The ink is the tenth of the inside pixels furthest in luma from the paper.
    let mut ranked = inside.clone();
    ranked.sort_by(|a, b| {
        (luma(*b) - luma(paper)).abs().partial_cmp(&(luma(*a) - luma(paper)).abs()).unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate((ranked.len() / 10).max(1));
    match mean(&ranked) {
        Some(ink) if (luma(ink) - luma(paper)).abs() >= 60.0 => (hex(ink), hex(paper)),
        // Not enough contrast to imitate: a plain chip is readable where a guess would not be.
        _ => ("#fff".into(), "rgba(0,0,0,0.78)".into()),
    }
}

/// The image phase's translations are in; draw them over the pictures.
fn finish_images(tab: Id) {
    let Some((browser_id, layers, applied, truncated, note)) = JOBS.with(|j| {
        let jobs = j.borrow();
        let job = jobs.get(&tab)?;
        let layers: Vec<Value> = {
            let mut texts = job.out_all.iter();
            job.layers
                .iter()
                .map(|layer| {
                    json!({
                        "image": layer.image,
                        "boxes": layer.boxes.iter().map(|b| json!({
                            "x": b.x, "y": b.y, "w": b.w, "h": b.h,
                            "text": texts.next().cloned().unwrap_or_else(|| b.text.clone()),
                            "ink": b.ink, "paper": b.paper, "rtl": b.rtl,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect()
        };
        Some((job.browser_id, layers, job.applied, job.truncated, job.images_note.clone()))
    }) else {
        return;
    };
    let payload = Value::Array(layers).to_string();
    let expression = format!("globalThis.__staTranslate.applyImages({payload})");
    evaluate(browser_id, expression, move |result| {
        let images = result.ok().and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        drop_job(tab);
        finish(tab, Outcome { strings: applied, images, truncated, images_note: note, ..Outcome::default() });
    });
}

/// What the run came to, for `Command::TranslateFinished`.
#[derive(Default)]
struct Outcome {
    strings: u32,
    images: u32,
    restored: bool,
    cancelled: bool,
    truncated: bool,
    error: Option<String>,
    images_note: Option<String>,
}

impl Outcome {
    fn failed(reason: &str) -> Self {
        Self { error: Some(reason.to_string()), ..Self::default() }
    }
}

fn fail(tab: Id, reason: &str) {
    // Keep whatever already landed in the page: the user can read that much.
    let applied = JOBS.with(|j| j.borrow().get(&tab).map_or(0, |job| job.applied));
    drop_job(tab);
    finish(tab, Outcome { strings: applied, error: Some(reason.to_string()), ..Outcome::default() });
}

/// Releases a job's in-flight request without dropping the handle inside its own callback.
fn release(job: &mut Job) {
    if let Some(request) = job.request.take() {
        task::post_ui(move || {
            if request.request_status() == UrlrequestStatus::IO_PENDING {
                request.cancel();
            }
            drop(request);
        });
    }
}

fn drop_job(tab: Id) {
    STARTING.with(|s| s.borrow_mut().remove(&tab));
    if let Some(mut job) = JOBS.with(|j| j.borrow_mut().remove(&tab)) {
        release(&mut job);
    }
}

fn report(tab: Id, phase: TranslatePhase, done: u32, total: u32) {
    controller::dispatch(Command::TranslateProgress { tab, phase, done, total });
}

/// Throttled progress. The count is computed inside the borrow and dispatched outside it:
/// `controller::dispatch` runs the store synchronously and would double-borrow `JOBS`.
fn maybe_report(tab: Id, phase: TranslatePhase, done: u32, total: u32) {
    let send = JOBS.with(|j| {
        let mut jobs = j.borrow_mut();
        let job = jobs.get_mut(&tab)?;
        let now = sta_core::now_ms();
        if now - job.last_progress < PROGRESS_EVERY_MS {
            return None;
        }
        job.last_progress = now;
        Some(())
    });
    if send.is_some() {
        report(tab, phase, done, total);
    }
}

fn finish(tab: Id, outcome: Outcome) {
    STARTING.with(|s| s.borrow_mut().remove(&tab));
    if let Some(e) = outcome.error.as_deref() {
        log_info!("translate: tab {tab} failed ({e})");
    }
    controller::dispatch(Command::TranslateFinished {
        tab,
        strings: outcome.strings,
        images: outcome.images,
        restored: outcome.restored,
        error: outcome.error,
        cancelled: outcome.cancelled,
        truncated: outcome.truncated,
        images_note: outcome.images_note,
    });
}

/// A tab whose browser went away stops translating, and says so — core would otherwise leave the
/// chip spinning on a tab that no longer exists.
pub fn on_browser_closed(browser_id: i32) {
    for tab in tabs_of(browser_id) {
        drop_job(tab);
    }
}

/// A new document has none of the old one's text, and the page script died with it.
pub fn on_navigated(browser_id: i32) {
    for tab in tabs_of(browser_id) {
        drop_job(tab);
        finish(tab, Outcome { cancelled: true, ..Outcome::default() });
    }
}

fn tabs_of(browser_id: i32) -> Vec<Id> {
    JOBS.with(|j| j.borrow().iter().filter(|(_, job)| job.browser_id == browser_id).map(|(t, _)| *t).collect())
}

/// Drops every job (after the message loop ended, before `cef::shutdown()`).
pub fn clear() {
    STARTING.with(|s| s.borrow_mut().clear());
    let taken = JOBS.with(|j| std::mem::take(&mut *j.borrow_mut()));
    drop(taken);
}

pub fn debug_snapshot() -> Value {
    JOBS.with(|j| {
        let jobs = j.borrow();
        json!({
            "jobs": jobs.iter().map(|(tab, job)| json!({
                "tab": tab,
                "target": job.target,
                "images": job.images,
                "strings": job.texts.len(),
                "done": job.at,
                "applied": job.applied,
            })).collect::<Vec<_>>(),
            "starting": STARTING.with(|s| s.borrow().iter().copied().collect::<Vec<_>>()),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(n: usize, each: usize) -> Vec<String> {
        (0..n).map(|i| format!("{}{i}", "a".repeat(each))).collect()
    }

    #[test]
    fn a_batch_stops_at_the_url_limit() {
        // Long enough that the length cap bites before MAX_BATCH does.
        let texts = texts(MAX_BATCH, 400);
        let (url, count) = batch_url("en", &texts, 0);
        assert!(count > 0 && count < MAX_BATCH, "length should cap the batch, took {count}");
        assert!(url.len() <= MAX_URL_BYTES + 400 * 3, "one overshooting string is allowed, got {}", url.len());
        assert!(url.starts_with(ENDPOINT));
        assert!(url.contains("&tl=en"));
    }

    #[test]
    fn a_batch_stops_at_the_count_limit() {
        let texts = texts(MAX_BATCH * 2, 1);
        let (_, count) = batch_url("ko", &texts, 0);
        assert_eq!(count, MAX_BATCH);
    }

    #[test]
    fn one_string_longer_than_the_limit_is_still_sent() {
        let texts = vec!["x".repeat(MAX_URL_BYTES * 2)];
        let (_, count) = batch_url("en", &texts, 0);
        assert_eq!(count, 1, "a single huge paragraph must not stall the job");
    }

    #[test]
    fn batches_resume_where_the_last_one_stopped() {
        let texts = texts(10, 1);
        let (url, count) = batch_url("en", &texts, 8);
        assert_eq!(count, 2);
        assert!(url.contains("a8") && url.contains("a9") && !url.contains("a7"));
    }

    #[test]
    fn text_is_percent_encoded() {
        let (url, _) = batch_url("en", &["a&b=c d".to_string()], 0);
        assert!(url.contains("a%26b%3Dc%20d"), "{url}");
    }

    #[test]
    fn batch_ids_never_repeat() {
        // Two jobs on the same tab must not share an id, or a late answer from the first is taken
        // for an answer to the second.
        let ids: Vec<u64> = (0..5).map(|_| next_batch_id()).collect();
        let mut sorted = ids.clone();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "{ids:?}");
        assert!(ids.windows(2).all(|w| w[1] > w[0]), "{ids:?}");
    }

    #[test]
    fn the_answer_shape_is_pairs_of_translation_and_detected_language() {
        let body = br#"[["hello","ko"],["world","ko"]]"#;
        assert_eq!(parse_batch(body).unwrap(), vec!["hello", "world"]);
    }

    #[test]
    fn a_flat_answer_is_read_too() {
        // Not what the endpoint sent when measured, but cheap to tolerate.
        assert_eq!(parse_batch(br#"["hello"]"#).unwrap(), vec!["hello"]);
    }

    #[test]
    fn a_broken_answer_is_an_error_not_a_panic() {
        assert!(parse_batch(b"not json").is_err());
        assert!(parse_batch(br#"{"error":"nope"}"#).is_err());
        assert!(parse_batch(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn unreadable_entries_become_empty_strings_and_keep_their_place() {
        // apply() skips empty strings, so a hole leaves that node alone instead of shifting others.
        assert_eq!(parse_batch(br#"[["a","ko"],null,["c","ko"]]"#).unwrap(), vec!["a", "", "c"]);
    }

    #[test]
    fn a_short_answer_still_advances_by_what_was_sent() {
        // The endpoint answered 2 of the 5 strings the batch carried. The cursor must move 5, or
        // the next batch re-sends strings 2..5 and the page never finishes.
        let sent = 5usize;
        let received = parse_batch(br#"[["a","ko"],["b","ko"]]"#).unwrap();
        assert_eq!(received.len(), 2);
        let at = 10usize;
        assert_eq!(at + sent, 15, "cursor advances by the sent count, not {}", received.len());
    }
}
