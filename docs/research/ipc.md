# sta: `sta://` UI scheme + JS <-> Rust IPC (cef 152.3.0+152.0.6, CEF 152.0.6)

Ground truth used:
- `B` = `cef-152.3.0+152.0.6/src/bindings/x86_64_pc_windows_msvc.rs` (line numbers `B:Lnnn`)
- `MR` = `src/wrapper/message_router.rs`, `MRU` = `src/wrapper/message_router_utils.rs`, `RM` = `src/wrapper/resource_manager.rs`, `SRH` = `src/wrapper/stream_resource_handler.rs`, `BRH` = `src/wrapper/byte_read_handler.rs`, `STR` = `src/string.rs`, `RC` = `src/rc.rs`
- CEF 152 headers in `.cef/152.0.6/cef_windows_x86_64/include/`
- CEF library sources (not shipped in the binary distribution), fetched from `github.com/chromiumembedded/cef` master: `libcef/common/scheme_registrar_impl.cc`, `libcef/browser/net_service/resource_handler_wrapper.cc`, `libcef/browser/stream_impl.cc`, `libcef/common/request_impl.cc`, `libcef/browser/frame_host_impl.cc`, `libcef/renderer/render_manager.cc`. Local copies: `scratchpad/research/ipcsrc/`.

---

## TL;DR: decisions

1. **Scheme:** register `sta` with `STANDARD | SECURE | CORS_ENABLED | FETCH_ENABLED | DISPLAY_ISOLATED`, **in every process**. That means you pass the `App` to `execute_process`. cefsimple passes `None`, and you must change that. Do not use `LOCAL` or `CSP_BYPASSING`.
2. **Assets:** use one `SchemeHandlerFactory` registered for `sta` plus a small in-memory `ResourceHandler` (`BytesHandler` below), with the files embedded by `rust-embed`. Don't use `wrapper::resource_manager`. Reading the code, its first request deadlocks (see section 2.6).
3. **IPC: pick Option A, `cef::wrapper::message_router`.**
   - **Requests:** use non-persistent queries.
   - **Rust -> JS push:** use a single *persistent* `__subscribe` query per page. It can succeed any number of times. It is canceled automatically on navigation, close and renderer crash.
   - **Security:** only the UI browsers' own `Client` (`UiClient`) forwards process messages to the router. The renderer adds `window.__staQuery` only to main frames of browsers created with `extra_info {sta_ui: true}` that are showing a `sta://ui/` URL. The browser-side handler checks `browser.identifier()`, `frame.is_main()` and `frame.url()` again, and that check is the one that counts.
4. **Push outside the router (Option B/C):** `Frame::execute_java_script` works, but nothing confirms the page received it. Build JSON with `serde_json` and escape U+2028 and U+2029.

---

## 1. Registering the custom scheme

### 1.1 APIs (verbatim)

- `impl ImplApp`: `fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {}` (B:L33653)
- `pub struct SchemeRegistrar(*mut _cef_scheme_registrar_t);` (B:L33366). `impl ImplSchemeRegistrar`: `fn add_custom_scheme(&self, scheme_name: Option<&CefString>, options: ::std::os::raw::c_int) -> ::std::os::raw::c_int;` (B:L33369)
- `pub struct SchemeOptions(cef_scheme_options_t);` (B:L49996), with these constants (B:L50018-50034):
  - `SchemeOptions::NONE`, `STANDARD`, `LOCAL`, `DISPLAY_ISOLATED`, `SECURE`, `CORS_ENABLED`, `CSP_BYPASSING`, `FETCH_ENABLED`
  - `impl SchemeOptions { pub fn get_raw(&self) -> i32 }` (B:L50038)
  - `SchemeOptions` is **not** a bitflags type. It is a newtype over a C enum, so you OR the `get_raw()` values yourself, as `tests_shared/src/common/client_app.rs:41` does.
- `pub fn execute_process(args: Option<&MainArgs>, application: Option<&mut App>, windows_sandbox_info: *mut u8) -> ::std::os::raw::c_int` (B:L58226)
- `pub fn initialize(args: Option<&MainArgs>, settings: Option<&Settings>, application: Option<&mut App>, windows_sandbox_info: *mut u8) -> ::std::os::raw::c_int` (B:L58252)

### 1.2 What the headers and CEF source say

- `cef_app.h`: "This method is called on the main thread for each process and the registered schemes should be the same across all processes." `cef_scheme.h`: "If |scheme_name| is a custom scheme then you must also implement the CefApp::OnRegisterCustomSchemes() method in all processes."
- `cef_scheme.h` on `AddCustomScheme`: "It should only be called once per unique |scheme_name| value. If |scheme_name| is already registered or if an error occurs this method will return false."
- `cef_types.h` option semantics:
  - **STANDARD:** URL canonicalization to `scheme://host/path`; the origin is scheme+host+port. Non-standard schemes "cannot be used as a target for form submission".
  - **LOCAL:** "same security rules as those applied to 'file' URLs". XHR is limited to the same URL. **Don't use.**
  - **DISPLAY_ISOLATED:** "can only be displayed from other content hosted with the same scheme. For example, pages in other origins cannot create iframes or hyperlinks to URLs with the scheme."
  - **SECURE:** "same security rules as those applied to 'https' URLs".
  - **CORS_ENABLED:** "should be set in most cases where CEF_SCHEME_OPTION_STANDARD is set".
  - **CSP_BYPASSING:** "should not be set in most cases where CEF_SCHEME_OPTION_STANDARD is set".
  - **FETCH_ENABLED:** "the scheme can perform Fetch API requests".
- `scheme_registrar_impl.cc` (CEF master) maps the options onto Chromium's lists:
  - `STANDARD` → `standard_schemes`, and also `referrer_schemes` when it is neither local nor display-isolated. So with DISPLAY_ISOLATED, sta pages send no Referer.
  - `SECURE` → `secure_schemes`, which makes the origin potentially trustworthy.
  - `CORS_ENABLED` → `cors_enabled_schemes`; `CSP_BYPASSING` → `csp_bypassing_schemes`.
  - `render_manager.cc` registers DISPLAY_ISOLATED (`RegisterURLSchemeAsDisplayIsolated`) and FETCH_ENABLED (`RegisterURLSchemeAsSupportingFetchAPI`) **in Blink, in the renderer**. That is one more reason the `App` must reach `execute_process`.

### 1.3 Code

```rust
// src/scheme.rs
use cef::*;

pub const SCHEME: &str = "sta";
pub const UI_ORIGIN: &str = "sta://ui/";

pub fn scheme_options() -> ::std::os::raw::c_int {
    [
        SchemeOptions::STANDARD,
        SchemeOptions::SECURE,          // secure context: crypto.subtle, clipboard, no mixed-content with https
        SchemeOptions::CORS_ENABLED,    // module scripts / CORS-mode fetches
        SchemeOptions::FETCH_ENABLED,   // fetch() to sta:// (Option B, and fetch of JSON assets)
        SchemeOptions::DISPLAY_ISOLATED,// web pages can't iframe/link sta:// (renderer-side, defense in depth)
    ]
    .iter()
    .fold(0, |acc, o| acc | o.get_raw()) as ::std::os::raw::c_int
}

wrap_app! {
    pub struct StaApp {
        shared: std::sync::Arc<crate::ipc::Shared>,
    }

    impl App {
        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            let Some(registrar) = registrar else { return };
            let ok = registrar.add_custom_scheme(Some(&CefString::from(SCHEME)), scheme_options());
            debug_assert_eq!(ok, 1, "sta scheme registration failed");
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(crate::ipc::StaBrowserProcessHandler::new(self.shared.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(crate::ipc::StaRenderProcessHandler::new(
                self.shared.renderer_router.clone(),
                self.shared.renderer_ui_ids.clone(),
            ))
        }
    }
}

// src/main.rs
fn main() {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let shared = std::sync::Arc::new(crate::ipc::Shared::new());
    let mut app = crate::scheme::StaApp::new(shared.clone());

    // SAME App in every process: schemes + RenderProcessHandler live in the renderer.
    let code = cef::execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if code >= 0 {
        std::process::exit(code); // subprocess finished
    }
    let settings = cef::Settings { no_sandbox: 1, ..Default::default() };
    assert_eq!(cef::initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()), 1);
    cef::run_message_loop();
    cef::shutdown();
}
```

### 1.4 How the `wrap_*!` macros work

This applies to every `wrap_x!` used in this report. Macro definitions: `wrap_app` B:L33673, `wrap_scheme_handler_factory` B:L33489, `wrap_resource_handler` B:L24807, `wrap_client` B:L27923, `wrap_request_handler` B:L26882, `wrap_life_span_handler` B:L20763, `wrap_browser_process_handler` B:L29211, `wrap_render_process_handler` B:L32583, `wrap_v8_handler` B:L29990, `wrap_task` B:L29498, `wrap_read_handler` B:L4577.

- **Struct form:** `vis struct Name { field: Ty, ... }` or the unit form `vis struct Name;`. The macro adds a hidden `cef_object: *mut RcImpl<_cef_x_t, Self>` field.
- **Constructor:** it generates `pub fn new(field1, field2, ...) -> X`. Arguments are in declaration order, and it returns the CEF wrapper type (`App`, `ResourceHandler`, `Task`, and so on).
- **Clone:** it generates `impl Clone`, which calls `.clone()` on **every field**. So every field type must be `Clone`:
  - `Mutex<T>` will not compile; use `Arc<Mutex<T>>`.
  - `RefCell<T: Clone>` and `OnceLock<T: Clone>` are fine.
  - Keep mutable state behind `Arc` so clones share it.
- **impl blocks:** these interfaces have **exactly one** `impl X { ... }` block (`App`, `Client`, `ResourceHandler`, `SchemeHandlerFactory`, `RequestHandler`, `LifeSpanHandler`, `RenderProcessHandler`, `V8Handler`, `Task`). Only Views delegates take several base-interface blocks (`ViewDelegate`, `PanelDelegate`, `WindowDelegate`, as in cefsimple). Methods are `fn m(&self, a: T, ...) -> R { ... }` and must match the `ImplX` trait signature, though type aliases such as `i32` vs `c_int` are fine.
- **Parameter names:** they are matched as `$arg_name:ident`, so **`_` alone is not allowed**. Write `_browser`.
  - VERIFIED-FIX (addition): the same `ident` matcher also rejects binding patterns such as `mut x: T` or `(a, b): T`, and the receiver must be written literally as `&self` (`& $self:ident`). Rebind inside the body (`let mut x = x;`). For Views delegates (`wrap_window_delegate!` B:L43304, etc.) every base block (`impl ViewDelegate {}`, `impl PanelDelegate {}`, `impl WindowDelegate {...}`) is **mandatory and in that order**, even when empty (cefsimple `simple_app.rs:13-24`).
