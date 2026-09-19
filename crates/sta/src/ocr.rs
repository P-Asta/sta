//! Reading the text in a picture, with Windows' own recogniser [owner: tabs] (`translate.rs`).
//!
//! `Windows.Media.Ocr` is a WinRT API, and sta depends on `windows-sys` — raw FFI with no WinRT
//! projection. Declaring the interfaces by hand is possible but is a large slab of vtable and
//! `IAsyncOperation` work, and pulling in the `windows` crate is not an option: a new dependency of
//! `sta` re-runs `cef-dll-sys`'s build script, which needs cmake and ninja (docs/RELEASING.md).
//! So the recogniser is reached the way the rest of this repo reaches Windows-only odds and ends —
//! a PowerShell script (`ocr.ps1`, embedded with `include_str!`) run as a child process.
//!
//! It runs **once for every batch of images, not once per image**: process start is ~250 ms of a
//! ~270 ms call, so 20 images cost ~0.4 s batched against ~5.5 s one process each (measured).
//! Paths go in on stdin, one per line; strict JSON comes back on stdout.
//!
//! Everything here happens on a worker thread and returns through [`task::post_ui_from_any_thread`]:
//! CEF's UI thread must never wait on a process.
//!
//! The pictures never leave the machine — only the text found in them, which then goes to the same
//! translation endpoint as the page's own text.
//!
//! Public API:
//! - `pub fn languages() -> Vec<String>` (cached), `pub fn pick_language(page: &str) -> Option<String>`
//! - `pub fn recognize(lang: String, paths: Vec<PathBuf>, done: impl FnOnce(Result<Vec<Page>, String>))`
//! - `pub struct Page { lines: Vec<Line> }`, `pub struct Line { text, x, y, w, h }`

#![cfg(windows)]

use crate::task;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SCRIPT: &str = include_str!("ocr.ps1");
/// A batch of a dozen small crops is a few hundred ms; this is the "something is wrong" bound.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);
const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;

/// One line of recognised text and where it sits, in the pixels of the image it came from.
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The lines found in one image, in the order the paths were handed over.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub lines: Vec<Line>,
}

/// Where the script is materialised (it has to be a real file for `-File`).
fn script_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = crate::paths::dirs().base.join("ocr.ps1");
        // Rewrite whenever it differs, so an upgraded sta never runs the old script.
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() != Some(SCRIPT)
            && let Err(e) = std::fs::write(&path, SCRIPT)
        {
            log_warn!("ocr: cannot write {} ({e})", path.display());
            return None;
        }
        Some(path)
    })
    .as_deref()
}

/// `powershell.exe` by absolute path — never off `PATH`, which the page cannot influence but the
/// environment can.
fn powershell() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32\\WindowsPowerShell\\v1.0\\powershell.exe")
}

fn run(args: &[&str], stdin_text: Option<&str>) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let Some(script) = script_path() else { return Err("the OCR helper could not be written".into()) };
    let mut command = Command::new(powershell());
    command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .args(args)
        .stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|e| format!("Windows OCR could not be started ({e})"))?;
    if let Some(text) = stdin_text
        && let Some(mut stdin) = child.stdin.take()
    {
        // Dropped here on purpose: the script reads to end-of-stream before it answers.
        let _ = stdin.write_all(text.as_bytes());
    }
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(std::time::Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Windows OCR did not answer".into());
            }
            Err(e) => return Err(format!("Windows OCR failed ({e})")),
        }
    };
    let mut out = String::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(MAX_OUTPUT_BYTES).read_to_string(&mut out);
    }
    if out.trim().is_empty() {
        // The script always answers, even to say it failed. Silence with a non-zero exit is the
        // shape Group Policy leaves when it refuses to run scripts at all.
        return Err(if status.success() { "Windows OCR answered nothing".into() } else { "Windows can't run the OCR helper on this machine".into() });
    }
    Ok(out)
}

