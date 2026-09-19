//! "Translate page" [owner: tabs] (context menu → `Command::TranslatePage`).
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
//! 3. The translations go back in the same order, and the page puts them where they came from.
//!
//! Running it again on a page that is already translated **restores the original** instead, so the
//! single menu item is both ways. The page is the record of which of the two will happen; core
//! only says which language to aim for, and hears the outcome as `Command::TranslateFinished`.
//!
//! The endpoint is Google's free `translate_a/t`, the one that takes repeated `q` parameters and
//! answers `[[translated, detected], …]` in the same order. That is a GET with everything in the
//! URL, so batches are capped by URL length as much as by count. **The text of the page leaves the
//! machine** — that is the deal this feature makes, and the only reason it needs no API key.
//!
//! Public API:
//! - `pub fn translate(tab: Id, target: String)`
//! - `pub fn on_browser_closed(browser_id: i32)`, `pub fn clear()`, `pub fn debug_snapshot()`

use crate::devtools_cdp::{self, User};
use crate::{controller, tabs, task};
use cef::rc::Rc as _;
use cef::*;
use serde_json::{Value, json};
use sta_core::{Command, Id};
use std::cell::RefCell;
use std::collections::HashMap;

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
/// A page with more prose than this is translated down to here rather than refused.
const MAX_STRINGS: usize = 4000;

const UR_FLAG_DISABLE_CACHE: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_DISABLE_CACHE.0 as i32;
const UR_FLAG_NO_RETRY_ON_5XX: i32 = sys::cef_urlrequest_flags_t::UR_FLAG_NO_RETRY_ON_5XX.0 as i32;

/// One page being translated. Batches run one at a time: the endpoint is free and unofficial, and
/// a page's worth of parallel requests is exactly what gets an IP rate-limited.
struct Job {
    browser_id: i32,
    target: String,
    /// Every string, in the order the page collected them.
    texts: Vec<String>,
    /// Translations so far, same order and length as `texts` once done.
    out: Vec<String>,
    /// Where the batch in flight starts in `texts`.
    at: usize,
    /// The batch in flight: its body and the request handle keeping it alive.
    body: Vec<u8>,
    request: Option<Urlrequest>,
    /// Bumped per batch so a late callback from a cancelled one is ignored.
    batch: u64,
    timed_out: bool,
}

thread_local! {
    /// At most one job per tab; a second Translate on a busy tab is ignored, not queued.
    static JOBS: RefCell<HashMap<Id, Job>> = RefCell::new(HashMap::new());
}

/// Entry point (`Effect::TranslatePage`). Installs the page script, then either restores the
/// original page or starts collecting it.
pub fn translate(tab: Id, target: String) {
    if JOBS.with(|j| j.borrow().contains_key(&tab)) {
        log_debug!("translate: tab {tab} is already being translated");
        return;
    }
    let Some(browser) = tabs::browser_for_tab(tab) else {
        finish(tab, 0, 0, false, Some("the tab is gone".into()));
        return;
    };
    let browser_id = browser.identifier();
    // Install (idempotent) and ask in one round trip whether this page is already translated.
    let expression = format!("{SHIM_JS};globalThis.__staTranslate.translated()");
    evaluate(browser_id, expression, move |result| match result {
        Ok(value) if value.as_bool() == Some(true) => restore(tab, browser_id),
        Ok(_) => collect(tab, browser_id, target),
        Err(e) => finish(tab, 0, 0, false, Some(e)),
    });
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
        Ok(_) => finish(tab, 0, 0, true, None),
        Err(e) => finish(tab, 0, 0, false, Some(e)),
    });
}

fn collect(tab: Id, browser_id: i32, target: String) {
    evaluate(browser_id, "globalThis.__staTranslate.collect()".into(), move |result| {
        let texts = match result {
            Ok(value) => value
                .get("texts")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>())
                .unwrap_or_default(),
            Err(e) => return finish(tab, 0, 0, false, Some(e)),
        };
        if texts.is_empty() {
            return finish(tab, 0, 0, false, None);
        }
        let mut texts = texts;
        texts.truncate(MAX_STRINGS);
        let out = vec![String::new(); texts.len()];
        let job = Job {
            browser_id,
            target,
            texts,
            out,
            at: 0,
            body: Vec::new(),
            request: None,
            batch: 0,
            timed_out: false,
        };
        JOBS.with(|j| j.borrow_mut().insert(tab, job));
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
        let Some(job) = jobs.get_mut(&tab) else { return None };
        if job.at >= job.texts.len() {
            return Some(None); // done
        }
        let (url, count) = batch_url(&job.target, &job.texts, job.at);
        job.batch += 1;
        job.body.clear();
        job.timed_out = false;
        Some(Some((url, count, job.batch, job.browser_id)))
    });
    match started {
        None => {}                        // the job is gone (tab closed)
        Some(None) => apply(tab),         // every batch answered
        Some(Some((url, count, batch, browser_id))) => {
            let request = create_request(&url, tab, batch);
            let ok = JOBS.with(|j| {
                let mut jobs = j.borrow_mut();
                let Some(job) = jobs.get_mut(&tab) else { return false };
                job.request = request;
                job.request.is_some()
            });
            if !ok {
                return fail(tab, "the request could not be started".into());
            }
            let _ = (count, browser_id);
            task::post_ui_delayed(REQUEST_TIMEOUT_MS, move || on_timeout(tab, batch));
        }
    }
}