- **Threading:** the macro adds no `Send`/`Sync` bounds, but CEF calls these objects from several threads. Use only `&self` and synchronize yourself. `RefGuard`-backed CEF types (`Browser`, `Frame`, `Callback`, ...) are `Send + Sync` (RC:L283-284).
  - VERIFIED-FIX (addition): that blanket `unsafe impl<T: Rc> Send/Sync for RefGuard<T>` also makes `V8Value`/`V8Context` `Send + Sync`, so the compiler will not stop you moving them off the renderer main thread. `cef_render_process_handler.h`: "V8 handles can only be accessed from the thread on which they are created."
- **Adding methods:** you can add your own inherent `impl Name { ... }` blocks. SRH:L117 adds `new_with_stream` that way.

### 1.5 Out-parameters in cef-rs (important)

- **Scalar out-params** (`Option<&mut i32>`, `Option<&mut i64>`) are `WrapParamRef`s. The value is written back to C when the shim returns (RC:L123-145), so `*x = v` works.
- **`Option<&mut CefString>` out-params** (`redirect_url`, the V8 `exception`) wrap the C string as `CefStringData::BorrowedMut` (STR:L271-274). **Assigning `*s = CefString::from("...")` silently does nothing**, because it replaces the Rust wrapper and never touches the C string. Use `s.try_set("...")` (STR:L571), which calls `cef_string_utf16_clear` plus `utf8_to_utf16` on the borrowed pointer.
- **`retval: Option<&mut Option<V8Value>>`:** assignment works, because the shim writes `wrap_retval` back (B:L30069-30072).

---

## 2. Scheme handler factory and resource handler

### 2.1 APIs (verbatim)

**Registration**
- `pub fn register_scheme_handler_factory(scheme_name: Option<&CefString>, domain_name: Option<&CefString>, factory: Option<&mut SchemeHandlerFactory>) -> ::std::os::raw::c_int` (B:L58192)
- Per request context: `impl ImplRequestContext`: `fn register_scheme_handler_factory(&self, scheme_name: Option<&CefString>, domain_name: Option<&CefString>, factory: Option<&mut SchemeHandlerFactory>) -> ::std::os::raw::c_int;` (B:L11116)

**Factory**
- `impl ImplSchemeHandlerFactory`: `fn create(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, scheme_name: Option<&CefString>, request: Option<&mut Request>) -> Option<ResourceHandler>` (B:L33471)

**`ImplResourceHandler`** (B:L24743)
- `fn open(&self, request: Option<&mut Request>, handle_request: Option<&mut ::std::os::raw::c_int>, callback: Option<&mut Callback>) -> ::std::os::raw::c_int` (B:L24745)
- `fn process_request(&self, request: Option<&mut Request>, callback: Option<&mut Callback>) -> ::std::os::raw::c_int` (B:L24754). Deprecated.
- `fn response_headers(&self, response: Option<&mut Response>, response_length: Option<&mut i64>, redirect_url: Option<&mut CefString>)` (B:L24762). This is C `get_response_headers`.
- `fn skip(&self, bytes_to_skip: i64, bytes_skipped: Option<&mut i64>, callback: Option<&mut ResourceSkipCallback>) -> ::std::os::raw::c_int` (B:L24770)
- `fn read(&self, data_out: *mut u8, bytes_to_read: ::std::os::raw::c_int, bytes_read: Option<&mut ::std::os::raw::c_int>, callback: Option<&mut ResourceReadCallback>) -> ::std::os::raw::c_int` (B:L24779). `data_out` is a **raw `*mut u8`**, not a slice.
- `fn read_response(...)` (B:L24789). Deprecated.
- `fn cancel(&self) {}` (B:L24799)

**Callbacks**
- `ImplCallback`: `fn cont(&self);` (B:L8728), `fn cancel(&self);` (B:L8730)
- `ImplResourceReadCallback`: `fn cont(&self, bytes_read: ::std::os::raw::c_int);` (B:L24672)
- `ImplResourceSkipCallback`: `fn cont(&self, bytes_skipped: i64);` (B:L24617)

**`ImplResponse`** (B:L24321)
- `fn set_status(&self, status: ::std::os::raw::c_int);` (L24331)
- `fn set_status_text(&self, status_text: Option<&CefString>);` (L24335)
- `fn set_mime_type(&self, mime_type: Option<&CefString>);` (L24339)
- `fn set_charset(&self, charset: Option<&CefString>);` (L24343)
- `fn set_header_by_name(&self, name: Option<&CefString>, value: Option<&CefString>, overwrite: ::std::os::raw::c_int);` (L24347)
- `fn set_header_map(&self, header_map: Option<&mut CefStringMultimap>);` (L24356)
- `fn set_error(&self, error: Errorcode);` (L24327)

**`ImplRequest`** (B:L6673)
- `fn url(&self) -> CefStringUserfree;` (L6677)
- `fn method(&self) -> CefStringUserfree;` (L6681)
- `fn post_data(&self) -> Option<PostData>;` (L6691)
- `fn header_by_name(&self, name: Option<&CefString>) -> CefStringUserfree;` (L6699)
- `fn resource_type(&self) -> ResourceType;` (L6724)
- `fn identifier(&self) -> u64;` (L6728)

**Streams**
- `pub fn stream_reader_create_for_data(data: *mut u8, size: usize) -> Option<StreamReader>` (B:L57287)
- `pub fn stream_reader_create_for_handler(handler: Option<&mut ReadHandler>) -> Option<StreamReader>` (B:L57301)
- `pub fn stream_reader_create_for_file(file_name: Option<&CefString>) -> Option<StreamReader>` (B:L57271)

**Strings and MIME**
- `pub fn get_mime_type(extension: Option<&CefString>) -> CefStringUserfree` (B:L58650)
- `CefStringMultimap::new()` (STR:L1141); `pub fn append(&mut self, key: &str, value: &str) -> bool` (STR:L1147)
- String conversion: `CefString::from(&str)` (STR:L489). `CefString::from(&CefStringUserfree)` (STR:L504) then `.to_string()` (Display, STR:L624).

### 2.2 Threads and lifecycle (headers + `resource_handler_wrapper.cc`)

**Where each call runs**

- **`SchemeHandlerFactory::create`:** `cef_scheme.h`: "The methods of this class will always be called on the IO thread." `browser` and `frame` "will be the browser window and frame respectively that originated the request or NULL if the request did not originate from a browser window (for example, if the request came from CefURLRequest). The |request| object passed to this method cannot be modified."
- **`register_scheme_handler_factory`:** "may be called on any thread in the browser process" (`cef_scheme.h`). Call it from `on_context_initialized`. An empty or `None` `domain_name` matches all hosts of a standard scheme.
- **`open`, `skip`, `read`:** `cef_resource_handler.h` says each "will be called in sequence but not from a dedicated thread". That is a worker sequence, neither UI nor IO; SRH:L20-29 debug-asserts both.
- **`get_response_headers`:** `CEF_REQUIRE_IOT()` in `resource_handler_wrapper.cc`, so the IO thread.
- **`cancel`:** posted to IO by `HandlerProvider::Detach` (`CEF_POST_TASK(CEF_IOT, ... CefResourceHandler::Cancel)`). It can race with a worker-thread `read`, so keep state behind a `Mutex`.

**Initial values and how returns are interpreted** (`resource_handler_wrapper.cc`)

- **`open`:**
  - The wrapper starts with `bool handle_request = false;`.
  - Result `true` + `handle_request=true`: continue now.
  - Result `true` + `handle_request=false`: wait for `callback.cont()` / `callback.cancel()`.
  - Result `false` + `handle_request=true`: cancel.
  - Result `false` + `handle_request=false`: legacy `ProcessRequest` on IO.
  - The cef-rs default `open` returns 0 and leaves `handle_request` alone, which falls through to `ProcessRequest`, whose default returns 0 → cancel.
- **`get_response_headers`:**
  - `int64_t response_length = -1; CefString redirect_url;` If `redirect_url` is non-empty you get a 307 with a `Location` header. If `response.set_error(...)` was called, that error wins.
  - An empty status text is filled from `net::GetHttpReasonPhrase`, so `set_status_text` is optional.
- **`read`:**
  - Return `true` with `bytes_read > 0`: data delivered.
  - Return `true` with `bytes_read == 0`: async; call `ResourceReadCallback::cont(n)` later. `data_out` "will remain valid until the callback is executed".
  - Return `false` with `bytes_read == 0`: EOF.
  - Return `false` with `bytes_read < 0`: error.
  - Return `false` with `bytes_read == -1`: legacy `ReadResponse`.
  - VERIFIED-FIX: those two rows overlap. `-1` is special-cased first (`if (*bytes_read == -1)` → `ReadResponse`, `resource_handler_wrapper.cc` L292), so the error case is really `bytes_read <= -2`. Use `-2` (ERR_FAILED), as the header suggests.
  - VERIFIED-FIX (addition): returning `true` with `bytes_read == 0` without keeping the `callback` is **not** a clean EOF. `InputStreamWrapper::Read` drops its `CefRefPtr<ReadCallbackWrapper>` when it returns, and `~ReadCallbackWrapper` then runs the callback with `net::ERR_FAILED` (L69-75). The request fails.
  - **Always set `*bytes_read` explicitly.** (The C caller `InputStreamReader::Read` starts with `int bytes_read = 0`, `stream_reader_url_loader.cc` L311. `WrapParamRef` writes back whatever value the out-param holds when the shim returns.)
- **`skip`:** returning `true` with `bytes_skipped == 0` means "wait for the callback". For "no bytes to skip", return `false` with `-2` (ERR_FAILED).
- **Callback threads:** `ReadCallbackWrapper` and `SkipCallbackWrapper` re-post to the worker sequence if needed, so `cont()` may be called from any thread, including UI.

### 2.3 Ready-made helpers in cef-rs, and how to use them

**`wrapper::stream_resource_handler::StreamResourceHandler`** (SRH)