/// Which recognisers Windows has installed, lower-cased BCP-47 tags. Cached: it costs a process.
pub fn languages() -> &'static [String] {
    static LANGS: OnceLock<Vec<String>> = OnceLock::new();
    LANGS.get_or_init(|| {
        let text = match run(&["-ListLanguages"], None) {
            Ok(text) => text,
            Err(e) => {
                log_info!("ocr: cannot list languages ({e})");
                return Vec::new();
            }
        };
        let value: Value = serde_json::from_str(text.trim()).unwrap_or(Value::Null);
        let list: Vec<String> = value
            .get("available")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.to_ascii_lowercase()).collect())
            .unwrap_or_default();
        log_info!("ocr: recognizers installed: {}", if list.is_empty() { "none".to_string() } else { list.join(", ") });
        list
    })
}

/// The installed recogniser for the page's language, matching on the base subtag (`ko` matches
/// `ko-KR`). `None` means Windows cannot read this language and the image phase must be skipped —
/// never silently fall back to another recogniser, which would read English with a Korean model.
pub fn pick_language(page: &str) -> Option<String> {
    let page = page.to_ascii_lowercase();
    let base = page.split(['-', '_']).next().unwrap_or(&page);
    let installed = languages();
    installed
        .iter()
        .find(|tag| *tag == &page)
        .or_else(|| installed.iter().find(|tag| tag.split(['-', '_']).next() == Some(base)))
        .cloned()
}

/// Reads every image, off the UI thread. `done` runs back on the UI thread with one [`Page`] per
/// path, in the same order.
pub fn recognize(lang: String, paths: Vec<PathBuf>, done: impl FnOnce(Result<Vec<Page>, String>) + Send + 'static) {
    // Shared so that a thread which never starts still answers: a caller that is left waiting
    // forever would strand the chip in "Reading images".
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(done)));
    let worker = slot.clone();
    let spawned = std::thread::Builder::new().name("sta-ocr".into()).spawn(move || {
        let stdin: String = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        let result = run(&["-Batch", "-Lang", &lang], Some(&stdin)).and_then(|text| parse_batch(&text, paths.len()));
        task::post_ui_from_any_thread(move || {
            if let Some(done) = worker.lock().ok().and_then(|mut s| s.take()) {
                done(result);
            }
        });
    });
    if let Err(e) = spawned {
        log_warn!("ocr: cannot start the worker thread ({e})");
        if let Some(done) = slot.lock().ok().and_then(|mut s| s.take()) {
            done(Err("Windows OCR could not be started".into()));
        }
    }
}