fn on_timeout(tab: Id, batch: u64) {
    let stale = JOBS.with(|j| j.borrow().get(&tab).is_some_and(|job| job.batch == batch));
    if stale {
        JOBS.with(|j| {
            if let Some(job) = j.borrow_mut().get_mut(&tab) {
                job.timed_out = true;
            }
        });
        fail(tab, "the translation service did not answer".into());
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
                fail(self.tab, "the answer was too large".into());
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
        return; // superseded, timed out or the tab is gone
    }
    match status {
        Some(200) => {}
        Some(code) => return fail(tab, format!("the translation service answered {code}")),
        None => return fail(tab, "the translation service could not be reached".into()),
    }
    let parsed = JOBS.with(|j| {
        let jobs = j.borrow();
        jobs.get(&tab).map(|job| (parse_batch(&job.body), job.at))
    });
    let Some((translations, at)) = parsed else { return };
    let translations = match translations {
        Ok(t) => t,
        Err(e) => return fail(tab, e),
    };
    let done = JOBS.with(|j| {
        let mut jobs = j.borrow_mut();
        let Some(job) = jobs.get_mut(&tab) else { return false };
        for (i, text) in translations.iter().enumerate() {
            if let Some(slot) = job.out.get_mut(at + i) {
                *slot = text.clone();
            }
        }
        // An empty answer would loop forever; treat it as the end of what we can do.
        job.at = if translations.is_empty() { job.texts.len() } else { at + translations.len() };
        release(job);
        true
    });
    if done {
        next_batch(tab);
    }
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

/// Hands the translations to the page and reports how many landed.
fn apply(tab: Id) {
    let Some((browser_id, out)) = JOBS.with(|j| j.borrow().get(&tab).map(|job| (job.browser_id, job.out.clone()))) else {
        return;
    };
    let payload = Value::Array(out.into_iter().map(Value::String).collect()).to_string();
    let expression = format!("globalThis.__staTranslate.apply({payload})");
    evaluate(browser_id, expression, move |result| {
        drop_job(tab);
        match result {
            Ok(value) => {
                let strings = value.as_u64().unwrap_or(0) as u32;
                finish(tab, strings, 0, false, None);
            }
            Err(e) => finish(tab, 0, 0, false, Some(e)),
        }
    });
}

fn fail(tab: Id, reason: String) {
    drop_job(tab);
    finish(tab, 0, 0, false, Some(reason));
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
    if let Some(mut job) = JOBS.with(|j| j.borrow_mut().remove(&tab)) {
        release(&mut job);
    }
}

fn finish(tab: Id, strings: u32, images: u32, restored: bool, error: Option<String>) {
    if let Some(e) = error.as_deref() {
        log_info!("translate: tab {tab} failed ({e})");
    }
    controller::dispatch(Command::TranslateFinished { tab, strings, images, restored, error });
}

/// A tab whose browser went away stops translating.
pub fn on_browser_closed(browser_id: i32) {
    let tabs: Vec<Id> = JOBS.with(|j| j.borrow().iter().filter(|(_, job)| job.browser_id == browser_id).map(|(t, _)| *t).collect());
    for tab in tabs {
        drop_job(tab);
    }
}

/// Drops every job (after the message loop ended, before `cef::shutdown()`).
pub fn clear() {
    let taken = JOBS.with(|j| std::mem::take(&mut *j.borrow_mut()));
    drop(taken);
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
}

pub fn debug_snapshot() -> Value {
    JOBS.with(|j| {
        let jobs = j.borrow();
        json!({
            "jobs": jobs.iter().map(|(tab, job)| json!({
                "tab": tab,
                "target": job.target,
                "strings": job.texts.len(),
                "done": job.at,
            })).collect::<Vec<_>>(),
        })
    })
}