- Fields: `status_code: i32, status_text: String, mime_type: String, header_map: Option<CefStringMultimap>, stream: Option<StreamReader>`. The macro therefore generates `StreamResourceHandler::new(status_code: i32, status_text: String, mime_type: String, header_map: Option<CefStringMultimap>, stream: Option<StreamReader>) -> ResourceHandler`.
- Convenience constructor: `pub fn new_with_stream(mime_type: String, stream: StreamReader) -> ResourceHandler` (SRH:L118), which uses 200 "OK" with no extra headers.
- Limitations:
  - `response_length` is always `-1` (unknown length).
    - VERIFIED-FIX: not always. SRH:L64 is `*response_length = if self.stream.is_some() { -1 } else { 0 };`.
  - VERIFIED-FIX (addition): **`stream: None` breaks the request.** With no stream, `read` does `*bytes_read = 0; return 1;` (SRH:L92-95), which means "wait for the callback", and SRH never keeps the callback. `StreamReaderURLLoader::ReadMore` calls `Read` even when content-length is 0 (`stream_reader_url_loader.cc` L748-785). The dropped `ReadCallbackWrapper` then completes the load with `ERR_FAILED` (see 2.2). This comes from reading the source, not a test, but expect a failed load, not an empty body.
  - No `skip`, so Range requests fail (this matters for `<video>`).
  - `open` handles the request synchronously.

**`stream_reader_create_for_data(ptr, len)`**
- CEF **copies** the buffer. `stream_impl.cc` (master): `CefBytesReader::SetData` → `data_.reserve(datasize); std::copy(...)`. So the Rust `Vec` may be dropped right after the call.
- It **returns `None` for `size == 0`** (`if (data && size > 0)`). Handle empty files.

**`wrapper::byte_read_handler`** (BRH)
- `ByteStream::new(bytes: Vec<u8>)`, plus the macro-generated `ByteReadHandler::new(stream: Arc<Mutex<ByteStream>>) -> ReadHandler`.
- Use it with `stream_reader_create_for_handler` (this is what `tests_shared/src/browser/resource_util/win.rs:419` does). It avoids CEF's copy.
  - VERIFIED-FIX: wrong line. That file has 124 lines; the call is at `win.rs:74-75` (`ByteReadHandler::new(Arc::new(Mutex::new(stream)))` then `stream_reader_create_for_handler(Some(&mut handler))`).

```rust
use cef::{*, wrapper::{stream_resource_handler::StreamResourceHandler, byte_read_handler::*}};
use std::sync::{Arc, Mutex};

fn stream_handler(mime: &str, bytes: &[u8]) -> ResourceHandler {
    let mut headers = CefStringMultimap::new();
    headers.append("Cache-Control", "no-store");
    // copy into CEF (fine for small UI files):
    let mut tmp = bytes.to_vec();
    let stream = stream_reader_create_for_data(tmp.as_mut_ptr(), tmp.len());
    // or zero-extra-copy: let mut rh = ByteReadHandler::new(Arc::new(Mutex::new(ByteStream::new(bytes.to_vec()))));
    //                     let stream = stream_reader_create_for_handler(Some(&mut rh));
    StreamResourceHandler::new(200, "OK".to_string(), mime.to_string(), Some(headers), stream)
    // VERIFIED-FIX: stream == None (empty file) => response_length 0, but SRH read() returns
    // true + bytes_read=0 ("async") without keeping the callback => load fails with ERR_FAILED.
    // Handle empty files with BytesHandler (2.4) instead.
}
```

**`wrapper::resource_manager::ResourceManager`: do not use it.**
- It is built for `ResourceRequestHandler::on_before_resource_load` + `resource_handler`, not for a scheme factory.
- **Deadlock (reading the code):**
  - `ResourceManager::on_before_resource_load(&mut self, ...)` (RM:L906) can only be reached through `manager.lock()`, since `new()` returns `Arc<Mutex<Self>>`.
  - It calls `self.send_request` (RM:L970) → `ResourceManagerRequest::send_request` (RM:L138), which does `state.manager.upgrade()` → `manager.lock()` (RM:L149) on the **same non-reentrant `std::sync::Mutex`** on the same thread.
  - Result: a deadlock on the first request.
  - VERIFIED-FIX (addition): there is a second self-lock. `ResourceManager::send_request` holds `request.lock()` (RM:L983) while `request.send_request()` calls `provider.on_request(request)`, and the built-in providers lock the same request mutex again (`ContentProvider` RM:L267). So even if you avoid the manager lock, it still deadlocks. This only applies with at least one provider; with none it returns `CONTINUE` early (RM:L919-923).
- **Inverted MIME logic:** `ContentProvider` has `if self.mime_type.is_empty() { self.mime_type.clone() } else { resolver }` (RM:L281-285).

### 2.4 Recommended in-memory handler (complete)

```rust
// src/scheme_handler.rs
use cef::*;
use std::{borrow::Cow, sync::{Arc, Mutex}};

pub struct Resp {
    pub status: i32,
    pub mime: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Cow<'static, [u8]>, // rust-embed EmbeddedFile.data is Cow<'static,[u8]>
    pub pos: usize,
}

impl Resp {
    pub fn new(status: i32, mime: &'static str, body: impl Into<Cow<'static, [u8]>>) -> Self {
        Self { status, mime, headers: Vec::new(), body: body.into(), pos: 0 }
    }
}

/// Shared by BytesHandler and the async ApiHandler (Option B).
pub fn fill_headers(r: &Resp, response: Option<&mut Response>, response_length: Option<&mut i64>) {
    let Some(response) = response else { return };
    response.set_status(r.status);
    response.set_mime_type(Some(&CefString::from(r.mime)));
    if r.mime.starts_with("text/") || r.mime.ends_with("json") || r.mime == "image/svg+xml" {
        response.set_charset(Some(&CefString::from("utf-8")));
    }
    for (k, v) in &r.headers {
        response.set_header_by_name(Some(&CefString::from(*k)), Some(&CefString::from(v.as_str())), 1);
    }
    if let Some(len) = response_length {
        *len = r.body.len() as i64; // known length (WrapParamRef writes it back)
    }
}

pub fn copy_out(r: &mut Resp, data_out: *mut u8, bytes_to_read: i32, bytes_read: Option<&mut i32>) -> i32 {
    let Some(out) = bytes_read else { return 0 };
    *out = 0;
    let remaining = r.body.len() - r.pos;
    if remaining == 0 || bytes_to_read <= 0 {
        return 0; // bytes_read=0 + false => response complete
    }
    let n = remaining.min(bytes_to_read as usize);
    // SAFETY: CEF guarantees `data_out` points to at least `bytes_to_read` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(r.body.as_ptr().add(r.pos), data_out, n) };
    r.pos += n;
    *out = n as i32;
    1
}

wrap_resource_handler! {
    pub struct BytesHandler {
        resp: Arc<Mutex<Resp>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            _callback: Option<&mut Callback>,
        ) -> i32 {
            if let Some(h) = handle_request { *h = 1; } // handle immediately
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>, // to redirect: `if let Some(u) = _redirect_url { u.try_set("sta://ui/"); }`
        ) {
            if let Ok(r) = self.resp.lock() { fill_headers(&r, response, response_length); }
        }

        fn skip(
            &self,
            bytes_to_skip: i64,
            bytes_skipped: Option<&mut i64>,
            _callback: Option<&mut ResourceSkipCallback>,
        ) -> i32 {
            let Some(out) = bytes_skipped else { return 0 };
            let Ok(mut r) = self.resp.lock() else { *out = -2; return 0 };
            let n = (r.body.len() - r.pos).min(bytes_to_skip.max(0) as usize);
            if n == 0 { *out = -2; return 0; } // 0 + true would mean "wait for callback"
            r.pos += n;
            *out = n as i64;
            1
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            match self.resp.lock() {
                Ok(mut r) => copy_out(&mut r, data_out, bytes_to_read, bytes_read),
                Err(_) => { if let Some(b) = bytes_read { *b = -2; } 0 }
            }
        }

        fn cancel(&self) {
            if let Ok(mut r) = self.resp.lock() { r.pos = r.body.len(); }
        }
    }
}

impl BytesHandler {
    pub fn from(resp: Resp) -> ResourceHandler { Self::new(Arc::new(Mutex::new(resp))) }
    pub fn text(status: i32, msg: &'static str) -> ResourceHandler {
        Self::from(Resp::new(status, "text/plain", msg.as_bytes()))
    }
}
```

### 2.5 Factory (assets + optional API routing)

```rust
// src/scheme_factory.rs
use cef::*;
use std::sync::Arc;
use crate::{scheme_handler::*, assets::ui_asset};

/// "sta://ui/a/b.js?x#y" -> ("ui", "a/b.js")
fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("sta://")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    Some(rest.split_once('/').unwrap_or((rest, "")))
}

wrap_scheme_handler_factory! {
    pub struct StaSchemeFactory {
        shared: Arc<crate::ipc::Shared>,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            // IO thread. Never return None for our scheme (would surface as a load error); return a 404.
            let Some(request) = request else { return Some(BytesHandler::text(400, "bad request")) };
            let url = CefString::from(&request.url()).to_string();
            match split_url(&url) {
                Some(("ui", path)) => Some(ui_asset(path)),
                // Option B only: Some(("ui", p)) if p.starts_with("api/") => api_handler(&self.shared, _browser, _frame, request, p)
                _ => Some(BytesHandler::text(404, "not found")),
            }
        }
    }
}

// in BrowserProcessHandler::on_context_initialized (UI thread):
// let mut f = StaSchemeFactory::new(shared.clone());
// register_scheme_handler_factory(Some(&CefString::from("sta")), None, Some(&mut f));
```

The static UI assets are not secret, so the factory does not need to check the browser id for them. What keeps web tabs from opening `sta://ui` is `TabClient::on_before_browse` (section 4.4), and the IPC surface has its own check.

---

## 3. Reading a POST body from `Request`

APIs:
- `ImplRequest::post_data(&self) -> Option<PostData>` (B:L6691)
- `ImplPostData`:
  - `fn has_excluded_elements(&self) -> ::std::os::raw::c_int;` (B:L7082)
  - `fn element_count(&self) -> usize;` (B:L7084)
  - `fn elements(&self, elements: Option<&mut Vec<Option<PostDataElement>>>);` (B:L7086)
- `ImplPostDataElement`:
  - `fn get_type(&self) -> PostdataelementType;` (B:L7276)
  - `fn file(&self) -> CefStringUserfree;` (B:L7278)
  - `fn bytes_count(&self) -> usize;` (B:L7280)
  - `fn bytes(&self, size: usize, bytes: *mut u8) -> usize;` (B:L7282)