/// `{"images":[{"path":…,"lines":[{"text","x","y","w","h"}]} | {"error":…}, …]}`. A per-image error
/// is an empty page, not a failed batch: one unreadable picture must not lose the other eleven.
fn parse_batch(text: &str, expected: usize) -> Result<Vec<Page>, String> {
    let value: Value = serde_json::from_str(text.trim()).map_err(|_| "Windows OCR answered something unreadable".to_string())?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    let images = value.get("images").and_then(Value::as_array).ok_or_else(|| "Windows OCR answered no images".to_string())?;
    let mut pages: Vec<Page> = images
        .iter()
        .map(|image| {
            let lines = image
                .get("lines")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|l| {
                            let text = l.get("text").and_then(Value::as_str)?.trim().to_string();
                            (!text.is_empty()).then(|| Line {
                                text,
                                x: l.get("x").and_then(Value::as_f64).unwrap_or(0.0),
                                y: l.get("y").and_then(Value::as_f64).unwrap_or(0.0),
                                w: l.get("w").and_then(Value::as_f64).unwrap_or(0.0),
                                h: l.get("h").and_then(Value::as_f64).unwrap_or(0.0),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Page { lines }
        })
        .collect();
    pages.resize(expected, Page::default());
    Ok(pages)
}

/// Lines that belong to the same block of text, merged. Windows returns one `OcrLine` per visual
/// line, and translating the lines of a speech bubble separately produces word salad.
pub fn merge_lines(lines: &[Line]) -> Vec<Line> {
    let mut rows: Vec<Line> = lines.to_vec();
    rows.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<Line> = Vec::new();
    for line in rows {
        let joined = out.iter_mut().find(|prev| {
            let overlap = (prev.x + prev.w).min(line.x + line.w) - prev.x.max(line.x);
            let gap = line.y - (prev.y + prev.h);
            overlap > 0.35 * prev.w.min(line.w) && gap < 0.6 * prev.h.max(line.h) && gap > -prev.h
        });
        match joined {
            Some(prev) => {
                prev.text.push(' ');
                prev.text.push_str(&line.text);
                let right = (prev.x + prev.w).max(line.x + line.w);
                let bottom = (prev.y + prev.h).max(line.y + line.h);
                prev.x = prev.x.min(line.x);
                prev.y = prev.y.min(line.y);
                prev.w = right - prev.x;
                prev.h = bottom - prev.y;
            }
            None => out.push(line),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f64, y: f64, w: f64, h: f64) -> Line {
        Line { text: text.into(), x, y, w, h }
    }

    #[test]
    fn a_batch_answer_becomes_one_page_per_path() {
        let text = r#"{"lang":"en-US","images":[{"path":"a","width":10,"height":10,"lines":[{"text":"hi","x":1,"y":2,"w":3,"h":4}]},{"path":"b","error":"nope"}]}"#;
        let pages = parse_batch(text, 2).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines[0].text, "hi");
        assert!(pages[1].lines.is_empty(), "one unreadable image must not lose the others");
    }

    #[test]
    fn a_short_answer_is_padded_so_indices_still_line_up() {
        let pages = parse_batch(r#"{"images":[{"lines":[]}]}"#, 3).unwrap();
        assert_eq!(pages.len(), 3);
    }

    #[test]
    fn a_top_level_error_is_an_error() {
        assert!(parse_batch(r#"{"error":"no recognizer","code":"no-recognizer"}"#, 1).is_err());
        assert!(parse_batch("not json", 1).is_err());
    }

    #[test]
    fn stacked_lines_of_one_bubble_merge() {
        let merged = merge_lines(&[line("Hello", 10.0, 10.0, 80.0, 20.0), line("world", 12.0, 32.0, 70.0, 20.0)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Hello world");
        assert_eq!((merged[0].x, merged[0].y), (10.0, 10.0));
        assert!((merged[0].h - 42.0).abs() < 0.001, "the union box covers both lines");
    }

    #[test]
    fn far_apart_lines_stay_separate() {
        // Different columns, and a vertical gap far bigger than a line.
        let merged = merge_lines(&[line("left", 0.0, 0.0, 40.0, 20.0), line("right", 200.0, 0.0, 40.0, 20.0)]);
        assert_eq!(merged.len(), 2);
        let stacked = merge_lines(&[line("top", 0.0, 0.0, 40.0, 20.0), line("bottom", 0.0, 400.0, 40.0, 20.0)]);
        assert_eq!(stacked.len(), 2);
    }

    #[test]
    fn a_language_is_matched_on_its_base_subtag() {
        // pick_language consults the installed list, which needs Windows; the matching itself is
        // what this locks down.
        let installed = ["en-us".to_string(), "ko".to_string()];
        let base = |page: &str| {
            let page = page.to_ascii_lowercase();
            let b = page.split(['-', '_']).next().unwrap_or(&page).to_string();
            installed.iter().find(|t| **t == page).or_else(|| installed.iter().find(|t| t.split(['-', '_']).next() == Some(b.as_str()))).cloned()
        };
        assert_eq!(base("en-US"), Some("en-us".into()));
        assert_eq!(base("ko-KR"), Some("ko".into()));
        assert_eq!(base("ja"), None, "an uninstalled recognizer must not fall back to another language");
    }
}