- Constants `PostdataelementType::EMPTY/BYTES/FILE` (B:L47180-47184). The type derives `PartialEq`.

Gotchas:
- **`elements()` only fills as many slots as the `Vec` already has.** The shim uses `arg.len()` as the count and truncates to CEF's count (B:L7137-7175). **Pre-size it with `vec![None; post.element_count()]`.**
- `cef_request.h`: `HasExcludedElements` is "true if the underlying POST data includes elements that are not represented by this CefPostData object (for example, multi-part file upload data)".
- `request_impl.cc` (master) converts only `network::DataElement::Tag::kBytes` → BYTES and `kFile` → FILE. Any other element (data pipe, i.e. `Blob`/`ReadableStream` bodies) becomes an **EMPTY element with no data**. So `fetch(url, {method:'POST', body: JSON.stringify(x)})` (string body → bytes) works, but a `Blob`/`FormData`-with-file/stream body does not. CefSharp discussion #4784 reports the same limitation for Blob bodies. I found no ceftests coverage of fetch/XHR POST to a custom scheme (`cors_unittest.cc` tests GET only), so smoke-test this.
  - VERIFIED-FIX: `cors_unittest.cc` is not GET-only. Its XHR/fetch sub-requests use `kSubRequestMethod[] = "GET"` (L1016), but there are form-POST redirect tests (e.g. `RedirectPost307HttpSchemeToCustomNonStandardScheme`, L31) that check `GetPostData()` (L1990). The conclusion stands: nothing covers a fetch/XHR POST body with a Blob or stream. `request_impl.cc` L1150-1164 (master) confirms that only `kBytes`/`kFile` are converted. Other tags leave the element EMPTY.

```rust
pub fn read_post_body(request: &Request) -> Result<Vec<u8>, &'static str> {
    let Some(post) = request.post_data() else { return Ok(Vec::new()) };
    if post.has_excluded_elements() != 0 { return Err("excluded elements"); }
    let mut elements: Vec<Option<PostDataElement>> = vec![None; post.element_count()];
    post.elements(Some(&mut elements));
    let mut out = Vec::new();
    for el in elements.into_iter().flatten() {
        let t = el.get_type();
        if t == PostdataelementType::BYTES {
            let n = el.bytes_count();
            let start = out.len();
            out.resize(start + n, 0);
            let got = el.bytes(n, out[start..].as_mut_ptr());
            out.truncate(start + got);
        } else if t == PostdataelementType::FILE {
            return Err("file element"); // CefString::from(&el.file()).to_string() is a path
        } else {
            return Err("unsupported body (Blob/stream => EMPTY element); send a string body");
        }
    }
    Ok(out)
}
```

---

## 4. IPC options

### 4.A `cef::wrapper::message_router` (a port of `CefMessageRouter`, 1784 LoC; `MR`)

#### 4.A.1 API (verbatim, `MR`)

**Config**
- `pub struct MessageRouterConfig { pub js_query_function: String, pub js_cancel_function: String, pub message_size_threshold: usize }` (MR:L17)
- `Default` gives `"cefQuery"`, `"cefQueryCancel"` and 16 KB. Messages above the threshold go through a shared-memory region.
  - VERIFIED-FIX: precisely, `payload.size() < threshold` goes as a list message, and `>=` 16 KB goes through shared memory (MRU `create_browser_response_builder` L679, `build_renderer_message`). IPC message names are `js_query_function + "Msg"` and `js_cancel_function + "Msg"` (MR `MESSAGE_SUFFIX`).

**Callback and handler traits**
- `pub trait BinaryBuffer: Send { fn data(&self) -> &[u8]; fn data_mut(&mut self) -> &mut [u8]; }` (MR:L49)
- `pub trait BrowserSideCallback: Send + Sync` (MR:L62):
  - `fn success_str(&self, response: &str);`
  - `fn success_binary(&self, data: &[u8]);`
  - `fn failure(&self, error_code: i32, error_message: &str);`
- `pub trait BrowserSideHandler: Send + Sync` (MR:L76). Doc: "All methods will be executed on the browser process UI thread." Methods:
  - `fn on_query_str(&self, _browser: Option<Browser>, _frame: Option<Frame>, _query_id: i64, _request: &str, _persistent: bool, _callback: Arc<Mutex<dyn BrowserSideCallback>>) -> bool` (MR:L85)
  - `fn on_query_binary(&self, _browser: Option<Browser>, _frame: Option<Frame>, _query_id: i64, _request: &dyn BinaryBuffer, _persistent: bool, _callback: Arc<Mutex<dyn BrowserSideCallback>>) -> bool` (MR:L105)
  - `fn on_query_canceled(&self, _browser: Option<Browser>, _frame: Option<Frame>, _query_id: i64) {}` (MR:L124)

**Browser-side router** (MR:L129; implemented by `pub struct BrowserSideRouter` MR:L465)
- `pub trait MessageRouterBrowserSide`:
  - VERIFIED-FIX (omitted member): `type Callback: BrowserSideCallback;` (MR:L130). `BrowserSideRouter` sets `type Callback = BrowserSideRouterCallback;` (MR:L764).
  - `fn new(config: MessageRouterConfig) -> Arc<Self>;`
  - `fn add_handler(&self, handler: Arc<dyn BrowserSideHandler>, first: bool) -> Option<HandlerId>;`. Must be on UI; it `debug_assert`s.
  - `fn remove_handler(&self, handler_id: HandlerId) -> bool;`
  - `fn cancel_pending(&self, browser: Option<Browser>, handler_id: Option<HandlerId>);`
  - `fn pending_count(&self, browser: Option<Browser>, handler_id: Option<HandlerId>) -> usize;`
- `pub trait MessageRouterBrowserSideHandlerCallbacks: MessageRouterBrowserSide` (MR:L163):
  - `fn on_before_close(&self, browser: Option<Browser>);`
  - `fn on_render_process_terminated(&self, browser: Option<Browser>);`
  - `fn on_before_browse(&self, browser: Option<Browser>, frame: Option<Frame>);`
  - `fn on_process_message_received(&self, browser: Option<Browser>, frame: Option<Frame>, source_process: ProcessId, message: Option<ProcessMessage>) -> bool;`

**Renderer-side router** (MR:L196; implemented by `pub struct RendererSideRouter` MR:L1138)
- `pub trait MessageRouterRendererSide`:
  - `fn new(config: MessageRouterConfig) -> Arc<Self>;`
  - `fn pending_count(&self, browser: Option<Browser>, context: Option<V8Context>) -> usize;`
- `pub trait MessageRouterRendererSideHandlerCallbacks: MessageRouterRendererSide` (MR:L207):
  - `fn on_context_created(&self, browser: Option<Browser>, frame: Option<Frame>, context: Option<V8Context>);`
  - `fn on_context_released(&self, browser: Option<Browser>, frame: Option<Frame>, context: Option<V8Context>);`
  - `fn on_process_message_received(&self, browser: Option<Browser>, frame: Option<Frame>, source_process: Option<ProcessId>, message: Option<ProcessMessage>) -> bool;`. Note that `source_process` is **`Option<ProcessId>`** here.

**CEF handler hooks you forward** (verbatim)
- `ImplClient::on_process_message_received(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, source_process: ProcessId, message: Option<&mut ProcessMessage>) -> ::std::os::raw::c_int` (B:L27907)
- `ImplLifeSpanHandler::on_before_close(&self, browser: Option<&mut Browser>)` (B:L20755)
- `ImplLifeSpanHandler::on_after_created(&self, browser: Option<&mut Browser>)` (B:L20749)
- `ImplRequestHandler::on_before_browse(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, user_gesture: ::std::os::raw::c_int, is_redirect: ::std::os::raw::c_int) -> ::std::os::raw::c_int` (B:L26781)
- `ImplRequestHandler::on_render_process_terminated(&self, browser: Option<&mut Browser>, status: TerminationStatus, error_code: ::std::os::raw::c_int, error_string: Option<&CefString>)` (B:L26865)
- `ImplRenderProcessHandler`:
  - `on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>)` (B:L32518)
  - `on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>)` (B:L32531)
  - `on_context_released(...)`, same parameters (B:L32539)
  - `on_process_message_received(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, source_process: ProcessId, message: Option<&mut ProcessMessage>) -> ::std::os::raw::c_int` (B:L32565)

#### 4.A.2 Semantics (`cef_message_router.h`, plus reading `MR`)

**JS API**

```js
window.cefQuery({request, persistent, onSuccess, onFailure}) // returns request id
window.cefQueryCancel(id)
```

`request` must be a string or an ArrayBuffer. `onSuccess(response)` receives a string, or an ArrayBuffer for `success_binary`. `onFailure(error_code, error_message)`.

**Persistence and push**

Header: "If the query is persistent then the callbacks will remain registered until one of the following conditions are met: A. The query is canceled in JavaScript... B. ...Callback::Failure... C. The context associated with the query is released due to browser destruction, navigation or renderer process termination." The header lists the "Subscription" and "Broadcast" patterns explicitly.

**Answer: yes, a persistent query can push events from Rust to JS repeatedly.**
- In the Rust port, `BrowserSideRouterCallbackSuccess::execute` keeps `router` for persistent callbacks (`callback.router.clone()`, MR:L397-402).
- The renderer's `execute_success_callback` does not remove persistent entries (`get_request_info(..., false)`, MR:L1320).
- Each `success_str` posts one UI task, so events arrive in order.

**Threads**
- Handler methods run on the UI thread.
- `BrowserSideCallback` methods "may be called on any browser process thread" (MR:L61).
- Renderer-router methods run on the render main thread.
- Unhandled queries fail with code -1, "The query has been canceled".

**Forwarding rules** (from the header)
- Call `on_before_browse` "only if the navigation is allowed to proceed".
- "In order for the router to function correctly any browser or context instance passed into a single router callback must then be passed into all router callbacks." Forwarding a **superset** to `on_context_released` and `on_process_message_received` is harmless: unknown contexts map to `RESERVED_ID`, and foreign message names return false.

**cef-rs port gotchas (found by reading the code)**

1. **Invalid-argument exceptions are silently lost.**
   - `RendererSideV8Handler` does `*exception = CefString::from($message)` (MR:L1641-1648). As section 1.5 explains, that never writes the C out-param, and the function returns 1 with no retval, so JS gets `undefined` and no query is sent.
   - Missing keys come back from `value_bykey` as an *undefined* `V8Value`, not `None`. So **omitting `onSuccess` or `onFailure` hits the "must have type function" error (MR:L1676-1702), and the query silently never happens.** Always pass both.
2. **Don't call router methods from inside handler callbacks.** `BrowserSideRouter::on_process_message_received` holds the `browser_query_info_map` `std::sync::Mutex` while it calls `on_query_str/binary` (MR:L980-1078). `cancel_pending_for` holds the same lock while calling `on_query_canceled` (MR:L663-711). If your handler synchronously calls `router.pending_count/cancel_pending/remove_handler`, it deadlocks. `callback.success_str/failure` is fine, because it only posts a task.
   - VERIFIED-FIX (addition, PLAUSIBLE): the same lock is still held for any **synchronous re-entry** into a forwarded callback. CEF runs `OnBeforeBrowse` synchronously from its navigation throttle (`throttle_handler.cc` L100-105: "Must use SynchronyMode::kSync"). So a handler that navigates or reloads **a UI browser** in-line (`frame.load_url`, `host.reload()`) can reach `UiRequestHandler::on_before_browse` → `router.on_before_browse` → `cancel_pending_for` → the same map mutex, and deadlock. I did not confirm that Chromium starts the throttle synchronously inside `LoadURL`. Either way, `post_task(ThreadId::UI, ...)` any command that navigates, reloads or closes a UI browser. Tab browsers are safe, because `TabClient` does not forward to the router.
3. `create_browser_response_builder` returns a `std::rc::Rc` that is moved into a cross-thread `Task` (MRU:L679). It has a single owner and no concurrent refcounting, so in practice it's benign.
4. The shared-memory path (>16 KB) writes UTF-8 on both sides, so it is internally consistent. It is not wire-compatible with C++ `CefMessageRouter`, but that doesn't matter here.
5. VERIFIED-FIX (new port bug): **`Failure` on a persistent query leaves a stale browser-side entry.**
   - `BrowserSideRouter::on_callback_failure` calls `self.get_query_info(browser_id, query_id, false)` (MR:L542). Persistent entries are only removed when `always_remove` is true, so the entry stays in `browser_query_info_map`, holding its `Browser`/`Frame` refs.
   - C++ `OnCallbackFailure` passes `always_remove=true` and `DCHECK(removed)` (`libcef_dll/wrapper/cef_message_router.cc` L456-459).
   - The renderer side does remove its own entry (`execute_failure_callback` uses `true`).
   - Consequences:
     - `pending_count` is inflated.
     - On the next main-frame navigation, close or crash, `on_query_canceled` still fires for a query you already failed. The header says it should not: "If a query is canceled for a reason other than Callback::Failure being executed then the associated Handler's OnQueryCanceled method will be called."
   - Mitigation: make `on_query_canceled` idempotent (the `UiIpc` sketch's `subscribers.remove` already is). Avoid failing a persistent query that you have already registered.

### 4.B `fetch()`/XHR POST to `sta://ui/api/...`, with push via `execute_java_script`

- **Threads:** `create` runs on IO, `open/read` on a worker sequence, headers on IO (section 2.2). To touch app state owned by the UI thread:
  1. In `open`, set `*handle_request = 0` and return 1.
  2. `post_task(ThreadId::UI, Some(&mut task))` with a `wrap_task!` task.
  3. In the task, fill the shared `Resp`, then call `callback.cont()`. That works from any thread; CEF re-posts to the worker sequence.
  - Relevant signatures: `pub fn post_task(thread_id: ThreadId, task: Option<&mut Task>) -> ::std::os::raw::c_int` (B:L57830); `pub fn currently_on(thread_id: ThreadId) -> ::std::os::raw::c_int` (B:L57820); `ThreadId::UI/IO/FILE_USER_BLOCKING/RENDERER` (B:L47510-47522).
- **Knowing the requesting browser:** yes. `create(browser, frame, ...)` gives `browser.identifier()` (`fn identifier(&self) -> ::std::os::raw::c_int;` B:L11725) and `frame.url()` / `frame.is_main()` (B:L7567/L7557). `browser` is `None` for requests not issued by a browser (CefURLRequest, and probably service workers), so reject those. `CefBrowser` and `CefFrame` methods "may be called on any thread" in the browser process (`cef_browser.h`, `cef_frame.h`).
- **Security:** the factory is **global** and runs for every browser. A web page can at least *try* `fetch('sta://ui/api/x', {method:'POST', mode:'no-cors'})`. Renderer-side DISPLAY_ISOLATED and CORS checks may block that, but they are not a security boundary, and a POST can have side effects before any CORS read-blocking applies. So every API request **must** be authorized in `create`/`open` by browser id, main frame and URL.
  - A tighter variant: serve `/api` only from `UiClient`'s `RequestHandler::resource_request_handler` → `ImplResourceRequestHandler::resource_handler(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>) -> Option<ResourceHandler>` (B:L25515). That handler exists per `Client`, which gives the same client-level isolation as Option A.
- **Costs:**
  - Each call allocates a handler and goes through the full loader pipeline: renderer → browser URLLoader proxy → IO → worker → UI hop → worker → data pipe → renderer.
  - POST bodies must be strings (section 3).
  - Push needs `execute_java_script`, which you fire and forget. If it runs before the page defines `window.__staDispatch`, the event is lost, so you need a "ready" handshake.
  - Its advantages: no renderer code, and requests show up in the DevTools Network tab.

```rust
// Option B core (sketch)
wrap_task! {
    struct ApiTask {
        browser_id: i32,
        cmd: String,
        body: Vec<u8>,
        resp: std::sync::Arc<std::sync::Mutex<crate::scheme_handler::Resp>>,
        callback: Callback,
    }
    impl Task {
        fn execute(&self) {
            // UI thread: owns browser state
            // VERIFIED-FIX: 4.E defines `dispatch(browser_id: i32, cmd: &str, payload: serde_json::Value)`;
            // passing `&self.body` (&Vec<u8>) would not type-check. Parse first:
            let payload: serde_json::Value = serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null);
            let (status, json) = match crate::commands::dispatch(self.browser_id, &self.cmd, payload) {
                Ok(v) => (200, v.to_string()),
                Err(e) => (500, serde_json::json!({ "error": e }).to_string()),
            };
            if let Ok(mut r) = self.resp.lock() {
                r.status = status;
                r.body = json.into_bytes().into();
                r.headers.push(("Cache-Control", "no-store".into()));
            }
            self.callback.cont(); // -> response_headers (IO) -> read (worker)
        }
    }
}

wrap_resource_handler! {
    pub struct ApiHandler {
        browser_id: i32,
        cmd: String,
        resp: std::sync::Arc<std::sync::Mutex<crate::scheme_handler::Resp>>,
    }
    impl ResourceHandler {
        fn open(&self, request: Option<&mut Request>, handle_request: Option<&mut i32>, callback: Option<&mut Callback>) -> i32 {
            let (Some(request), Some(h), Some(cb)) = (request, handle_request, callback) else { return 0 };
            *h = 0; // decide later
            let Ok(body) = crate::post::read_post_body(request) else { *h = 1; return 0 }; // cancel
            let mut task = ApiTask::new(self.browser_id, self.cmd.clone(), body, self.resp.clone(), cb.clone());
            post_task(ThreadId::UI, Some(&mut task));
            1
        }
        fn response_headers(&self, response: Option<&mut Response>, response_length: Option<&mut i64>, _redirect_url: Option<&mut CefString>) {
            if let Ok(r) = self.resp.lock() { crate::scheme_handler::fill_headers(&r, response, response_length); }
        }
        fn read(&self, data_out: *mut u8, bytes_to_read: i32, bytes_read: Option<&mut i32>, _callback: Option<&mut ResourceReadCallback>) -> i32 {
            match self.resp.lock() { Ok(mut r) => crate::scheme_handler::copy_out(&mut r, data_out, bytes_to_read, bytes_read), Err(_) => 0 }
        }
        fn cancel(&self) {}
    }
}
// in factory.create: authorize (ui_browser_ids.contains(browser.identifier()) && frame.is_main()!=0
// && frame url starts_with "sta://ui/" && method == "POST") before returning ApiHandler.
```

### 4.C Custom V8 binding + `ProcessMessage`s

**Renderer, in `on_context_created` (gated)**
- Create the binding with `pub fn v8_value_create_function(name: Option<&CefString>, handler: Option<&mut V8Handler>) -> Option<V8Value>` (B:L58116).
- Attach it with `ImplV8Value::set_value_bykey(&self, key: Option<&CefString>, value: Option<&mut V8Value>, attribute: V8Propertyattribute) -> ::std::os::raw::c_int` (B:L31319) on `context.global()` (B:L29706).
- The handler implements `ImplV8Handler::execute(&self, name: Option<&CefString>, object: Option<&mut V8Value>, arguments: Option<&[Option<V8Value>]>, retval: Option<&mut Option<V8Value>>, exception: Option<&mut CefString>) -> ::std::os::raw::c_int` (B:L29973). It:
  1. creates `pub fn v8_value_create_promise() -> Option<V8Value>` (B:L58141),
  2. builds `pub fn process_message_create(name: Option<&CefString>) -> Option<ProcessMessage>` (B:L57369) and fills `argument_list()` (B:L6543) with `set_int/set_string` (B:L3099/L3103),
  3. sends it with `ImplFrame::send_process_message(&self, target_process: ProcessId, message: Option<&mut ProcessMessage>)` (B:L7581) to `ProcessId::BROWSER`,
  4. stores `(V8Context, promise)` under a request id in a `thread_local!` map.

**Browser**
- `Client::on_process_message_received` validates the sender, dispatches, and replies with `frame.send_process_message(ProcessId::RENDERER, ...)`.

**Renderer, in `on_process_message_received`**
- Look up the id, then:

```rust
ctx.enter();
promise.resolve_promise(v8_value_create_string(Some(&body)).as_mut());
// or promise.reject_promise(Some(&msg));
ctx.exit();
```

  (`resolve_promise` B:L31378, `reject_promise` B:L31380, `enter/exit` B:L29708/L29710). `cef_v8.h`: resolve and reject "should only be called from within the scope of a CefV8Handler or CefV8Accessor callback, or in combination with calling Enter() and Exit() on a stored CefV8Context reference."
- In `on_context_released`, drop the entries for that context.
- Push: the browser sends `sta.event`, and the renderer calls a registered JS function via `execute_function_with_context` (B:L31371).

**Header rules**
- `cef_frame.h` `SendProcessMessage`: "Ownership of the message contents will be transferred and the |message| reference will be invalidated. Message delivery is not guaranteed in all cases (for example, if the browser is closing, navigating, or if the target process crashes)."
- `cef_render_process_handler.h`: "V8 handles can only be accessed from the thread on which they are created."

**Verdict:** it has the lowest overhead and a native Promise API, but you re-implement everything the router already does: ids, context tracking, cancellation on navigation or crash, large payloads.

### 4.D Evaluation

| | A: message_router | B: fetch → scheme handler | C: custom V8 + ProcessMessage |
|---|---|---|---|
| Rust code you write | ~150 LoC glue (router ready-made) | ~200 LoC (async handler, auth, body parse) + push/ready handshake | ~350+ LoC in both processes |
| Renderer code | forward 3 callbacks | none | binding + promise map + lifecycle |
| Push Rust→JS | persistent query: ordered, needs no ready handshake, auto-canceled | `execute_java_script`, fire-and-forget, may run before listeners exist | manual event message → JS callback |
| Robustness | cancellation on navigate/close/crash built in; >16 KB via shared memory | each call independent; body limits (strings only); caching/CORS/fetch-flag pitfalls | all edge cases on you |
| Security | per-`Client` forwarding + renderer gating + browser-side check | global factory reachable by any page's requests; you must authorize every call (side effects) | same as A |
| Latency | 1 IPC hop each way + 1 UI task | full resource-load pipeline, several thread hops, data pipe | ≈ A (slightly lower) |
| Debuggability | console | DevTools Network tab | console |

**Recommendation: A.** It has the least bespoke code, handles lifecycle correctly, supports push natively with ordering and automatic re-subscription after reload, and gives the tightest isolation because only `UiClient` forwards messages. Work around gotchas A.1 and A.2 as described above.

### 4.E Complete Option A sketch

```rust
// src/ipc.rs
use cef::{*, wrapper::message_router::*};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::{HashMap, HashSet}, sync::{Arc, Mutex}};
use crate::scheme::UI_ORIGIN;

pub fn router_config() -> MessageRouterConfig {
    MessageRouterConfig {
        js_query_function: "__staQuery".to_string(),
        js_cancel_function: "__staQueryCancel".to_string(),
        ..Default::default()
    }
}

/// Created in main() in every process (cheap); each process uses its half.
pub struct Shared {
    pub browser_router: Arc<BrowserSideRouter>,
    pub renderer_router: Arc<RendererSideRouter>,
    pub ipc: Arc<UiIpc>,
    pub ui_browser_ids: Arc<Mutex<HashSet<i32>>>,        // browser process
    pub renderer_ui_ids: Arc<Mutex<HashMap<i32, u32>>>,  // renderer process (refcount: cross-origin
                                                         // swaps create the new browser before destroying the old one)
}

impl Shared {
    pub fn new() -> Self {
        let ui_browser_ids = Arc::new(Mutex::new(HashSet::new()));
        Self {
            browser_router: BrowserSideRouter::new(router_config()),
            renderer_router: RendererSideRouter::new(router_config()),
            ipc: Arc::new(UiIpc { ui_browser_ids: ui_browser_ids.clone(), subscribers: Mutex::new(HashMap::new()) }),
            ui_browser_ids,
            renderer_ui_ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// ---------------- browser side: handler ----------------
type Cb = Arc<Mutex<dyn BrowserSideCallback>>;

#[derive(Deserialize)]
struct Invoke { cmd: String, #[serde(default)] payload: Value }

pub struct UiIpc {
    ui_browser_ids: Arc<Mutex<HashSet<i32>>>,
    subscribers: Mutex<HashMap<i64, (i32, Cb)>>, // query_id -> (browser_id, callback)
}

fn fail(cb: &Cb, code: i32, msg: &str) { if let Ok(cb) = cb.lock() { cb.failure(code, msg); } }
fn ok(cb: &Cb, v: &Value) { if let Ok(cb) = cb.lock() { cb.success_str(&v.to_string()); } }

impl UiIpc {
    fn trusted(&self, browser: &Browser, frame: &Frame) -> bool {
        self.ui_browser_ids.lock().map(|s| s.contains(&browser.identifier())).unwrap_or(false)
            && frame.is_main() != 0
            && CefString::from(&frame.url()).to_string().starts_with(UI_ORIGIN)
    }

    /// Rust -> JS push. Callable from any browser-process thread.
    pub fn emit(&self, target_browser: Option<i32>, event: &str, payload: &impl serde::Serialize) {
        let msg = serde_json::json!({ "event": event, "payload": payload }).to_string();
        let subs = self.subscribers.lock().unwrap();
        for (browser_id, cb) in subs.values() {
            if target_browser.map_or(true, |t| t == *browser_id) {
                if let Ok(cb) = cb.lock() { cb.success_str(&msg); } // posts a UI task; never blocks on router locks
            }
        }
    }
}

impl BrowserSideHandler for UiIpc {
    fn on_query_str(
        &self,
        browser: Option<Browser>,
        frame: Option<Frame>,
        query_id: i64,
        request: &str,
        persistent: bool,
        callback: Cb,
    ) -> bool {
        // UI thread. Router lock is held: do NOT call browser_router.* here (see 4.A gotcha 2).
        let (Some(browser), Some(frame)) = (browser, frame) else { fail(&callback, 403, "no frame"); return true };
        if !self.trusted(&browser, &frame) { fail(&callback, 403, "forbidden"); return true; }
        let req: Invoke = match serde_json::from_str(request) {
            Ok(r) => r,
            Err(e) => { fail(&callback, 400, &e.to_string()); return true; }
        };
        if req.cmd == "__subscribe" {
            if !persistent { fail(&callback, 400, "__subscribe must be persistent"); return true; }
            self.subscribers.lock().unwrap().insert(query_id, (browser.identifier(), callback));
            return true; // intentionally never completed; canceled on navigate/close/crash
        }
        match crate::commands::dispatch(browser.identifier(), &req.cmd, req.payload) {
            Ok(v) => ok(&callback, &v),            // or move `callback` to a worker and answer later
            Err(e) => fail(&callback, 1, &e),
        }
        true
    }

    fn on_query_canceled(&self, _browser: Option<Browser>, _frame: Option<Frame>, query_id: i64) {
        self.subscribers.lock().unwrap().remove(&query_id);
    }
}

// ---------------- browser side: CEF handlers ----------------
wrap_browser_process_handler! {
    pub struct StaBrowserProcessHandler { shared: Arc<Shared> }
    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            let s = &self.shared;
            s.browser_router.add_handler(s.ipc.clone(), false); // UI thread required
            let mut factory = crate::scheme_factory::StaSchemeFactory::new(s.clone());
            register_scheme_handler_factory(Some(&CefString::from(crate::scheme::SCHEME)), None, Some(&mut factory));
            crate::window::create_main_window(s.clone()); // builds sidebar/overlay via create_ui_browser_view
        }
    }
}

pub fn create_ui_browser_view(shared: &Arc<Shared>, page: &str, delegate: Option<&mut BrowserViewDelegate>) -> Option<BrowserView> {
    let mut client = UiClient::new(shared.clone());
    let url = CefString::from(format!("{UI_ORIGIN}{page}").as_str());
    let mut extra = dictionary_value_create()?;                   // B:L57235
    extra.set_bool(Some(&CefString::from("sta_ui")), 1);    // B:L2451
    browser_view_create(Some(&mut client), Some(&url), Some(&BrowserSettings::default()), Some(&mut extra), None, delegate) // B:L59126
}

wrap_client! {
    pub struct UiClient { shared: Arc<Shared> }
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(UiLifeSpan::new(self.shared.clone())) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(UiRequestHandler::new(self.shared.clone())) }
        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            self.shared.browser_router
                .on_process_message_received(browser.cloned(), frame.cloned(), source_process, message.cloned())
                .into()
        }
    }
}
// TabClient (web content) simply never forwards to browser_router.

wrap_life_span_handler! {
    struct UiLifeSpan { shared: Arc<Shared> }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(b) = browser { self.shared.ui_browser_ids.lock().unwrap().insert(b.identifier()); }
        }
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            1 // UI never opens popups; route `_target_url` to the tab manager instead
        }
        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let b = browser.cloned();
            if let Some(b) = &b { self.shared.ui_browser_ids.lock().unwrap().remove(&b.identifier()); }
            self.shared.browser_router.on_before_close(b);
        }
    }
}

wrap_request_handler! {
    struct UiRequestHandler { shared: Arc<Shared> }
    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let url = request.map(|r| CefString::from(&r.url()).to_string()).unwrap_or_default();
            let is_main = frame.as_ref().map(|f| f.is_main() != 0).unwrap_or(false);
            if is_main && !url.starts_with(UI_ORIGIN) {
                // e.g. a link clicked in the sidebar: post_task(UI) -> open `url` in a tab
                return 1; // cancel: the UI browser never leaves sta://ui/
            }
            self.shared.browser_router.on_before_browse(browser.cloned(), frame.cloned()); // only when allowed
            0
        }
        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: i32,
            _error_string: Option<&CefString>,
        ) {
            self.shared.browser_router.on_render_process_terminated(browser.cloned());
            // then reload the UI browser: browser.host()... / frame.load_url(UI_ORIGIN + page)
        }
    }
}
// TabClient's RequestHandler::on_before_browse: return 1 for main-frame URLs starting with "sta://ui/".

// ---------------- renderer side ----------------
wrap_render_process_handler! {
    pub struct StaRenderProcessHandler {
        router: Arc<RendererSideRouter>,
        ui_ids: Arc<Mutex<HashMap<i32, u32>>>,
    }
    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let is_ui = extra_info.map(|d| d.bool(Some(&CefString::from("sta_ui"))) != 0).unwrap_or(false); // B:L2429
            if let (true, Some(b)) = (is_ui, browser) {
                *self.ui_ids.lock().unwrap().entry(b.identifier()).or_insert(0) += 1;
            }
        }
        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            let Some(b) = browser else { return };
            let mut m = self.ui_ids.lock().unwrap();
            if let Some(n) = m.get_mut(&b.identifier()) { *n -= 1; if *n == 0 { m.remove(&b.identifier()); } }
        }
        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            let trusted = self.ui_ids.lock().unwrap().contains_key(&browser.identifier())
                && frame.is_main() != 0
                && CefString::from(&frame.url()).to_string().starts_with(UI_ORIGIN);
            if trusted {
                // injects window.__staQuery / __staQueryCancel (READONLY|DONTENUM|DONTDELETE) before page scripts run
                self.router.on_context_created(Some(browser.clone()), Some(frame.clone()), context.cloned());
            }
        }
        fn on_context_released(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            self.router.on_context_released(browser.cloned(), frame.cloned(), context.cloned()); // superset is safe
        }
        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            self.router.on_process_message_received(browser.cloned(), frame.cloned(), Some(source_process), message.cloned()).into()
        }
    }
}
```

`commands::dispatch(browser_id: i32, cmd: &str, payload: Value) -> Result<Value, String>` is your own router. It runs on the UI thread. For slow work, move the `Cb` to a worker and call `success_str` later; that is allowed from any thread.

### 4.F JS client: `ui/src/ipc.js`, loaded first as a module

```js
const q = window.__staQuery;                // injected before any page script; undefined in untrusted contexts
if (typeof q !== 'function') throw new Error('sta IPC unavailable');

export function invoke(cmd, payload = null) {
  return new Promise((resolve, reject) => {
    q({                                      // ALWAYS pass both callbacks (cef-rs port gotcha)
      request: JSON.stringify({ cmd, payload }),
      persistent: false,
      onSuccess: (r) => { try { resolve(r === '' ? null : JSON.parse(r)); } catch (e) { reject(e); } },
      onFailure: (code, msg) => reject(Object.assign(new Error(msg), { code })),
    });
  });
}

const listeners = new Map();
export function on(event, cb) {
  let set = listeners.get(event);
  if (!set) listeners.set(event, (set = new Set()));
  set.add(cb);
  return () => set.delete(cb);
}

q({
  request: JSON.stringify({ cmd: '__subscribe', payload: null }),
  persistent: true,
  onSuccess: (raw) => {
    const { event, payload } = JSON.parse(raw);
    listeners.get(event)?.forEach((cb) => { try { cb(payload); } catch (e) { console.error(e); } });
  },
  onFailure: (code, msg) => console.warn('sta event stream closed', code, msg),
});

window.sta = Object.freeze({ invoke, on });
// Pattern: subscribe first (above), then `await invoke('state.snapshot')`; carry a monotonic `seq`
// in snapshot + events to drop duplicates emitted in between.
```

---

## 5. `Frame::execute_java_script`, `Browser::main_frame`, safe JSON

- `impl ImplFrame`: `fn execute_java_script(&self, code: Option<&CefString>, script_url: Option<&CefString>, start_line: ::std::os::raw::c_int);` (B:L7550)
- `impl ImplBrowser`: `fn main_frame(&self) -> Option<Frame>;` (B:L11733)
- Also `fn is_valid(&self)`, `fn is_main(&self) -> ::std::os::raw::c_int;` (B:L7557), and `fn url(&self) -> CefStringUserfree;` (B:L7567)
  - VERIFIED-FIX: the `is_valid` signature was incomplete. Verbatim: `impl ImplFrame`: `fn is_valid(&self) -> ::std::os::raw::c_int;` (B:L7522).

Semantics:
- `cef_browser.h`: "In the browser process this will return a valid object until after CefLifeSpanHandler::OnBeforeClose is called... The main frame object will change during cross-origin navigation or re-navigation after renderer process termination." **Call `main_frame()` fresh each time; don't cache the `Frame`.**
- `cef_frame.h`: browser-process frame methods "may be called on any thread".
- `frame_host_impl.cc` (master): `ExecuteJavaScript` → `SendToRenderFrame`, which posts to UI if needed. It **queues** the script while the renderer frame isn't bound yet, drops it with a warning if the frame is detached, and changes `start_line <= 0` to 1. The script runs in whatever document is current, so if it lands before your listeners are defined, it's a no-op.

Escaping. JSON is a subset of ECMAScript since ES2019, so `serde_json` output is a valid JS expression. Pass it as a function *argument*, never as a bare statement where `{` would parse as a block. Also escape U+2028 and U+2029, which is harmless:

```rust
fn js_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serializable")
        .replace('\u{2028}', "\\u2028")   // only legal inside JSON strings, so this stays valid JSON
        .replace('\u{2029}', "\\u2029")
}

pub fn push_via_js(browser: &Browser, event: &str, payload: &serde_json::Value) {
    let Some(frame) = browser.main_frame() else { return };
    let code = format!("window.__staDispatch?.({},{});", js_json(&event), js_json(payload));
    frame.execute_java_script(
        Some(&CefString::from(code.as_str())),
        Some(&CefString::from("sta://ui/__push.js")), // shows in DevTools stack traces
        1,
    );
}
```

---

## 6. Serving embedded assets, MIME, CSP, favicons, secure context

**Embedding: use `rust-embed` 8.x**
- `#[derive(rust_embed::Embed)] #[folder = "ui/dist/"] struct UiAssets;` then `UiAssets::get(path) -> Option<EmbeddedFile>`, where `file.data: Cow<'static, [u8]>`.
- In debug builds it reads from disk (hot reload) unless you enable the `debug-embed` feature; release builds embed the files.
- `include_bytes!` or `include_dir` also work. Still reject `..` and `\` segments before lookup.

```rust
// src/assets.rs
use crate::scheme_handler::{BytesHandler, Resp};
use cef::ResourceHandler;

#[derive(rust_embed::Embed)]
#[folder = "ui/dist/"]
struct UiAssets;

pub const UI_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob: https:; font-src 'self' data:; connect-src 'self'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'; object-src 'none'";
// optional hardening: "; require-trusted-types-for 'script'"

pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",   // module scripts need a JS MIME type
        "css" => "text/css",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain",
        _ => "application/octet-stream",     // or fall back to cef::get_mime_type (B:L58650)
    }
}

pub fn ui_asset(path: &str) -> ResourceHandler {
    let path = if path.is_empty() || path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
    if path.split('/').any(|s| s == ".." || s.contains('\\')) {
        return BytesHandler::text(400, "bad path");
    }
    let Some(file) = UiAssets::get(&path) else { return BytesHandler::text(404, "not found") };
    let mime = mime_for(&path);
    let mut resp = Resp::new(200, mime, file.data);
    resp.headers.push(("Cache-Control", "no-store".into()));
    resp.headers.push(("X-Content-Type-Options", "nosniff".into()));
    if mime == "text/html" {
        resp.headers.push(("Content-Security-Policy", UI_CSP.into()));
        resp.headers.push(("Referrer-Policy", "no-referrer".into()));
    }
    BytesHandler::from(resp)
}
```

**CSP**
- A scheme handler's headers flow through into the response (`GetResponseHeaders` copies the header map into `extra_headers`), so a CSP *header* works. `frame-ancestors` only works as a header; everything else could also go in a `<meta>` tag.
- With STANDARD, the origin is `sta://ui`, so `'self'` should match same-scheme+host URLs. If a directive doesn't match in practice, list `sta://ui` explicitly.
- **No inline scripts.** The main threat to the IPC is XSS in the UI: tab titles and URLs are attacker-controlled, so render them with `textContent`.
- **Don't set `CSP_BYPASSING`.** It adds the scheme to Chromium's `csp_bypassing_schemes`, which lets sta:// resources bypass *other* pages' CSP.

**Secure context**
- `SECURE` puts the scheme into Chromium `secure_schemes` (`scheme_registrar_impl.cc`), which makes the origin potentially trustworthy. In practice `window.isSecureContext === true` *provided the scheme is also registered in the renderer*. A CEF forum thread (magpcss.org/ceforum t=15658) reports exactly that fix, and magreenblatt suggests pairing it with `standard`.
- That gives you `crypto.subtle`, `navigator.clipboard` and friends, and https subresources raise no mixed-content warnings.

**https favicons (`<img src="https://site/favicon.ico">`)**
- Allowed with `img-src https:`. The page is a secure context, so https images aren't mixed content. With DISPLAY_ISOLATED the scheme isn't a referrer scheme, and `Referrer-Policy: no-referrer` adds belt and braces.
- `http:` icons will be auto-upgraded or blocked as mixed content.
- **Better:** fetch icons in the browser process and serve them as `sta://ui/favicon/<tab_id>`.
  - Get icon URLs from `DisplayHandler::on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>)` (B:L17613).
  - Download with `BrowserHost::download_image(&self, image_url: Option<&CefString>, is_favicon: ::std::os::raw::c_int, max_image_size: u32, bypass_cache: ::std::os::raw::c_int, callback: Option<&mut DownloadImageCallback>)` (B:L12585).
  - Encode with `Image::as_png(&self, scale_factor: f32, with_transparency: ::std::os::raw::c_int, pixel_width: Option<&mut ::std::os::raw::c_int>, pixel_height: Option<&mut ::std::os::raw::c_int>) -> Option<BinaryValue>` (B:L4110), and cache the result.
  - This keeps the UI renderer off the network, needs no https exception in CSP, and naturally reuses the tab's session.
  - VERIFIED-FIX: "reuses the tab's session" is only partly true.
    - `cef_browser.h` `DownloadImage`: "If |is_favicon| is true then cookies are not sent and not accepted during download." The request context and HTTP cache are shared, but cookies are not.
    - The images are "received from the renderer", so call `download_image` on **the tab's** `BrowserHost` (`tab_browser.host()`), not the UI browser's. Otherwise the UI browser's process does the fetch.
    - `CefDownloadImageCallback` "will be called on the browser process UI thread".

**DISPLAY_ISOLATED caveat**
- It only blocks other schemes from displaying or linking sta:// in the *renderer*. Browser-initiated loads (`frame.load_url`, `browser_view_create`) still work.
- If DevTools has trouble fetching source maps for sta:// pages, drop the flag in debug builds only.

---

## Sources (web)

- CEF source (master): [scheme_registrar_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/common/scheme_registrar_impl.cc), [resource_handler_wrapper.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/net_service/resource_handler_wrapper.cc), [stream_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/stream_impl.cc), [request_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/common/request_impl.cc), [cors_unittest.cc](https://github.com/chromiumembedded/cef/blob/master/tests/ceftests/cors_unittest.cc)
- CEF forum, secure custom scheme needs renderer registration: [magpcss.org/ceforum t=15658](https://magpcss.org/ceforum/viewtopic.php?f=6&t=15658)
- Blob POST bodies not visible to CEF (CefSharp #4784): [github.com/cefsharp/CefSharp/discussions/4784](https://github.com/cefsharp/CefSharp/discussions/4784)
- Historical: custom scheme fetch() unsupported before FETCH_ENABLED ([BriskBard forum](https://www.briskbard.com/forum/viewtopic.php?t=461), [CEF issue #2579](https://bitbucket.org/chromiumembedded/cef/issues/2579/option-to-enable-fetch-api-support-for))
- rust-embed 8.12 docs: [docs.rs/rust-embed](https://docs.rs/rust-embed/latest/rust_embed/)

---

## Verification log

Adversarial pass against the local cef 152.3.0 bindings and wrapper sources, CEF 152.0.6 headers and `libcef_dll`, and CEF master sources. Copies of the fetched sources are in `scratchpad/research/ipcsrc/`: `frame_host_impl.cc`, `v8_impl.cc`, `stream_reader_url_loader.cc`, `throttle_handler.cc`, `cors_unittest.cc`, and rust-embed 8.12.0 crates. About 125 claims were checked.

### Confirmed exactly (name, parameter types, return type, trait, line number)

**Scheme registration**
- `on_register_custom_schemes` L33653
- `SchemeRegistrar` L33366 and `add_custom_scheme` L33369
- `SchemeOptions` L49996, its constants L50019-50034, and `get_raw` L50038 (a newtype, not bitflags)
- `execute_process` L58226, `initialize` L58252, `api_hash` L56201
- `Settings.no_sandbox: c_int`, `impl Default for Settings` and `BrowserSettings`

**wrap_*! macros**
- All 11 cited macro lines.
- Struct and unit forms, `new(fields in order) -> X`, and `Clone` cloning every field.
- The `$arg_name:ident` restriction.
- One impl block for non-Views interfaces, several for Views delegates.

**Resource handling**
- `register_scheme_handler_factory` (global L58192, and on `RequestContext` L11116)
- `SchemeHandlerFactory::create` L33471
- All `ImplResourceHandler` methods, L24743-24799; `read` really takes a raw `*mut u8`
- `ImplCallback`, `ResourceReadCallback`, `ResourceSkipCallback`
- `ImplResponse` and `ImplRequest`, all cited lines
- `stream_reader_create_for_data`, `_for_handler`, `_for_file`, and `get_mime_type`
- `CefStringMultimap::new`/`append`, `CefString` `From<&str>` and `From<&CefStringUserfree>`, `Display`, `try_set` (STR:L571)
- `CefStringMultimap` and the other string types are re-exported through `cef::*` (bindings L66-81)

**Post data**
- `PostData` (L7082-7086) and `PostDataElement` (L7276-7282)
- `PostdataelementType` derives `PartialEq`
- `elements()` counts from `arg.len()` and truncates (L7137-7175). C++ ctocpp uses `max(GetElementCount(), size)`, so the Rust port differs as the report says.

**Browser, frame and process-message APIs**
- `post_task`, `currently_on`, `ThreadId` constants
- `Browser::identifier` L11725 and `main_frame` L11733
- `Frame::execute_java_script` L7550, `is_main` L7557, `url` L7567, `send_process_message` L7581
- `ResourceRequestHandler::resource_handler` L25515

**V8 and handler hooks**
- The V8 functions, `set_value_bykey`, `V8Handler::execute` L29973, `resolve_promise`/`reject_promise`, `enter`/`exit`, `execute_function_with_context`
- `Client`, `LifeSpanHandler` (including the full 13-parameter `on_before_popup`), `RequestHandler`, `RenderProcessHandler` and `BrowserProcessHandler` hooks
- `browser_view_create` L59126, `dictionary_value_create`, `DictionaryValue::bool`/`set_bool`
- `on_favicon_urlchange` L17613, `download_image` L12585, `Image::as_png` L4110

**Out-parameter behaviour**
- `WrapParamRef` write-back on drop (RC:L123-145).
- `Option<&mut CefString>` out-params become `CefStringData::BorrowedMut`; assigning to them never reaches C, so use `try_set`.
- The V8 `retval` write-back at B:L30069-30072.
- `RefGuard` is `Send + Sync` (RC:L283-284).

**Message router (all MR line numbers verified)**
- The trait and method signatures.
- The renderer-side `source_process: Option<ProcessId>`.
- `on_process_message_received` holds the `browser_query_info_map` guard while calling handlers. The `query_id_generator` and `handlers` guards are released at the end of the match arm.
- `cancel_pending_for` holds the map lock while calling `on_query_canceled`.
- `*exception = CefString::from(..)` is lost. The C++ `FunctionCallbackImpl` only throws if `exception` is non-empty (`v8_impl.cc`).
- `GetValue(key)` returns an *undefined* value, not NULL, for a missing key (`v8_impl.cc` L2100-2122). C++ `CefMessageRouter` uses `HasValue`, so gotcha A.1 (omitting `onSuccess`/`onFailure`) is real.
- Persistent success keeps the entry.
- Unhandled queries fail with -1 and "The query has been canceled".
- The shared-memory path is UTF-8 on both sides.
- `Rc` inside a `Task` (MRU:L679).
- String requests become `MessagePayload::String` (`build_renderer_message` checks `is_string`), so `on_query_str` is the right hook.
- An empty string payload is still delivered as a string, because `ListValue::SetString` takes `optional_param=value`.

**Header and CEF source semantics**
- Quotes from `cef_app.h`, `cef_scheme.h`, `cef_types.h` (scheme options), `cef_resource_handler.h`, `cef_message_router.h` (persistence A/B/C, Subscription/Broadcast, "must then be passed into all router callbacks"), `cef_browser.h` (`GetMainFrame`), `cef_frame.h` (`SendProcessMessage`, any thread), `cef_v8.h` (promise scope), `cef_render_process_handler.h` (cross-origin browser created before the old one is destroyed, which justifies the refcount; `extra_info` reaches `BrowserView` browsers).
- `scheme_registrar_impl.cc`: the referrer-scheme exclusion.
- `render_manager.cc`: DISPLAY_ISOLATED and FETCH_ENABLED are registered in Blink.
- `resource_handler_wrapper.cc`: the `Open`/`GetResponseHeaders`/`Read`/`Skip`/`Cancel` threads and return-value tables.
- `stream_impl.cc`: `CreateForData` copies, and returns NULL for size 0.
- `request_impl.cc`: only Bytes and File post elements are converted.
- `frame_host_impl.cc`: `ExecuteJavaScript` posts to UI, queues while unbound, drops when detached, and changes `startLine <= 0` to 1.

**Resource manager and rust-embed**
- RM deadlock (RM:L906/970/138/149) and the inverted MIME logic (RM:L281-285).
- rust-embed 8.12: `#[derive(Embed)]` exists, it generates an inherent `pub fn get(&str) -> Option<EmbeddedFile>`, and `EmbeddedFile.data: Cow<'static, [u8]>`.

**Snippets reviewed for type-correctness (none were compiled)**
- `Option<&mut T>::cloned()`, `bool.into()` to `i32`, `Arc<UiIpc>` coercing to `Arc<dyn BrowserSideHandler>`
- MutexGuard deref coercions in `copy_out`/`fill_headers`, and `Cow` conversions
- The `let-else` patterns and disjoint field borrows in `dispatch(..., &req.cmd, req.payload)`
- Lock-order analysis of `UiIpc::emit` (`subscribers` then callback mutex) against the router: no deadlock cycle, because the router's map-lock holders all run on the UI thread

### Errors found and fixed inline (marked VERIFIED-FIX)
1. **SRH `response_length`** is not "always -1". It is `-1` when there is a stream and `0` when there isn't (SRH:L64).
2. **SRH with `stream: None`**: `read` returns true with 0 bytes and never keeps the callback. The report called this an empty body; per the CEF source (`resource_handler_wrapper.cc` `~ReadCallbackWrapper`, and `stream_reader_url_loader.cc` `ReadMore`, which reads regardless of length) it most likely fails with `ERR_FAILED`. Fixed in 2.3 and in the snippet comment.
3. **`Read` return table** overlapped: `-1` means legacy `ReadResponse`, and errors are `<= -2`. Also added that `true` + 0 bytes without keeping the callback leads to `ERR_FAILED`.
4. **tests_shared citation** `resource_util/win.rs:419` corrected to `:74-75`.
5. **`cors_unittest.cc`** is not GET-only; it has form-POST redirect tests. Its XHR/fetch sub-requests are GET only.
6. **`MessageRouterBrowserSide`** listing was missing the associated `type Callback` (MR:L130). Threshold wording tightened to `>=`.
7. **New port bug, gotcha A.5**: `on_callback_failure` uses `always_remove=false` (MR:L542, where C++ `cef_message_router.cc` L456 uses `true`), leaving stale persistent entries and firing `on_query_canceled` later.
8. **Gotcha A.2 extended**: synchronous `OnBeforeBrowse` re-entry from a navigation started inside a handler (`throttle_handler.cc` uses `SynchronyMode::kSync`). Rated PLAUSIBLE; the advice is to defer navigation or closing of UI browsers with `post_task`.
9. **Option B `ApiTask`** called `dispatch` with `&Vec<u8>`, which contradicts the 4.E signature (`Value`). Fixed.
10. **`Frame::is_valid`** signature was incomplete. Now verbatim at B:L7522.
11. **Favicons via `DownloadImage`**: with `is_favicon=1` no cookies are sent, the call must go to the tab's `BrowserHost`, and the callback runs on the UI thread.
12. **Additions**:
    - The macro `ident` matcher rejects `mut` parameter patterns.
    - Views delegate base blocks are mandatory and ordered.
    - `V8Value`/`V8Context` are `Send + Sync` at the type level but still thread-bound.
    - `ResourceManager` has a second self-lock on the request mutex.

No error was found in the core recommendation (Option A, scheme flags, `App` passed to `execute_process`, the `BytesHandler` design, or the `try_set` out-param gotcha).

### Still uncertain / not verified
- **Secure context:** `window.isSecureContext === true` for `SECURE` custom schemes, and the magpcss forum thread t=15658 that supports it. The forum timed out and could not be fetched. The mechanism (`secure_schemes` are registered in every process through `ContentClient`) is consistent with the source.
- **CSP header on custom-scheme documents:** I believe Chromium parses headers for non-network loaders, but did not check. Verify in DevTools; a `<meta>` CSP is the fallback, except for `frame-ancestors`.
- **Whether `LoadURL` reaches `OnBeforeBrowse` synchronously** on the calling stack (item 8).
- **CEF master versus the CEF 152 release branch:** the fetched files may differ in details. The logic cited is long-standing.
- **CefSharp discussion #4784:** not re-fetched.
- **Whether a web page's cross-origin `fetch` POST to `sta://ui/api`** is blocked before it reaches the factory: not tested. Keep the server-side authorization in any case.
- **Nothing was compiled or run.** The snippets were only reviewed by reading them against the bindings.
