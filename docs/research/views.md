# CEF Views for sta's window (CEF 152.0.6 / `cef` 152.3.0)

Where the facts come from:
- Rust signatures: copied from `cef-152.3.0+152.0.6/src/bindings/x86_64_pc_windows_msvc.rs`. `L<n>` means a line number in that file.
- C++ quotes: the local headers in `.cef/152.0.6/cef_windows_x86_64/include/`.
- Behaviour notes: CEF / Chromium `master` sources downloaded on 2026-09-16. They match what the 152 headers describe. Sources are listed at the end.
- **The skeleton in §7 passed `cargo check` against `cef = "=152.3.0"`** (probe crate: `scratchpad/research/viewsprobe`, file `src/shell.rs`). The macro gotchas in §1 were also confirmed with the compiler.
- VERIFIED-FIX: the "master" sources were re-checked against the exact CEF 152 commit (`708dc140`, `cef_version.h`) and Chromium tag `152.0.7977.83`. Most code is identical. The main difference is that `CefWindowView::CloseOverlayViews()` does not exist in 152: `CefWindowView::WindowClosing()` closes the overlay hosts inline. The §7 skeleton **as printed in this file** was extracted verbatim and re-compiled (`scratchpad/research/verifyprobe`, `cargo check` exit 0), including the edits marked VERIFIED-FIX below.

---

## 0. Summary

| Question | Answer |
|---|---|
| Several BrowserViews in one Window? | **Yes.** `cef_types_runtime.h`: *"Alloy style Windows with the Views framework can host only Alloy style BrowserViews but Chrome style Windows can host both style BrowserViews. Additionally, a Chrome style Window can host at most one Chrome style BrowserView but potentially multiple Alloy style BrowserViews."* CEF enforces this at runtime (`browser_view_impl.cc`: `"Cannot add Chrome style BrowserView to Alloy style Window"`, `"Cannot add multiple Chrome style BrowserViews"`). |
| Which styles? | **Alloy for the Window and for every BrowserView** (sidebar, tabs, command bar). Reasons: many tabs rules out Chrome style; overlays that hold a BrowserView must be Alloy; our tab-close flow needs `LifeSpanHandler::do_close`, which is Alloy-only. |
| Reparent vs hide? | Reparenting is supported: removal hands ownership back to our reference and the browser survives. **Still, keep tabs attached and hide them with `set_visible(0)`.** Hidden views drop out of BoxLayout and their WebContents become HIDDEN. VERIFIED-FIX: detaching does **not** re-theme the Widget on every switch. `CefWidgetImpl::AddAssociatedProfile`/`RemoveAssociatedProfile` are ref-counted per Profile, and a theme change fires only when the theme Profile changes. Reparent only to move a tab to another Window or overlay. |
| BrowserView in an overlay? | Yes, Alloy only. Use `add_overlay_view(.., DockingMode::CUSTOM, can_activate = 1)` and then `request_focus()`. Keyboard focus works. The page is **opaque** (a windowed browser can't be transparent). |
| Frameless + `-webkit-app-region`? | **Not automatic.** Implement `DragHandler::on_draggable_regions_changed`, convert each region origin with `browser_view.convert_point_to_window`, then call `window.set_draggable_regions`. CEF adds a 4 DIP resize border (16 DIP corners). A draggable point returns `HTCAPTION`, so the window moves natively and Aero Snap should work. |
| **Rust-only traps** | (1) `can_resize`, `can_maximize`, `can_minimize` and `can_close` **default to 0 in Rust** (C++ defaults to true), so you must override them. (2) `Size{w, 0}` from `preferred_size` is treated as empty and ignored. (3) Calling `set_accelerator` again with the same `command_id` replaces the earlier binding. (4) A RefCell double borrow inside a callback is a panic inside `extern "C"`, which aborts the process. (5) `wrap_window_delegate!{ struct X; ... }` doesn't compile; write `struct X {}`. (6) For Views-hosted Alloy browsers, returning 0 from `do_close` **closes the whole top-level Window**. VERIFIED-FIX: (2), (3) and (6) are CEF behaviours, not Rust-specific. For (6), `CloseHostWindow()` closes `root_view()->GetWidget()`. For a BrowserView inside an overlay that is the overlay's child Widget, not the top-level Window. |

---

## 1. The `wrap_*!` macros

### 1.1 Shape (macros at L37209, L37739, L41531, L43304)

```text
wrap_view_delegate!         { [vis] struct Name { f: T, ... }  impl ViewDelegate { ... } }
wrap_panel_delegate!        { [vis] struct Name { ... }  impl ViewDelegate { ... }  impl PanelDelegate { ... } }
wrap_browser_view_delegate! { [vis] struct Name { ... }  impl ViewDelegate { ... }  impl BrowserViewDelegate { ... } }
wrap_window_delegate!       { [vis] struct Name { ... }  impl ViewDelegate { ... }  impl PanelDelegate { ... }  impl WindowDelegate { ... } }
```

Rules (compiler-verified in `viewsprobe/src/gotchas.rs`):
- **List every base block, in base-to-derived order, even when it's empty.** Leaving out `impl PanelDelegate {}` fails with `no rules expected 'WindowDelegate'`. VERIFIED-FIX: swapping the order (`impl PanelDelegate {} impl ViewDelegate {}`) fails with `no rules expected 'PanelDelegate'` (re-tested).
- **The unit-struct form is broken for multi-base macros.** `wrap_window_delegate!{ struct X; impl ViewDelegate{} impl PanelDelegate{} impl WindowDelegate{} }` and `wrap_browser_view_delegate!{ struct X; ... }` fail to compile, because the first macro arm drops the base blocks when it forwards. Use `struct X {}`. The single-base `wrap_view_delegate!{ struct X; impl ViewDelegate {} }` does compile.
- Fields: `vis name: Type`. Generics use the `struct X<T: Bound> {..}` syntax. **Every field type must be `Clone`**, because the macro generates `impl Clone` that calls `.clone()` on each field. A `std::sync::Mutex<i32>` field gives `E0599: no method named clone`. `Rc<RefCell<_>>`, `Rc<Cell<_>>` and `Arc<Mutex<_>>` all work.
- **No `Send`/`Sync` bound on fields.** `Rc<RefCell<..>>` compiles, and that is fine because every Views callback runs on the UI thread. However, the generated handle types (`BrowserView`, `Window`, `ViewDelegate`, ...) are `Send + Sync` through `unsafe impl<T: Rc> Send/Sync for RefGuard<T>` (`rc.rs` L283-284). The compiler won't stop you moving them to another thread. Don't do it.
- Method bodies must use the **exact** trait signature (tables below). Anything you don't override keeps the Rust trait default.

### 1.2 What `::new` does

The macro generates `impl Name { pub fn new(<fields in declaration order>) -> WindowDelegate { WindowDelegate::new(Self { fields..., cef_object: null }) } }`. `WindowDelegate::new` (L43158) runs:

```rust
let mut cef_object = std::mem::zeroed();
<T as ImplWindowDelegate>::init_methods(&mut cef_object);   // fills view + panel + window fn ptrs
let object = RcImpl::new(cef_object, interface);            // Box, refcount = 1
<T as WrapWindowDelegate>::wrap_rc(&mut (*object).interface, object);
object.wrap_result()                                        // WindowDelegate(RefGuard)
```

Consequences:
- Callbacks receive `&self`, which is the single struct stored inside the `RcImpl`. State in `RefCell`/`Cell` fields is therefore shared across callbacks.
- `init_methods` installs **every** C function pointer. CEF always calls the Rust trait method, so **the Rust defaults (`Default::default()`) replace the C++ defaults.** These differ:

| Method (bindings line) | Rust default | C++ default (header) | Effect |
|---|---|---|---|
| `ImplWindowDelegate::can_resize` (L43241) | `0` | `true` | Window isn't resizable, and there is no `WS_THICKFRAME`. |
| `can_maximize` (L43245) / `can_minimize` (L43249) | `0` | `true` | Maximize/minimize disabled. |
| `can_close` (L43253) | `0` | `true` | `window.close()` and Alt+F4 do nothing. cefsimple overrides only this one. |
| `with_standard_window_buttons` (L43225) | `0` | `!IsFrameless()` | macOS only. |
| `ImplBrowserViewDelegate::delegate_for_popup_browser_view` (L37670) | `None` | `this` | A popup BrowserView gets no delegate (its own nested popups then get a default Window). |
| `chrome_toolbar_type` (L37689) | `ChromeToolbarType::UNKNOWN` | `CEF_CTT_NONE` | Harmless: CEF treats UNKNOWN as "no toolbar" (`chrome_browser_view.cc`). |

Everything else matches: `ShowState::NORMAL`, `RuntimeStyle::DEFAULT`, `State::DEFAULT`, `Rect`/`Size` zero, and `0` for booleans.

- The generated `impl Clone for Name` calls `add_ref()` and clones each field (a `RefCell` field becomes an independent copy). There is **no matching `Drop`**, so each clone leaks a reference. Clone the returned handle (`WindowDelegate`, `BrowserViewDelegate`) instead. (cef-rs PR #469, still open, adds a struct-only form with ordinary `impl ImplWindowDelegate for X {}` blocks. It isn't in 152.3.0.)

### 1.3 Threads, re-entrancy, panics
- Headers: *"The methods of this class will be called on the browser process UI thread"* (`cef_window_delegate.h`, `cef_view_delegate.h`, `cef_browser_view_delegate.h`) and *"Methods must be called on the browser process UI thread unless otherwise indicated"* (`cef_window.h`, `cef_view.h`, `cef_panel.h`). Messages from the renderer (IPC bridge) already arrive on the UI thread. From any other thread, use `post_task(ThreadId::UI, Some(&mut task))`: `pub fn post_task(thread_id: ThreadId, task: Option<&mut Task>) -> ::std::os::raw::c_int` (L57830), and don't capture `Rc` in that task.
- **Many CEF calls invoke delegate callbacks synchronously:**

| You call | Callback that fires inside the call |
|---|---|
| `window_create_top_level` | `on_window_created` runs **inside** `CefWindowImpl::Create → CreateWidget` (`window_impl.cc`), after `initial_bounds`, `is_frameless` and `can_resize`. A BrowserView added there creates its browser **before the call returns**, so its `on_browser_created` (and even its first `on_load_end`) can arrive while the caller still holds nothing: record the operation *before* creating the window (`ext_backend.rs`, gate S7). A Window with **no layout manager** leaves that view zero-sized, and a Chrome-style browser in a zero-sized view never commits its navigation. |
| `add_child_view(browser_view)` on an attached panel | VERIFIED-FIX (order, from Chromium `View::AddChildViewAtImpl` and CEF `CefBrowserViewView::AddedToWidget`): `on_theme_changed` → `on_parent_view_changed` → `on_window_changed` → browser creation (`CefBrowserViewImpl::AddedToWidget → CefBrowserHostBase::Create`) → `LifeSpanHandler::on_after_created` → `on_browser_created`. If this is the first browser of its Profile in the Widget, a second theme pass (`on_theme_colors_changed`/`on_theme_changed`) follows **asynchronously**. |
| `layout()` / `set_visible` / resizing | `preferred_size`, `on_layout_changed`, `on_blur`. |
| `window.close()` | `can_close`. |
| `host.close_browser(0)` on a page without unload handlers | `do_close` runs synchronously inside the call (VERIFIED-FIX addition: `AlloyBrowserHostImpl::CloseBrowser → CloseContents`). |
| Dropping the last reference to a detached BrowserView | Browser destruction and `on_before_close`. VERIFIED-FIX: this is synchronous only if no beforeunload/unload dispatch is needed. cefclient: *"OnBeforeClose ... may be called synchronously or asynchronously"*. |

- A `RefCell` borrow held across one of these calls means the callback panics. A panic inside `extern "C"` **aborts the process**.

  Pattern: copy the handles out in a `let` statement, let the borrow end, then call CEF. **Avoid `if let Some(w) = shell.borrow().window.clone() { w.close() }`**: the `Ref` temporary lives for the whole `if let` body.

  Keep values that `preferred_size` reads in a separate `Rc<Cell<_>>`, so layout never touches the RefCell.

---

## 2. Window creation and `WindowDelegate`

`pub fn window_create_top_level(delegate: Option<&mut WindowDelegate>) -> Option<Window>` (L59477). Call it on the UI thread, for example in `BrowserProcessHandler::on_context_initialized`. *"Windows are hidden by default"* (`cef_view.h` SetVisible), so call `window.show()` in `on_window_created`.

| Rust signature (trait `ImplWindowDelegate`) | Line | Semantics |
|---|---|---|
| `fn on_window_created(&self, window: Option<&mut Window>)` | L43178 | Synchronous during creation. Build the view tree here, set accelerators (`SetAccelerator` returns early `if (!widget_)`), then `show()`. |
| `fn on_window_closing(&self, window: Option<&mut Window>)` | L43180 | Overlays are already closed. VERIFIED-FIX: in 152, `CefWindowView::WindowClosing()` loops `overlay_hosts_` and calls `overlay_host->Close()` inline before `OnWindowClosing()`. `CloseOverlayViews()` exists only on CEF master. |
| `fn on_window_destroyed(&self, window: Option<&mut Window>)` | L43182 | *"Release all references to |window| and do not attempt to execute any methods on |window| after this callback returns."* Also release **detached** BrowserViews, overlay BrowserViews and the `OverlayController`. Otherwise their browsers stay alive and `on_before_close` never fires. |
| `fn can_close(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int` | L43253 | *"called for user-initiated window close actions and when CefWindow::Close() is called."* Use it with `try_close_browser` / `close_browser`. **Override it.** |
| `fn is_frameless(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int` | L43221 | *"The window will be resizable if CanResize() returns true. Use CefWindow::SetDraggableRegions() to specify draggable regions."* |
| `fn can_resize(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int` | L43241 | **Rust default 0.** Read once at widget creation (`params.delegate->SetCanResize`). |
| `fn can_maximize(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int` / `fn can_minimize(...)` | L43245 / L43249 | **Rust default 0.** VERIFIED-FIX (detail): these do not block `window.maximize()`/`minimize()`. They control `WS_MAXIMIZEBOX`/`WS_MINIMIZEBOX` (`widget_hwnd_utils.cc`; later `HWNDMessageHandler::SizeConstraintsChanged`) and enable the system-menu (Alt+Space) items (`hwnd_message_handler.cc`). For frameless windows, `remove_standard_frame` strips MIN/MAX box at creation anyway. |
| `fn initial_bounds(&self, window: Option<&mut Window>) -> Rect` | L43213 | DIP. *"If this method returns an empty CefRect then GetPreferredSize() will be called ... and the window will be placed on the screen with origin (0,0)."* cefclient returns an explicit size for frameless windows. |
| `fn initial_show_state(&self, window: Option<&mut Window>) -> ShowState` | L43217 | `ShowState::{NORMAL, MINIMIZED, MAXIMIZED, FULLSCREEN, HIDDEN}` (HIDDEN is macOS only; on Windows it maps to minimized). |
| `fn window_runtime_style(&self) -> RuntimeStyle` | L43280 | `RuntimeStyle::ALLOY` (L45702). Chrome style is the default unless Alloy is requested. |
| `fn on_window_bounds_changed(&self, window: Option<&mut Window>, new_bounds: Option<&Rect>)` | L43191 | *"|new_bounds| will be in DIP screen coordinates."* Use it to persist bounds and update the maximize glyph. |
| `fn on_window_activation_changed(&self, window: Option<&mut Window>, active: ::std::os::raw::c_int)` | L43184 | |
| `fn on_window_fullscreen_transition(&self, window: Option<&mut Window>, is_completed: ::std::os::raw::c_int)` | L43193 | Synchronous on Windows. *"With Alloy style you must also implement CefDisplayHandler::OnFullscreenModeChange to handle fullscreen transitions initiated by browser content."* That handler is `ImplDisplayHandler::on_fullscreen_mode_change` (L17620): call `window.set_fullscreen(..)` there and hide the sidebar. |
| `fn with_standard_window_buttons(&self, window: Option<&mut Window>) -> ::std::os::raw::c_int` | L43225 | *"only supported on macOS."* |
| `fn titlebar_height(&self, window: Option<&mut Window>, titlebar_height: Option<&mut f32>) -> ::std::os::raw::c_int` | L43229 | VERIFIED-FIX: not macOS-only. On macOS it moves the traffic lights. On every platform a returned height is used by `CefWindowView::UpdateBoundingBox` (`window_view.cc`) to keep CEF-positioned dialogs and the find bar below a custom title bar. Useful for a frameless window with an HTML top strip. |
| `fn on_accelerator(&self, window: Option<&mut Window>, command_id: ::std::os::raw::c_int) -> ::std::os::raw::c_int` | L43257 | Called for accelerators registered with `set_accelerator`. |
| `fn on_key_event(&self, window: Option<&mut Window>, event: Option<&KeyEvent>) -> ::std::os::raw::c_int` | L43265 | *"Called after all other controls in the window have had a chance to handle the event."* When a BrowserView has focus, `BrowserViewImpl::HandleKeyboardEvent` runs accelerators first, then this callback, then default handling. So this sees only keys the page didn't consume. `KeyEvent` (L1155) has `type_: KeyEventType`, `modifiers: u32`, `windows_key_code`, etc. |
| `fn minimum_size(&self, view: Option<&mut View>) -> Size` | L37161 (ViewDelegate) | The Window is a View, so this sets the minimum window size. |
| `fn on_layout_changed(&self, view: Option<&mut View>, new_bounds: Option<&Rect>)` | L37195 (ViewDelegate) | Runs after every layout of the window root view (`view_view.h` `Layout()`). **Reposition CUSTOM overlays here**, as `cef_window.h` recommends. |

**Accelerators:** `fn set_accelerator(&self, command_id: ::std::os::raw::c_int, key_code: ::std::os::raw::c_int, shift_pressed: ::std::os::raw::c_int, ctrl_pressed: ::std::os::raw::c_int, alt_pressed: ::std::os::raw::c_int, high_priority: ::std::os::raw::c_int)` (L44331), plus `remove_accelerator` (L44341) and `remove_all_accelerators` (L44343).
- `key_code` is a Windows VK code, for example `0x54` for 'T'.
- *"If |high_priority| is true then the key event will not be forwarded to the web content (`keydown` event handler) or CefKeyboardHandler first. If |high_priority| is false then the behavior will depend on the CefBrowserView::SetPreferAccelerators configuration."*
- **`accelerator_map_` is keyed by `command_id`** (`window_impl.cc` L822-825 calls `RemoveAccelerator(command_id)` first). Two shortcuts therefore need two ids.

---

## 3. Window methods (`ImplWindow`, plus the inherited `ImplPanel` and `ImplView`)

| Rust signature | Line | Notes |
|---|---|---|
| `fn show(&self)` / `fn hide(&self)` | L44244 / L44248 | |
| `fn close(&self)` | L44252 | Goes through `can_close`. `fn is_closed(&self) -> ::std::os::raw::c_int` (L44254). |
| `fn minimize(&self)` / `fn maximize(&self)` / `fn restore(&self)` | L44270 / L44268 / L44272 | |
| `fn is_maximized(&self) -> ::std::os::raw::c_int` / `is_minimized` / `is_fullscreen` | L44276 / L44278 / L44280 | |
| `fn set_fullscreen(&self, fullscreen: ::std::os::raw::c_int)` | L44274 | Fires `on_window_fullscreen_transition`. |
| `fn activate(&self)` / `fn bring_to_top(&self)` / `fn set_always_on_top(&self, on_top: ::std::os::raw::c_int)` | L44256 / L44262 / L44264 | There is no Window-specific `request_focus`. Use `activate()`, then `some_view.request_focus()`. |
| `fn center_window(&self, size: Option<&Size>)` | L44250 | *"Sizes the Window to |size| and centers it in the current display."* |
| `fn set_title(&self, title: Option<&CefString>)` | L44284 | Taskbar and Alt+Tab text, even when frameless. |
| `fn set_window_icon(&self, image: Option<&mut Image>)` / `fn set_window_app_icon(&self, image: Option<&mut Image>)` | L44288 / L44292 | 16x16 icon versus ICON_BIG (taskbar/Alt+Tab). Build the image with `pub fn image_create() -> Option<Image>` (L57259) and `fn add_png(&self, scale_factor: f32, png_data: Option<&[u8]>) -> ::std::os::raw::c_int` (L4081). |
| `fn set_background_color(&self, color: u32)` | L38385 (ImplView) | ARGB. *"The background color will be automatically reset when CefViewDelegate::OnThemeChanged is called"*, and that happens when a view is added to a Window. **Re-apply it in `on_theme_changed` (L37201).** |
| `fn set_draggable_regions(&self, regions: Option<&[DraggableRegion]>)` | L44316 | *"Call this method with an empty vector to clear ... The draggable region bounds should be in window coordinates."* Regions are applied in order: `draggable` is unioned in, `!draggable` is subtracted (`window_view.cc` SkRegion ops). |
| `fn window_handle(&self) -> cef_window_handle_t` | L44318 | On Windows `cef_window_handle_t = HWND` and `pub struct HWND(pub *mut HWND__)` (cef-dll-sys). Raw pointer: `window.window_handle().0.cast::<c_void>()`. |
| `fn bounds(&self) -> Rect` / `fn set_bounds(&self, bounds: Option<&Rect>)` | L38337 / L38335 | For a Window these are **DIP screen coordinates** (*"or DIP screen coordinates if there is no parent"*). |
| `fn client_area_bounds_in_screen(&self) -> Rect` | L44314 | Use its width and height to place overlays (overlay coordinates are window-relative). |
| `fn request_focus(&self)` | L38383 (ImplView) | For a BrowserView this is **asynchronous** (`CEF_POST_TASK ... work around issue #3040`). |
| `fn add_overlay_view(&self, view: Option<&mut View>, docking_mode: DockingMode, can_activate: ::std::os::raw::c_int) -> Option<OverlayController>` | L44296 | See §6c. |
| `fn display(&self) -> Option<Display>` / `fn focused_view(&self) -> Option<View>` | L44312 / L44282 | |
| `fn set_theme_color(&self, color_id: ::std::os::raw::c_int, color: u32)` / `fn theme_changed(&self)` | L44345 / L44347 | |
| `fn runtime_style(&self) -> RuntimeStyle` | L44349 | |

**Building a `DraggableRegion` (L933):**

```rust
pub struct DraggableRegion { pub bounds: Rect, pub draggable: ::std::os::raw::c_int }
pub struct Rect  { pub x: c_int, pub y: c_int, pub width: c_int, pub height: c_int }   // L195
pub struct Point { pub x: c_int, pub y: c_int }                                        // L162

// Regions come from DragHandler in *BrowserView* (DIP) coordinates. Convert them to *window*
// coordinates the way cefclient does: convert the origin, keep the size.
let mut origin = Point { x: r.bounds.x, y: r.bounds.y };
sidebar_view.convert_point_to_window(Some(&mut origin));  // fn convert_point_to_window(&self, point: Option<&mut Point>) -> c_int  L38395
out.push(DraggableRegion { bounds: Rect { x: origin.x, y: origin.y, ..r.bounds.clone() }, draggable: r.draggable });
window.set_draggable_regions(Some(&out));
```

A view at a non-zero offset (for example a sidebar placed below a top strip) is handled by `convert_point_to_window`. Recompute whenever the view moves or resizes, the sidebar is hidden, or an overlay appears or moves. Add `{bounds: overlay.bounds(), draggable: 0}` for each visible overlay, because regions are also active underneath overlays (cefclient `ViewsOverlayBrowser::UpdateDraggableRegions`).

---

## 4. Panels and layouts

| Rust signature | Line | Notes |
|---|---|---|
| `pub fn panel_create(delegate: Option<&mut PanelDelegate>) -> Option<Panel>` | L59420 | |
| `fn set_to_box_layout(&self, settings: Option<&BoxLayoutSettings>) -> Option<BoxLayout>` | L41646 (ImplPanel) | |
| `fn set_to_fill_layout(&self) -> Option<FillLayout>` | L41644 | |
| `fn get_layout(&self) -> Option<Layout>` / `fn layout(&self)` | L41648 / L41650 | `layout()` lays out now. |
| `fn add_child_view(&self, view: Option<&mut View>)` | L41652 | Takes ownership (`PassOwnership`). |
| `fn add_child_view_at(&self, view: Option<&mut View>, index: ::std::os::raw::c_int)` | L41654 | |
| `fn reorder_child_view(&self, view: Option<&mut View>, index: ::std::os::raw::c_int)` | L41656 | *"A negative value for |index| will move the View to the end."* |
| `fn remove_child_view(&self, view: Option<&mut View>)` | L41658 | *"The View can then be added to another Panel."* Ownership goes back to the `CefView` (`ResumeOwnership`). |
| `fn remove_all_child_views(&self)` | L41660 | *"The removed Views will be deleted if the client holds no references to them."* |
| `fn child_view_count(&self) -> usize` / `fn child_view_at(&self, index: ::std::os::raw::c_int) -> Option<View>` | L41662 / L41664 | |
| `fn set_flex_for_view(&self, view: Option<&mut View>, flex: ::std::os::raw::c_int)` / `fn clear_flex_for_view(&self, view: Option<&mut View>)` | L37040 / L37042 (ImplBoxLayout) | |
| `fn set_visible(&self, visible: ::std::os::raw::c_int)` / `fn is_visible` / `fn is_drawn` | L38365 / L38367 / L38369 | |
| `fn set_bounds(&self, bounds: Option<&Rect>)` / `fn set_size(&self, size: Option<&Size>)` / `fn set_position(&self, position: Option<&Point>)` | L38335 / L38341 / L38345 | Parent coordinates. **The parent's layout manager overwrites these on the next layout.** |
| `fn preferred_size(&self) -> Size` / `fn size_to_preferred_size(&self)` | L38353 / L38355 | |
| `fn invalidate_layout(&self)` | L38363 | *"Indicate that this View and all parent Views require a re-layout."* |
| `fn set_id(&self, id: c_int)` / `fn view_for_id(&self, id: c_int) -> Option<View>` / `fn is_attached(&self) -> c_int` / `fn parent_view(&self) -> Option<View>` | L38325 / L38333 / L38315 / L38331 | VERIFIED-FIX (notation): wherever this report writes `c_int`, the bindings say `::std::os::raw::c_int`. The names and lines are otherwise exact. |

Conversions (all add a reference): `impl From<&BrowserView> for View` (L39340), `impl From<&Panel> for View` (L41838), `impl From<&Window> for View` (L44523), `impl From<&Window> for Panel` (L44569). Typical call: `panel.add_child_view(Some(&mut View::from(&browser_view)))`.

**`BoxLayoutSettings` (L1395).** Fields: `size`, `horizontal: c_int`, `inside_border_horizontal_spacing`, `inside_border_vertical_spacing`, `inside_border_insets: Insets {top,left,bottom,right}` (L267), `between_child_spacing`, `main_axis_alignment: AxisAlignment`, `cross_axis_alignment: AxisAlignment`, `minimum_cross_axis_size`, `default_flex`.
- **Always write `..Default::default()`.** The `Default` impl (L1444) sets `size = size_of::<_cef_box_layout_settings_t>()`.
- `AxisAlignment::{START, CENTER, END, STRETCH}` (STRETCH at L49690). Use `STRETCH` on the cross axis so children fill the height.
- Header: *"The child views are always sized according to their preferred size ... Using the preferred size as the basis, free space along the main axis is distributed to views in the ratio of their flex weights ... A flex of 0 means this view is not resized."*
- Chromium `BoxLayout::InitializeChildData` skips children that are `!child->GetVisible()` (box_layout.cc ~L300). **Hidden children take no space**, and a visibility change invalidates the host layout automatically.
- `FillLayout`: *"causes the associated Panel's one child to be sized to match the bounds of its parent."* In Chromium it fills every non-hidden child.

**How CEF resolves preferred size** (`view_view.h` `CalculatePreferredSize`): it uses the delegate's `preferred_size` **only if `!IsEmpty()`, meaning width > 0 AND height > 0**. Otherwise it falls back to the views default, then to the view's *current* `size()`. So:
- **Sidebar with a fixed but changeable width (recommended):** implement `fn preferred_size(&self, view: Option<&mut View>) -> Size` (L37157) returning `Size { width: w.get(), height: 1 }` (height ≥ 1 with a horizontal STRETCH box). Give it flex 0 (`default_flex: 0`) and the content panel flex 1. To change the width: `cell.set(new)`, then `sidebar.invalidate_layout()` and `window.layout()`.
- `set_size()` on the sidebar happens to work through the "current size" fallback, but a flex or overflow pass can shrink it and it then stays shrunk. Don't rely on it.
- Hide the sidebar with `sidebar.set_visible(0)` and the content fills the window (the box skips hidden children).

---

## 5. BrowserView

`pub fn browser_view_create(client: Option<&mut Client>, url: Option<&CefString>, settings: Option<&BrowserSettings>, extra_info: Option<&mut DictionaryValue>, request_context: Option<&mut RequestContext>, delegate: Option<&mut BrowserViewDelegate>) -> Option<BrowserView>` (L59126). Argument order matches C++.
- *"The underlying CefBrowser will not be created until this view is added to the views hierarchy."* To be exact, it happens in `AddedToWidget`, so the parent must already be in a Window. A hidden view, or a hidden overlay, still gets its browser.
- `extra_info` goes to the renderer's `on_browser_created`.
- `request_context`: pass a separate context for each profile or Space.
- `settings` is copied. `background_color` must be fully opaque; otherwise a windowed browser uses white or the `CefSettings` colour.

| Rust signature | Line | Notes |
|---|---|---|
| `pub fn browser_view_get_for_browser(browser: Option<&mut Browser>) -> Option<BrowserView>` | L59186 | Maps a Browser from a handler back to its view. |
| `fn browser(&self) -> Option<Browser>` | L39160 (ImplBrowserView) | *"Will return NULL if the browser has not yet been created or has already been destroyed."* |
| `fn set_prefer_accelerators(&self, prefer_accelerators: ::std::os::raw::c_int)` | L39164 | Default false: the page's `keydown` with `preventDefault` wins over *normal*-priority accelerators. |
| `fn runtime_style(&self) -> RuntimeStyle` / `fn chrome_toolbar(&self) -> Option<View>` | L39166 / L39162 | The toolbar exists only in Chrome style. |

`ImplBrowserViewDelegate`:

| Rust signature | Line | Semantics |
|---|---|---|
| `fn browser_runtime_style(&self) -> RuntimeStyle` | L37708 | *"Chrome style is the default unless Alloy is specifically requested"* (`browser_view_impl.cc`). Popups are forced to the opener's style; DevTools is always Chrome style. |
| `fn on_browser_created(&self, browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>)` | L37656 | *"called after CefLifeSpanHandler::OnAfterCreated()"*. Synchronous inside `add_child_view`. |
| `fn on_browser_destroyed(&self, browser_view: Option<&mut BrowserView>, browser: Option<&mut Browser>)` | L37663 | *"called before CefLifeSpanHandler::OnBeforeClose()"*. |
| `fn delegate_for_popup_browser_view(&self, browser_view: Option<&mut BrowserView>, settings: Option<&BrowserSettings>, client: Option<&mut Client>, is_devtools: ::std::os::raw::c_int) -> Option<BrowserViewDelegate>` | L37670 | Rust default is `None`. Return a new TabDelegate. |
| `fn on_popup_browser_view_created(&self, browser_view: Option<&mut BrowserView>, popup_browser_view: Option<&mut BrowserView>, is_devtools: ::std::os::raw::c_int) -> ::std::os::raw::c_int` | L37680 | *"Optionally add |popup_browser_view| to the views hierarchy yourself and return true. Otherwise return false and a default CefWindow will be created."* Use this to adopt `window.open` / `target=_blank` popups as tabs (the opener link survives). Return 0 for DevTools. VERIFIED-FIX: contrary to the header, in 152 Alloy this fires **before** `on_after_created`/`on_browser_created` for the popup. `AlloyBrowserHostImpl::CreateInternal` step 1 is `opener->platform_delegate_->PopupBrowserCreated(...)`, then `OnAfterCreated()`, then `NotifyBrowserCreated()`. CEF drops its keep-alive reference right after this returns, so clone `popup_browser_view` if you adopt it asynchronously (the skeleton does). |
| `fn chrome_toolbar_type(&self, browser_view: Option<&mut BrowserView>) -> ChromeToolbarType` | L37689 | Chrome style only. |
| `fn use_frameless_window_for_picture_in_picture(&self, browser_view: Option<&mut BrowserView>) -> ::std::os::raw::c_int` | L37693 | *"Content in frameless windows should specify draggable regions using '-webkit-app-region: drag'"*. There are also `allow_move_for_picture_in_picture` (L37712) and `allow_picture_in_picture_without_user_activation` (L37719). |
| `fn on_gesture_command(&self, browser_view: Option<&mut BrowserView>, gesture_command: GestureCommand) -> ::std::os::raw::c_int` | L37700 | Touchpad swipe `GestureCommand::{BACK, FORWARD}`. Return 1 to handle or suppress it. VERIFIED-FIX: **macOS only**. `GetGestureCommand()` in `browser_view_impl.cc` is `#if BUILDFLAG(IS_MAC)`, so this never fires on Windows. |

**Focus stealing:** background tabs can grab focus when they navigate. Implement `ImplFocusHandler::on_set_focus(&self, browser: Option<&mut Browser>, source: FocusSource) -> c_int` (L19549) and return 1 (cancel) for tabs that aren't visible. It comes from `ImplClient::focus_handler` (L27867).

**Closing one tab without closing the Window.** This matters because of how Alloy handles `CloseContents` (`alloy_browser_host_impl.cc`):
- If `DoClose` returns false while the browser still has a window, CEF calls `platform_delegate_->CloseHostWindow()`. With Views that is `widget->Close()` (`browser_platform_delegate_views.cc`), which **closes the whole Window**.
- The correct flow:
  1. Call `host.close_browser(0)` (`fn close_browser(&self, force_close: c_int)`, L12544) so `beforeunload` handlers run.
  2. In `fn do_close(&self, browser: Option<&mut Browser>) -> c_int` (L20751), **return 1** and post a task.
  3. In the task, call `remove_child_view` and **drop every reference** to the BrowserView.
  4. `~CefBrowserViewImpl` then runs `browser->WindowDestroyed()`, which force-closes the browser and fires `on_before_close` (L20755). cefclient relies on this: *"We hold the last reference to the BrowserView, and releasing it will trigger overlay Browser destruction."*
- `DoClose` exists only for Alloy style (`cef_browser.h`: *"CefLifeSpanHandler::DoClose (Alloy style only)"*). This is one more reason to use Alloy.
- Issue #3376 ("Support closing CefBrowserView without closing CefWindow") is still open, so **test this flow in 152**.
- VERIFIED-FIX (supporting detail):
  - Step 4 exists since CEF commit "views: Trigger CefBrowser destruction on CefBrowserView release (see #3790)" (Oct 2024, M130). #3376 (2022) and forum t=19152 describe the older behaviour where `OnBeforeClose` never fired, so their "not working" reports predate this fix.
  - Returning 1 from `DoClose` also resets `destruction_state_` to `NONE` (`AlloyBrowserHostImpl::CloseContents`: `else if (destruction_state_ != DESTRUCTION_STATE_NONE) destruction_state_ = DESTRUCTION_STATE_NONE;`). So `~CefBrowserViewImpl` sees `!WillBeDestroyed()` and calls `WindowDestroyed()` → `CloseBrowser(true)`.
  - The header requires finishing the close promptly: *"You are still required to complete the browser close as soon as possible ... otherwise the browser will be left in a partially closed state"* (`cef_life_span_handler.h`).
  - For a **detached** BrowserView, `close_browser` + `DoClose`=0 does nothing: `CloseHostWindow()` finds no Widget. The browser is only destroyed once the last reference is dropped.

---

## 6. Critical questions

### (a) Multiple BrowserViews in one Window, and which styles
- **Allowed.** Header rule, quoted in §0: an Alloy Window hosts only Alloy BrowserViews; a Chrome Window hosts at most one Chrome BrowserView plus any number of Alloy ones. It is enforced in `CefBrowserViewImpl::AddedToWidget`. When the rule is broken, CEF logs an error and **the browser is not created** (no crash).
  - VERIFIED-FIX (nuance): the "multiple Chrome style" check is `cef_widget->IsChromeStyle() && cef_widget->GetThemeProfile()`. `GetThemeProfile()` is non-null once **any** BrowserView (Alloy too) is attached. In a Chrome-style Window, a Chrome-style BrowserView added after an Alloy one is therefore rejected, so add the Chrome-style view first. This doesn't affect the all-Alloy recommendation.
- VERIFIED-FIX (attribution): the quote *"Multiple Alloy-style BrowserViews are supported in overlays with issue #3790"* is a maintainer comment on **#3376**, not on #3790. On #3790 the maintainer wrote *"Yes, this appears to work with Alloy style browsers (tested M130). I'm adding some tests for it now."* #3790 was closed 2024-10-17 (confirmed via GitHub API).
- **Recommendation: Window = ALLOY, sidebar = ALLOY, every tab = ALLOY, command bar = ALLOY.**
  1. Several tabs, or split view, can't be Chrome style (at most one per Window).
  2. A BrowserView inside an overlay must be Alloy (cefclient: *"Overlay browser view must always be Alloy style."*; #3790).
  3. `DoClose` (the tab-close flow above) is Alloy only.
  4. We draw our own browser UI in HTML, so Chrome's toolbar and bubbles aren't needed.
  5. Mixing would only buy a Chrome-style Window with Alloy content, which gives Alloy browsers nothing.
- **Cost of Alloy** (`cef_types_runtime.h`: *"Chrome style provides the full Chrome UI and browser functionality whereas Alloy style provides less default browser functionality but adds additional client callbacks"*):
  - You implement permission prompts (`PermissionHandler`), downloads UI (`DownloadHandler`), find-in-page (`BrowserHost::find`), JS dialogs if you want custom ones, and HTML fullscreen (`DisplayHandler::on_fullscreen_mode_change` plus `window.set_fullscreen`).
  - DevTools still opens as a Chrome-style popup in its own Window. That is allowed.
    - VERIFIED-FIX: that is what the *undock* path does, and it is no longer how sta shows DevTools.
      The frontend document (`devtools://devtools/bundled/devtools_app.html`) loads fine in an
      **Alloy** BrowserView — it is an ordinary WebUI page, and Chromium gives it `DevToolsUIBindings`
      with the no-op `DefaultBindingsDelegate` — so sta docks it inside the tab's card and supplies
      the embedder half itself (`docs/research/devtools.md`, ARCHITECTURE §4.1). Only CEF's *own*
      DevTools window is Chrome style.

### (b) Reparent, or hide?
- **Reparenting works.** `RemoveChildView` calls `content_view()->RemoveChildView(view_ptr); view_util::ResumeOwnership(view);`. `AddChildView` of an unattached view calls `PassOwnership`.
  - The browser is destroyed only when (1) the last client reference is dropped while detached (`~CefBrowserViewImpl`: *"|browser_| may exist here if the BrowserView was removed from the Views hierarchy prior to tear-down and the last BrowserView reference was released ... Force the browser to be destroyed"*), or (2) it is still attached when the hierarchy is torn down (`Detach()` → `WindowDestroyed()`).
  - cefclient's overlay browser pops an Alloy BrowserView out of an overlay into a new top-level Window and back (`controller_->Destroy()`, `CreateTopLevelWindow(new PopoutWindowDelegate(.., browser_view_))`, `popout_window_->RemoveChildView(browser_view_)`, `AddOverlayView(browser_view_, ...)`).
- **Why hiding is still better for tab switching:**
  - Each removal runs `RemovedFromWidget → DisassociateFromWidget → RemoveAssociatedProfile` (*"May call Widget::ThemeChanged()"*), and each re-add runs `AddedToWidget → AddAssociatedProfile`. Each switch detaches and re-attaches the native view and compositor surface.
  - VERIFIED-FIX: it does **not** re-theme the Window on every switch. `CefWidgetImpl::AddAssociatedProfile` just increments a per-Profile count (*"Another instance of a known Profile"*). `NotifyThemeColorsChanged(..., call_theme_changed=true)`, which is async, runs only when `GetThemeProfile()` changes, i.e. `associated_profiles_.begin()`. With the sidebar attached in the same Profile, detaching a same-Profile tab never re-themes. It can happen when moving tabs of a *different* RequestContext/Profile. On re-add, Chromium's `View::AddChildViewAtImpl` still runs `PropagateThemeChanged()` on the **re-added view's subtree** (old theme is null), so that view's `on_theme_changed` fires and its `set_background_color` is reset.
  - Focus is lost, and you risk dropping the last reference by accident.
  - `set_visible(0)` gives the same resource behaviour: `NativeViewHostAura::HideWidget()` → `native_view()->Hide()`. The aura occlusion state becomes HIDDEN, so `WebContentsViewAura::GetVisibility()` reports `Visibility::HIDDEN`: page visibility `hidden` and throttled timers/rAF. Hidden children also take no layout space.
- **Recommendation:** attach every tab of the current Space to the content panel and switch with `set_visible` plus `reorder_child_view`. Use detach and re-add only to move a tab to another Window (tear-off), into an overlay ("peek"), or between Spaces that use different content panels.
  - While a view is detached, **hold a strong `BrowserView`** reference.
  - Release every reference (including detached views) in `on_window_destroyed`.
  - Unload tabs by closing their browsers, not by detaching.

### (c) Overlays
- **Docking modes** (`DockingMode`, L50732): `TOP_LEFT`, `TOP_RIGHT`, `BOTTOM_LEFT`, `BOTTOM_RIGHT`, `CUSTOM`.
  - Corner modes: *"sized to |view|'s preferred size, and positioned based on |docking_mode| ... re-positioned as appropriate when the Window resizes"*. Adjust with `set_insets` and `size_to_preferred_size`.
  - `CUSTOM`: starts at the top-left with its preferred size. *"Optionally change the overlay position and/or size when OnLayoutChanged is called on the Window's delegate."*
  - *"Overlays are hidden by default."* *"It is therefore recommended to call this method last after all other child Views have been added."*
- **`ImplOverlayController` methods (L41180):**

```rust
fn is_valid(&self) -> c_int;                                  // L41182
fn is_same(&self, that: Option<&mut OverlayController>) -> c_int; // L41184
fn contents_view(&self) -> Option<View>;                      // L41186
fn window(&self) -> Option<Window>;                           // L41188
fn docking_mode(&self) -> DockingMode;                        // L41190
fn destroy(&self);                                            // L41192
fn set_bounds(&self, bounds: Option<&Rect>);                  // L41194  CUSTOM only
fn bounds(&self) -> Rect;                                     // L41196  parent (window) coords
fn bounds_in_screen(&self) -> Rect;                           // L41198
fn set_size(&self, size: Option<&Size>);                      // L41200  CUSTOM only
fn size(&self) -> Size;                                       // L41202
fn set_position(&self, position: Option<&Point>);             // L41204  CUSTOM only
fn position(&self) -> Point;                                  // L41206
fn set_insets(&self, insets: Option<&Insets>);                // L41208  non-CUSTOM only
fn insets(&self) -> Insets;                                   // L41210
fn size_to_preferred_size(&self);                             // L41212
fn set_visible(&self, visible: c_int);                        // L41214
fn is_visible(&self) -> c_int;                                // L41216
fn is_drawn(&self) -> c_int;                                  // L41218
```

  The setters silently do nothing in the wrong mode (`overlay_view_host.cc` checks `docking_mode() == CEF_DOCKING_MODE_CUSTOM`). Empty bounds are ignored, and bounds are clipped to the window. Header: *"Methods exposed by this controller should be called in preference to methods of the same name exposed by the contents View."*
- **How overlays work:** each overlay is a child `views::Widget` (`TYPE_CONTROL`, translucent, parented to the window's native view). It is `Activatable::kYes` only when `can_activate` is true.
  - `destroy()` → `Cleanup()` removes the contents view and calls `ResumeOwnership`, so the view (and its browser) can be reused if you still hold it.
  - On window close, `CloseOverlayViews()` runs before the Widget is destroyed, so overlay BrowserViews **are not destroyed automatically** while you hold a reference. VERIFIED-FIX: in 152 this is the inline loop in `CefWindowView::WindowClosing()`. The 152 `Cleanup()` comment adds: *"if CefWindowView::WindowClosing is not called, DeleteDelegate will call this after the host Widget and all associated Widgets/Views have been destroyed"*.
  - VERIFIED-FIX (addition): if an overlay BrowserView's `do_close` returns 0, `CloseHostWindow()` closes the **overlay** Widget (its `root_view()->GetWidget()`), not the top-level Window.
- **BrowserView in an overlay: yes.**
  - It must be Alloy. Pass `can_activate = 1` for keyboard input, then call `browser_view.request_focus()` (cefclient does exactly this). Issue #3790 was about Chrome-style overlay browsers not appearing; Alloy works.
  - Its browser is created when the overlay is added (the command bar is pre-warmed).
  - **Transparency is not possible:** the Widget is translucent, but a windowed browser is opaque (`BrowserSettings.background_color`: *"If the alpha component is fully transparent for a windowed browser then the CefSettings.background_color value will be used"*). Design the command bar as an opaque rectangle; true rounded or blurred glass needs OSR. UPDATE:
    rounded corners and shadows work without OSR by surrounding the BrowserView with image and
    panel pieces inside the translucent overlay widget (ARCHITECTURE §4.4); blur still needs OSR.
  - Mark the overlay rectangle as `draggable: 0` in the Window's regions.
  - Dismissing on blur: `ViewDelegate::on_blur` (L37199) on the overlay BrowserView is a reasonable hook. Its behaviour for child-widget focus changes isn't documented; verify it, and fall back to a JS `blur` or `visibilitychange` event sent over IPC.

### (d) Frameless window on Windows
- `is_frameless = 1` → `CaptionlessFrameView` plus `params.remove_standard_frame = true` (`window_view.cc`).
  - The HWND keeps `WS_OVERLAPPEDWINDOW` minus MIN/MAX box. It keeps `WS_CAPTION | WS_SYSMENU`, and `WS_THICKFRAME` if `can_resize` (`widget_hwnd_utils.cc`).
  - Native move and snap therefore work, and Win+Arrow should too. The Windows 11 Snap Layouts flyout on the maximize button isn't available (HTML buttons, no `HTMAXBUTTON`). Double-click-to-maximize on a drag area is untested.
  - VERIFIED-FIX (uncertainty): `remove_standard_frame` strips `WS_MAXIMIZEBOX` at creation (`widget_hwnd_utils.cc` L82-84). Drag-to-top-edge snap-maximize and caption double-click maximize may depend on that style in Windows, so treat "snap works" as unverified until tested. `HWNDMessageHandler::SizeConstraintsChanged()` can re-add `WS_MAXIMIZEBOX` later when `can_resize && can_maximize`.
- **Resize borders:** `kResizeBorderThickness = 4` DIP inside the window edge and `kResizeAreaCornerSize = 16` DIP, only when not maximized or fullscreen. VERIFIED-FIX: the band exists even when `CanResize()` is false. `FrameView::GetHTComponentForFrame` then returns `HTBORDER` (no resize) instead of `HTLEFT`/`HTTOP`/..., and still never `HTCLIENT`. This hit test runs **before** draggable regions and the client view, so the outer 4 DIP of your sidebar or content can't receive clicks, whether or not the window is resizable. The exception is maximized or fullscreen, where the thickness is 0.
- **Hit-test order** (`CaptionlessFrameView::NonClientHitTest`): fullscreen → `HTCLIENT`; resize frame; point inside the draggable region → `HTCAPTION`; client view; otherwise `HTCAPTION`.
- **Draggable regions are NOT applied automatically for Views-hosted (Alloy) browsers.** The renderer sends them to `CefFrameHostImpl::UpdateDraggableRegions`, which leads to `CefDragHandler::OnDraggableRegionsChanged` and nothing else. The only internal consumer is Chrome-style frameless Document PiP (`ChromeBrowserDelegate::SupportsDraggableRegion() { return SupportsFramelessPictureInPicture(); }`).
  - You must implement `fn on_draggable_regions_changed(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, regions: Option<&[DraggableRegion]>)` (L19155), returned from `ImplClient::drag_handler` (L27859). Forward to `window.set_draggable_regions`.
  - Region coordinates are relative to that browser's view, so convert with `convert_point_to_window`. cefclient's `ViewsWindow::SetDraggableRegions` does exactly this, and cefclient's `RootWindowViews::OnSetDraggableRegions` posts to the UI thread first.
  - Header on when it fires: *"If draggable regions are never defined in a document this method will also never be called. If the last draggable region is removed from a document this method will be called with an empty vector."*
  - Accept regions **only from your own UI browsers**. Give tabs a Client without a DragHandler; otherwise any website could make parts of your window drag areas.
- There are no min/max/close buttons. Maintainer (forum t=16314): *"If you want min/max/close buttons on a frameless window you will need to implement them yourself (for example, in the HTML/JS)."* Wire them to `minimize`, `maximize`/`restore` and `close`.
- The cefclient demo for this setup is `cefclient --use-views --hide-frame --hide-controls`. The maintainer recommends Views for custom title bars (forum t=18605).

---

## 7. Recommended skeleton (compile-checked)

Tree: `Window (Alloy, frameless, horizontal BoxLayout)` → `[ sidebar BrowserView (flex 0, preferred width from a Cell) | content Panel (flex 1, horizontal BoxLayout, default_flex 1, one BrowserView per tab, inactive tabs hidden) ]`, plus `command-bar BrowserView` in a CUSTOM overlay added last.

```rust
//! sta Views shell skeleton. Compile-checked against cef = "=152.3.0" (Windows x64).
//! Everything here runs on the browser-process UI thread.
#![allow(dead_code)]

use cef::*;
use std::{cell::{Cell, RefCell}, rc::Rc};

pub const SIDEBAR_URL: &str = "sta://app/sidebar.html";
pub const COMMAND_BAR_URL: &str = "sta://app/command-bar.html";

// One command_id per accelerator: Window::set_accelerator() with an existing id REPLACES it.
pub const CMD_COMMAND_BAR: i32 = 1001; // Ctrl+T
pub const CMD_COMMAND_BAR_URL: i32 = 1004; // Ctrl+L
pub const CMD_TOGGLE_SIDEBAR: i32 = 1002; // Ctrl+S
pub const CMD_CLOSE_TAB: i32 = 1003; // Ctrl+W
const VK_L: i32 = 0x4C;
const VK_S: i32 = 0x53;
const VK_T: i32 = 0x54;
const VK_W: i32 = 0x57;
const VK_ESCAPE: i32 = 0x1B;

const UI_BG: u32 = 0xFF1E1E2E; // ARGB, must be fully opaque for windowed browsers

// ----------------------------------------------------------------------------- State
pub struct Tab { pub id: u64, pub view: BrowserView }

#[derive(Default)]
pub struct Shell {
    pub window: Option<Window>,
    pub root_layout: Option<BoxLayout>,
    pub sidebar: Option<BrowserView>,
    pub content: Option<Panel>,
    pub content_layout: Option<BoxLayout>,
    pub tabs: Vec<Tab>,
    pub shown: Vec<u64>, // 1 id = single tab, 2+ = split view (left-to-right order)
    pub next_tab_id: u64,
    pub command_bar: Option<BrowserView>,
    pub command_bar_ctl: Option<OverlayController>,
    pub tab_client: Option<Client>,
    /// Separate Cell so ViewDelegate::preferred_size never needs a RefCell borrow
    /// (it is called re-entrantly from inside layout()).
    pub sidebar_width: Rc<Cell<i32>>,
    pub sidebar_visible: bool,
    /// Last regions reported by the sidebar page, in sidebar-view coordinates.
    pub sidebar_regions: Vec<DraggableRegion>,
    pub closing: bool,
    pub live_browsers: usize,
}
pub type ShellRef = Rc<RefCell<Shell>>;

// ----------------------------------------------- post a closure to the UI thread (UI -> UI only; !Send)
wrap_task! {
    struct FnTask { f: Rc<RefCell<Option<Box<dyn FnOnce()>>>> }
    impl Task {
        fn execute(&self) {
            let f = self.f.borrow_mut().take();
            if let Some(f) = f { f(); }
        }
    }
}
pub fn post_ui(f: impl FnOnce() + 'static) {
    let mut task = FnTask::new(Rc::new(RefCell::new(Some(Box::new(f)))));
    post_task(ThreadId::UI, Some(&mut task));
}

// ----------------------------------------------------------------------------- Clients / handlers
wrap_client! {
    pub struct StaClient { shell: ShellRef, is_ui: bool }
    impl Client {
        fn drag_handler(&self) -> Option<DragHandler> {
            // Only our own UI may define window-drag areas; web pages in tabs must not.
            if self.is_ui { Some(UiDragHandler::new(self.shell.clone())) } else { None }
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(ShellLifeSpanHandler::new(self.shell.clone()))
        }
    }
}

wrap_drag_handler! {
    pub struct UiDragHandler { shell: ShellRef }
    impl DragHandler {
        fn on_draggable_regions_changed(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            regions: Option<&[DraggableRegion]>,
        ) {
            let Some(browser) = browser else { return };
            let sidebar = self.shell.borrow().sidebar.clone();
            let from_sidebar = sidebar
                .and_then(|bv| bv.browser())
                .map(|b| b.is_same(Some(&mut *browser)) != 0)
                .unwrap_or(false);
            if !from_sidebar { return; }
            self.shell.borrow_mut().sidebar_regions = regions.map(|r| r.to_vec()).unwrap_or_default();
            apply_draggable_regions(&self.shell);
        }
    }
}

wrap_life_span_handler! {
    pub struct ShellLifeSpanHandler { shell: ShellRef }
    impl LifeSpanHandler {
        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            self.shell.borrow_mut().live_browsers += 1;
        }
        fn do_close(&self, browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            // Returning 0 here would make CEF close the *whole top-level Window*
            // (CefBrowserPlatformDelegateViews::CloseHostWindow). We own teardown instead:
            // detach the BrowserView and drop the last reference (async, not re-entrantly).
            let Some(browser) = browser else { return 0 };
            let id = browser.identifier();
            let shell = self.shell.clone();
            post_ui(move || release_browser_view(&shell, id));
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let (left, closing, window) = {
                let mut s = self.shell.borrow_mut();
                s.live_browsers = s.live_browsers.saturating_sub(1);
                (s.live_browsers, s.closing, s.window.clone())
            };
            if left == 0 && closing {
                match window {
                    Some(w) if w.is_closed() == 0 => w.close(), // CanClose now returns 1
                    _ => quit_message_loop(),
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------- View delegates
wrap_browser_view_delegate! {
    pub struct SidebarDelegate { width: Rc<Cell<i32>> }
    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // height MUST be > 0: CEF ignores a CefSize that IsEmpty() (w<=0 || h<=0).
            Size { width: self.width.get(), height: 1 }
        }
    }
    impl BrowserViewDelegate {
        fn browser_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
    }
}

wrap_browser_view_delegate! {
    pub struct CommandBarDelegate { shell: ShellRef }
    impl ViewDelegate {
        fn on_blur(&self, _view: Option<&mut View>) {
            // Arc-like: clicking elsewhere dismisses the command bar (verify; see §6c).
            let shell = self.shell.clone();
            post_ui(move || set_command_bar_visible(&shell, false));
        }
    }
    impl BrowserViewDelegate {
        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY // overlays hosting a BrowserView must be Alloy
        }
    }
}

wrap_browser_view_delegate! {
    pub struct TabDelegate { shell: ShellRef }
    impl ViewDelegate {}
    impl BrowserViewDelegate {
        fn browser_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }
        fn delegate_for_popup_browser_view(
            &self,
            _browser_view: Option<&mut BrowserView>,
            _settings: Option<&BrowserSettings>,
            _client: Option<&mut Client>,
            is_devtools: ::std::os::raw::c_int,
        ) -> Option<BrowserViewDelegate> {
            // VERIFIED-FIX: DevTools popups are always Chrome style; handing them a delegate whose
            // browser_runtime_style() returns ALLOY logs "GetBrowserRuntimeStyle() requested Alloy
            // style; only Chrome style is supported for DevTools popups" (browser_view_impl.cc).
            if is_devtools != 0 {
                return None;
            }
            // Rust default returns None (C++ default returns `this`).
            Some(TabDelegate::new(self.shell.clone()))
        }
        fn on_popup_browser_view_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            popup_browser_view: Option<&mut BrowserView>,
            is_devtools: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if is_devtools != 0 {
                return 0; // DevTools is always Chrome style -> let CEF create its own Window
            }
            let Some(popup) = popup_browser_view.cloned() else { return 0 };
            let shell = self.shell.clone();
            post_ui(move || {
                if let Some(id) = adopt_tab(&shell, popup) { show_tabs(&shell, &[id]); }
            });
            1 // we took ownership of the popup BrowserView
        }
    }
}

wrap_panel_delegate! {
    pub struct ContentPanelDelegate { shell: ShellRef }
    impl ViewDelegate {
        fn on_layout_changed(&self, _view: Option<&mut View>, new_bounds: Option<&Rect>) {
            // e.g. forward content bounds to the sidebar UI for split-view resize handles.
            let _ = new_bounds;
        }
        fn on_theme_changed(&self, view: Option<&mut View>) {
            // set_background_color() is reset on every theme change (incl. being added to a Window).
            if let Some(view) = view { view.set_background_color(UI_BG); }
        }
    }
    impl PanelDelegate {}
}

wrap_window_delegate! {
    pub struct StaWindowDelegate { shell: ShellRef, ui_client: Client, tab_client: Client }

    impl ViewDelegate {
        fn minimum_size(&self, _view: Option<&mut View>) -> Size { Size { width: 640, height: 400 } }
        fn on_layout_changed(&self, _view: Option<&mut View>, _new_bounds: Option<&Rect>) {
            // Window content was laid out (resize, maximize, sidebar toggle...).
            layout_command_bar(&self.shell);
        }
        fn on_theme_changed(&self, view: Option<&mut View>) {
            if let Some(view) = view { view.set_background_color(UI_BG); }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let Some(window) = window else { return };
            build_window(&self.shell, window, &self.ui_client, &self.tab_client);
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            // Take every CEF handle out while borrowed, drop them *after* the borrow ends:
            // releasing a detached BrowserView synchronously destroys its browser and
            // fires OnBeforeClose, which borrows the shell again.
            let taken = {
                let mut s = self.shell.borrow_mut();
                s.closing = true;
                (s.window.take(), s.root_layout.take(), s.sidebar.take(), s.content.take(),
                 s.content_layout.take(), std::mem::take(&mut s.tabs), s.command_bar.take(),
                 s.command_bar_ctl.take(), s.tab_client.take())
            };
            drop(taken);
            let live = self.shell.borrow().live_browsers;
            if live == 0 { quit_message_loop(); }
        }

        fn can_close(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int {
            let views: Vec<BrowserView> = {
                let mut s = self.shell.borrow_mut();
                s.closing = true; // NB: reset this if the user cancels a beforeunload dialog
                let mut v: Vec<BrowserView> = s.tabs.iter().map(|t| t.view.clone()).collect();
                v.extend(s.sidebar.clone());
                v.extend(s.command_bar.clone());
                v
            };
            let mut pending = false;
            for bv in views {
                if let Some(host) = bv.browser().and_then(|b| b.host()) {
                    host.close_browser(0); // unload handlers -> DoClose -> release_browser_view
                    pending = true;
                }
            }
            // Returning 1 immediately is also OK: attached views are torn down with the
            // Window (Detach -> browser force-closed, unload handlers skipped).
            // VERIFIED-FIX: "unload handlers skipped" is not guaranteed: Detach -> WindowDestroyed ->
            // CloseBrowser(true), which still calls DispatchBeforeUnload() when
            // NeedToFireBeforeUnloadOrUnloadEvents() (alloy_browser_host_impl.cc); the header
            // recommends TryCloseBrowser()/IsReadyToBeClosed() in CanClose instead.
            (!pending) as ::std::os::raw::c_int
        }

        fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
            // Frameless windows need an explicit size (cefclient does the same).
            Rect { x: 0, y: 0, width: 1280, height: 800 }
        }
        fn initial_show_state(&self, _window: Option<&mut Window>) -> ShowState { ShowState::NORMAL }
        fn is_frameless(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int { 1 }
        // !!! The Rust trait defaults for these return 0 (C++ defaults are true).
        fn can_resize(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int { 1 }
        fn can_maximize(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int { 1 }
        fn can_minimize(&self, _window: Option<&mut Window>) -> ::std::os::raw::c_int { 1 }
        fn window_runtime_style(&self) -> RuntimeStyle { RuntimeStyle::ALLOY }

        fn on_window_activation_changed(&self, _window: Option<&mut Window>, active: ::std::os::raw::c_int) {
            let _ = active; // push to sidebar UI
        }
        fn on_window_bounds_changed(&self, window: Option<&mut Window>, _new_bounds: Option<&Rect>) {
            let maximized = window.map(|w| w.is_maximized() != 0).unwrap_or(false);
            let _ = maximized; // push to UI (swap maximize/restore glyph)
        }

        fn on_accelerator(&self, _window: Option<&mut Window>, command_id: ::std::os::raw::c_int) -> ::std::os::raw::c_int {
            match command_id {
                CMD_COMMAND_BAR | CMD_COMMAND_BAR_URL => {
                    let visible = self.shell.borrow().command_bar_ctl.as_ref()
                        .map(|c| c.is_visible() != 0).unwrap_or(false);
                    set_command_bar_visible(&self.shell, !visible);
                    1
                }
                CMD_TOGGLE_SIDEBAR => {
                    let visible = self.shell.borrow().sidebar_visible;
                    set_sidebar_visible(&self.shell, !visible);
                    1
                }
                CMD_CLOSE_TAB => {
                    let first = self.shell.borrow().shown.first().copied();
                    if let Some(id) = first { close_tab(&self.shell, id); }
                    1
                }
                _ => 0,
            }
        }

        fn on_key_event(&self, _window: Option<&mut Window>, event: Option<&KeyEvent>) -> ::std::os::raw::c_int {
            // Called only after the focused view/web content did not handle the key.
            let Some(event) = event else { return 0 };
            if event.type_ == KeyEventType::RAWKEYDOWN && event.windows_key_code == VK_ESCAPE {
                let visible = self.shell.borrow().command_bar_ctl.as_ref()
                    .map(|c| c.is_visible() != 0).unwrap_or(false);
                if visible { set_command_bar_visible(&self.shell, false); return 1; }
            }
            0
        }
    }
}

// ----------------------------------------------------------------------------- Construction
pub fn create_main_window() -> Option<Window> {
    let shell: ShellRef = Rc::new(RefCell::new(Shell { sidebar_visible: true, ..Default::default() }));
    shell.borrow().sidebar_width.set(260);
    let ui_client = StaClient::new(shell.clone(), true);
    let tab_client = StaClient::new(shell.clone(), false);
    // ::new(args...) takes the struct fields in declaration order.
    let mut delegate = StaWindowDelegate::new(shell, ui_client, tab_client);
    window_create_top_level(Some(&mut delegate)) // on_window_created runs INSIDE this call
}

fn ui_browser_settings() -> BrowserSettings {
    BrowserSettings { background_color: UI_BG, ..Default::default() }
}

fn build_window(shell: &ShellRef, window: &mut Window, ui_client: &Client, tab_client: &Client) {
    let mut ui_client = ui_client.clone();

    // Root: [ sidebar | content ] horizontally, both stretched vertically.
    let root_layout = window.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: 1,
        cross_axis_alignment: AxisAlignment::STRETCH,
        default_flex: 0,
        ..Default::default()
    }));
    window.set_background_color(UI_BG); // re-applied in on_theme_changed

    // Sidebar (our HTML UI).
    let width = shell.borrow().sidebar_width.clone();
    let mut sidebar_delegate = SidebarDelegate::new(width);
    let sidebar = browser_view_create(
        Some(&mut ui_client), Some(&CefString::from(SIDEBAR_URL)), Some(&ui_browser_settings()),
        None, None, Some(&mut sidebar_delegate),
    ).expect("sidebar BrowserView");
    window.add_child_view(Some(&mut View::from(&sidebar))); // browser is created here

    // Content panel holding one BrowserView per tab.
    let mut content_delegate = ContentPanelDelegate::new(shell.clone());
    let content = panel_create(Some(&mut content_delegate)).expect("content panel");
    let content_layout = content.set_to_box_layout(Some(&BoxLayoutSettings {
        horizontal: 1,
        between_child_spacing: 8, // split-view gutter
        inside_border_insets: Insets { top: 8, left: 0, bottom: 8, right: 8 }, // Arc-style inset card
        cross_axis_alignment: AxisAlignment::STRETCH,
        default_flex: 1, // every visible tab shares the width equally
        ..Default::default()
    }));
    window.add_child_view(Some(&mut View::from(&content))); // colors: see on_theme_changed
    if let Some(root_layout) = &root_layout {
        root_layout.set_flex_for_view(Some(&mut View::from(&content)), 1); // content takes the rest
    }

    // Command bar overlay: add LAST so it is top-most. Hidden by default; the browser is
    // created now (pre-warmed) because adding it attaches it to a Widget.
    let mut cmd_delegate = CommandBarDelegate::new(shell.clone());
    let command_bar = browser_view_create(
        Some(&mut ui_client), Some(&CefString::from(COMMAND_BAR_URL)), Some(&ui_browser_settings()),
        None, None, Some(&mut cmd_delegate),
    ).expect("command bar BrowserView");
    let command_bar_ctl =
        window.add_overlay_view(Some(&mut View::from(&command_bar)), DockingMode::CUSTOM, 1);

    // Accelerators. high_priority=1 -> handled before web content sees the key.
    window.set_accelerator(CMD_COMMAND_BAR, VK_T, 0, 1, 0, 1);
    window.set_accelerator(CMD_COMMAND_BAR_URL, VK_L, 0, 1, 0, 1);
    window.set_accelerator(CMD_TOGGLE_SIDEBAR, VK_S, 0, 1, 0, 1);
    window.set_accelerator(CMD_CLOSE_TAB, VK_W, 0, 1, 0, 1);

    {
        let mut s = shell.borrow_mut();
        s.window = Some(window.clone());
        s.root_layout = root_layout;
        s.sidebar = Some(sidebar);
        s.content = Some(content);
        s.content_layout = content_layout;
        s.command_bar = Some(command_bar);
        s.command_bar_ctl = command_bar_ctl;
        s.tab_client = Some(tab_client.clone());
    } // borrow released before any further CEF call that may re-enter

    window.set_title(Some(&CefString::from("sta")));
    window.show();

    if let Some(id) = open_tab(shell, "https://example.com/") { show_tabs(shell, &[id]); }
}

// ----------------------------------------------------------------------------- Operations (UI thread, e.g. from the IPC bridge)
pub fn open_tab(shell: &ShellRef, url: &str) -> Option<u64> {
    let mut client = shell.borrow().tab_client.clone()?;
    let mut delegate = TabDelegate::new(shell.clone());
    let view = browser_view_create(
        Some(&mut client), Some(&CefString::from(url)), Some(&BrowserSettings::default()),
        None, None, Some(&mut delegate),
    )?;
    adopt_tab(shell, view)
}

/// Adds an existing BrowserView (new or popup) to the content panel, hidden.
pub fn adopt_tab(shell: &ShellRef, view: BrowserView) -> Option<u64> {
    let content = shell.borrow().content.clone()?;
    view.set_visible(0); // hidden views are skipped by BoxLayout; browser still created
    content.add_child_view(Some(&mut View::from(&view)));
    let mut s = shell.borrow_mut();
    s.next_tab_id += 1;
    let id = s.next_tab_id;
    s.tabs.push(Tab { id, view });
    Some(id)
}

/// Show exactly `ids` (1 = single tab, 2+ = split view in that order); hide the rest.
pub fn show_tabs(shell: &ShellRef, ids: &[u64]) {
    let (content, tabs) = {
        let s = shell.borrow();
        let tabs: Vec<(u64, BrowserView)> = s.tabs.iter().map(|t| (t.id, t.view.clone())).collect();
        (s.content.clone(), tabs)
    };
    let Some(content) = content else { return };
    for (id, view) in &tabs {
        let show = ids.contains(id);
        if (view.is_visible() != 0) != show { view.set_visible(show as i32); }
    }
    for (index, id) in ids.iter().enumerate() {
        if let Some((_, view)) = tabs.iter().find(|(t, _)| t == id) {
            content.reorder_child_view(Some(&mut View::from(view)), index as i32);
        }
    }
    content.invalidate_layout();
    content.layout();
    if let Some((_, first)) = ids.first().and_then(|id| tabs.iter().find(|(t, _)| t == id)) {
        first.request_focus(); // async inside CEF
    }
    shell.borrow_mut().shown = ids.to_vec();
}

/// Starts a graceful close (beforeunload). Teardown continues in DoClose.
pub fn close_tab(shell: &ShellRef, id: u64) {
    let view = shell.borrow().tabs.iter().find(|t| t.id == id).map(|t| t.view.clone());
    if let Some(host) = view.and_then(|v| v.browser()).and_then(|b| b.host()) {
        host.close_browser(0);
    }
}

/// Detach the BrowserView owning `browser_id` and drop our last reference.
/// CefBrowserViewImpl's destructor then force-destroys the browser -> OnBeforeClose.
fn release_browser_view(shell: &ShellRef, browser_id: i32) {
    let matches = |bv: &BrowserView| bv.browser().map(|b| b.identifier() == browser_id).unwrap_or(false);
    let (tabs, sidebar, command_bar) = {
        let s = shell.borrow();
        let tabs: Vec<(u64, BrowserView)> = s.tabs.iter().map(|t| (t.id, t.view.clone())).collect();
        (tabs, s.sidebar.clone(), s.command_bar.clone())
    };
    let mut released: Option<BrowserView> = None;
    if let Some((id, view)) = tabs.into_iter().find(|(_, v)| matches(v)) {
        // NB: never write `if let Some(c) = shell.borrow().content.clone() { c.call() }` -
        // the Ref temporary lives for the whole if-let body and any delegate callback
        // that does borrow_mut() panics (= abort across FFI).
        let content = shell.borrow().content.clone();
        if let Some(content) = content { content.remove_child_view(Some(&mut View::from(&view))); }
        let mut s = shell.borrow_mut();
        s.tabs.retain(|t| t.id != id);
        s.shown.retain(|t| *t != id);
        released = Some(view);
    } else if sidebar.as_ref().map(matches).unwrap_or(false) {
        let window = shell.borrow().window.clone();
        if let (Some(window), Some(view)) = (window, sidebar.clone()) {
            window.remove_child_view(Some(&mut View::from(&view)));
        }
        released = shell.borrow_mut().sidebar.take();
    } else if command_bar.as_ref().map(matches).unwrap_or(false) {
        let ctl = shell.borrow_mut().command_bar_ctl.take();
        if let Some(ctl) = ctl { ctl.destroy(); } // removes contents view; BrowserView survives while referenced
        released = shell.borrow_mut().command_bar.take();
    }
    drop(released); // not inside any RefCell borrow
}

pub fn set_sidebar_visible(shell: &ShellRef, visible: bool) {
    let (window, sidebar) = {
        let mut s = shell.borrow_mut();
        s.sidebar_visible = visible;
        (s.window.clone(), s.sidebar.clone())
    };
    let (Some(window), Some(sidebar)) = (window, sidebar) else { return };
    sidebar.set_visible(visible as i32); // BoxLayout skips hidden children -> content fills
    window.invalidate_layout();
    window.layout();
    apply_draggable_regions(shell);
}

pub fn set_sidebar_width(shell: &ShellRef, width: i32) {
    let (window, sidebar) = {
        let s = shell.borrow();
        s.sidebar_width.set(width.clamp(180, 480));
        (s.window.clone(), s.sidebar.clone())
    };
    if let (Some(window), Some(sidebar)) = (window, sidebar) {
        sidebar.invalidate_layout(); // re-query SidebarDelegate::preferred_size
        window.layout();
        apply_draggable_regions(shell);
    }
}

pub fn set_command_bar_visible(shell: &ShellRef, visible: bool) {
    let (ctl, bar, first_tab) = {
        let s = shell.borrow();
        let first = s.shown.first()
            .and_then(|id| s.tabs.iter().find(|t| t.id == *id))
            .map(|t| t.view.clone());
        (s.command_bar_ctl.clone(), s.command_bar.clone(), first)
    };
    let Some(ctl) = ctl else { return };
    if visible {
        layout_command_bar(shell);
        ctl.set_visible(1);
        if let Some(bar) = bar { bar.request_focus(); } // requires can_activate=1 on add_overlay_view
    } else if ctl.is_visible() != 0 {
        ctl.set_visible(0);
        if let Some(tab) = first_tab { tab.request_focus(); }
    }
    apply_draggable_regions(shell); // overlay punches a no-drag hole
}

pub fn layout_command_bar(shell: &ShellRef) {
    let (window, ctl) = {
        let s = shell.borrow();
        (s.window.clone(), s.command_bar_ctl.clone())
    };
    let (Some(window), Some(ctl)) = (window, ctl) else { return };
    // Overlay bounds are in window (client) coordinates; use only the size here.
    let client = window.client_area_bounds_in_screen();
    let width = (client.width - 64).min(720);
    let height = (client.height - 96).min(440);
    if width <= 0 || height <= 0 { ctl.set_visible(0); return; } // empty bounds are ignored by CEF
    let x = (client.width - width) / 2;
    let y = (client.height / 6).max(48);
    ctl.set_bounds(Some(&Rect { x, y, width, height }));
}

/// Converts sidebar-view regions to window coordinates and applies them.
pub fn apply_draggable_regions(shell: &ShellRef) {
    let (window, sidebar, regions, visible, ctl) = {
        let s = shell.borrow();
        (s.window.clone(), s.sidebar.clone(), s.sidebar_regions.clone(), s.sidebar_visible, s.command_bar_ctl.clone())
    };
    let Some(window) = window else { return };
    let mut out: Vec<DraggableRegion> = Vec::new();
    if let (true, Some(sidebar)) = (visible, sidebar) {
        for r in regions {
            let mut origin = Point { x: r.bounds.x, y: r.bounds.y };
            if sidebar.convert_point_to_window(Some(&mut origin)) == 0 { continue; }
            out.push(DraggableRegion {
                bounds: Rect { x: origin.x, y: origin.y, width: r.bounds.width, height: r.bounds.height },
                draggable: r.draggable,
            });
        }
    }
    if let Some(ctl) = ctl {
        if ctl.is_visible() != 0 {
            // Regions are applied beneath overlays; exclude the overlay area explicitly.
            out.push(DraggableRegion { bounds: ctl.bounds(), draggable: 0 });
        }
    }
    window.set_draggable_regions(Some(&out)); // empty slice clears
}

// Window chrome buttons for a frameless window (call from IPC).
fn window_of(shell: &ShellRef) -> Option<Window> {
    shell.borrow().window.clone() // Ref dropped before the caller touches CEF
}
pub fn window_minimize(shell: &ShellRef) { if let Some(w) = window_of(shell) { w.minimize(); } }
pub fn window_toggle_maximize(shell: &ShellRef) {
    if let Some(w) = window_of(shell) {
        if w.is_maximized() != 0 { w.restore(); } else { w.maximize(); }
    }
}
pub fn window_close(shell: &ShellRef) {
    if let Some(w) = window_of(shell) { w.close(); } // -> can_close (borrow_mut) synchronously
}

pub fn set_icon(window: &Window, png_1x: &[u8], png_2x: &[u8]) {
    if let Some(mut image) = image_create() {
        image.add_png(1.0, Some(png_1x));
        image.add_png(2.0, Some(png_2x));
        window.set_window_icon(Some(&mut image));
        window.set_window_app_icon(Some(&mut image));
    }
}

#[cfg(windows)]
pub fn hwnd_of(window: &Window) -> *mut std::ffi::c_void {
    let hwnd: cef::sys::HWND = window.window_handle();
    hwnd.0.cast()
}
```

Notes on the skeleton:
- **Close flow.** `can_close` requests every browser close. Each `do_close` returns 1 and posts `release_browser_view`, which detaches the view and drops it, so `on_before_close` fires. When the last one closes, `on_before_close` calls `window.close()` again; `can_close` then finds no views and returns 1. Finally `on_window_destroyed` quits the message loop.
  - Caveat: if the user cancels a `beforeunload` dialog, `closing` stays true. Reset it from `JsDialogHandler`'s `on_before_unload_dialog` callback result, or with a timer.
    - VERIFIED-FIX (names): the Rust type is `JsdialogHandler`, macro `wrap_jsdialog_handler!`, returned from `fn jsdialog_handler(&self) -> Option<JsdialogHandler>` (L27879). The method is `fn on_before_unload_dialog(&self, browser: Option<&mut Browser>, message_text: Option<&CefString>, is_reload: ::std::os::raw::c_int, callback: Option<&mut JsdialogCallback>) -> ::std::os::raw::c_int` (L20111).
    - You only learn the user's choice if you show a custom dialog and call `callback` yourself; the default dialog doesn't report it.
    - `fn is_ready_to_be_closed(&self) -> ::std::os::raw::c_int` (L12548, `ImplBrowserHost`) is the header-recommended way for `can_close` to tell cancelable from mandatory closes.
  - VERIFIED-FIX: the `delegate_for_popup_browser_view` in the skeleton now returns `None` for DevTools (see comment there).
- **Spaces or profiles:** use one content Panel per Space and swap it with `set_visible` on the panels; the BoxLayout skips hidden panels too. Give each Space its own `RequestContext` when you create its tabs.
- **Split-view divider:** `between_child_spacing` is the gutter. For draggable split ratios, replace `default_flex` with `set_flex_for_view(view, weight)` per tab (for example 3:2), then call `content.layout()`.
- Handlers are created fresh on every `drag_handler()` / `life_span_handler()` call. That works, but you can cache them as fields of type `DragHandler` / `LifeSpanHandler` instead (they are `Clone`).

---

## 8. Sources
- Local headers: `include/views/cef_window.h`, `cef_window_delegate.h`, `cef_view.h`, `cef_view_delegate.h`, `cef_panel.h`, `cef_box_layout.h`, `cef_fill_layout.h`, `cef_overlay_controller.h`, `cef_browser_view.h`, `cef_browser_view_delegate.h`; `include/internal/cef_types_runtime.h`, `cef_types.h`, `cef_types_wrappers.h`; `include/cef_life_span_handler.h`, `cef_browser.h`, `cef_drag_handler.h`, `cef_focus_handler.h`.
- CEF implementation (master): [browser_view_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/browser_view_impl.cc), [window_view.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/window_view.cc), [window_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/window_impl.cc), [overlay_view_host.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/overlay_view_host.cc), [panel_impl.h](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/panel_impl.h), [view_view.h](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/view_view.h), [view_util.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/view_util.cc), [browser_platform_delegate_views.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/views/browser_platform_delegate_views.cc), [alloy_browser_host_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/alloy/alloy_browser_host_impl.cc), [frame_host_impl.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/frame_host_impl.cc), [chrome_browser_delegate.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/chrome/chrome_browser_delegate.cc), [chrome_browser_view.cc](https://github.com/chromiumembedded/cef/blob/master/libcef/browser/chrome/views/chrome_browser_view.cc).
- cefclient: [views_overlay_browser.cc](https://github.com/chromiumembedded/cef/blob/master/tests/cefclient/browser/views_overlay_browser.cc), [views_window.cc](https://github.com/chromiumembedded/cef/blob/master/tests/cefclient/browser/views_window.cc), [root_window_views.cc](https://github.com/chromiumembedded/cef/blob/master/tests/cefclient/browser/root_window_views.cc).
- Chromium: [box_layout.cc](https://github.com/chromium/chromium/blob/main/ui/views/layout/box_layout.cc), [fill_layout.cc](https://github.com/chromium/chromium/blob/main/ui/views/layout/fill_layout.cc), [native_view_host_aura.cc](https://github.com/chromium/chromium/blob/main/ui/views/controls/native/native_view_host_aura.cc), [web_contents_view_aura.cc](https://github.com/chromium/chromium/blob/main/content/browser/web_contents/web_contents_view_aura.cc), [widget_hwnd_utils.cc](https://github.com/chromium/chromium/blob/main/ui/views/widget/widget_hwnd_utils.cc).
- Issues and forum: [cef#3790](https://github.com/chromiumembedded/cef/issues/3790) (overlay BrowserView; Alloy works; mixed-style tests), [cef#3784](https://github.com/chromiumembedded/cef/issues/3784), [cef#3376](https://github.com/chromiumembedded/cef/issues/3376) (closing a BrowserView without the Window; still open), [cef#3382](https://github.com/chromiumembedded/cef/issues/3382) (frameless minimum size 46x39 on Windows), [forum t=16314](https://magpcss.org/ceforum/viewtopic.php?f=6&t=16314) (frameless buttons), [forum t=18605](https://magpcss.org/ceforum/viewtopic.php?f=6&t=18605) (custom title bar, use Views), [forum t=19152](https://magpcss.org/ceforum/viewtopic.php?f=6&t=19152) (closing one BrowserView), [cef-rs#297](https://github.com/tauri-apps/cef-rs/issues/297) / [#469](https://github.com/tauri-apps/cef-rs/issues/469) (wrap-macro DX; struct-only form not yet released).

---

## Verification log

The adversarial check was done on 2026-09-16. Evidence is in `scratchpad/verify152/` (CEF sources at the exact 152 commit `708dc140…` and Chromium `152.0.7977.83` files, fetched raw from GitHub) and `scratchpad/research/verifyprobe/` (compile probe).

### What was checked

**Rust API (bindings `x86_64_pc_windows_msvc.rs`, cef 152.3.0)**
- All ~140 bindings `L<n>` references were checked line by line: the name, parameter types, return type and enclosing trait all match.
  - Traits: `ImplWindow`, `ImplPanel`, `ImplView`, `ImplBoxLayout`, `ImplBrowserView`, `ImplOverlayController`, `ImplWindowDelegate`, `ImplViewDelegate`, `ImplBrowserViewDelegate`, `ImplLifeSpanHandler`, `ImplDragHandler`, `ImplFocusHandler`, `ImplDisplayHandler`, `ImplClient`, `ImplBrowserHost`, `ImplImage`.
  - Free functions: `window_create_top_level`, `panel_create`, `browser_view_create`, `browser_view_get_for_browser`, `image_create`, `post_task`, `quit_message_loop`.
  - `From` impls.
- Enum constants: `RuntimeStyle::{DEFAULT,CHROME,ALLOY}`, `ShowState::*`, `DockingMode::*`, `AxisAlignment::STRETCH`, `KeyEventType::RAWKEYDOWN`, `GestureCommand::{BACK,FORWARD}`, `ThreadId::UI`, `ChromeToolbarType` default `UNKNOWN`. All `Default` impls match the table in §1.2.
- Struct fields: `DraggableRegion`, `Rect`, `Point`, `Insets`, `KeyEvent`, `BoxLayoutSettings` (its `Default` sets `size`). Also `BrowserSettings.background_color: u32`, `cef::sys::HWND`, and `cef_window_handle_t = HWND`.
- Macros: `wrap_view_delegate!`, `wrap_panel_delegate!`, `wrap_browser_view_delegate!`, `wrap_window_delegate!`, `wrap_client!`, `wrap_drag_handler!`, `wrap_life_span_handler!`, `wrap_task!`.
  - The generated `new(fields…)`, `Clone` (add_ref, no Drop) and `init_methods` install every fn pointer.
  - `unsafe impl Send/Sync for RefGuard` is at `rc.rs` L283-284.
- Compiler re-tests:
  - The §7 skeleton (as printed, after edits) passes `cargo check`.
  - Unit-struct multi-base form fails.
  - A missing `impl PanelDelegate {}` fails.
  - Swapped block order fails.
  - A `Mutex` field fails with E0599.
  - Single-base unit struct compiles.

**Behaviour (CEF 152 / Chromium 152 sources)**
- Style mixing rules and `AddedToWidget` errors; Chrome default for Window and BrowserView; popup and DevTools style forcing.
- C++ delegate defaults (`CanResize`/`CanMaximize`/`CanMinimize`/`CanClose` = true, `WithStandardWindowButtons` = `!IsFrameless`, `GetDelegateForPopupBrowserView` = `this`, `CEF_CTT_NONE`); UNKNOWN toolbar is treated as none.
- Window creation order (`GetInitialBounds`, `IsFrameless`, `CanResize` inside `CreateWidget`, then `OnWindowCreated`); `SetAccelerator` returns early without a widget and replaces by `command_id`.
- `HandleKeyboardEvent` order; `RequestFocus` is async.
- Panel `AddChildView`/`RemoveChildView` ownership (`PassOwnership`/`ResumeOwnership`); `Layout()` is immediate.
- `CalculatePreferredSize` ignores an empty delegate size; BoxLayout and FillLayout skip hidden children; visibility changes invalidate layout; `NativeViewHostAura::HideWidget`; `WebContentsViewAura::GetVisibility`.
- `~CefBrowserViewImpl` and `Detach` force `WindowDestroyed`.
- Alloy `CloseBrowser`/`CloseContents`/`DoClose`/`CloseHostWindow` flow; the `DoClose` header text.
- Overlay host: TYPE_CONTROL translucent child widget, `Activatable`, CUSTOM-only setters, empty bounds ignored, clipping, initially hidden, `Cleanup`/`ResumeOwnership`.
- `CaptionlessFrameView` hit test, 4/16 DIP constants, `remove_standard_frame`, HWND styles.
- `SetDraggableRegions` SkRegion ops.
- Draggable regions only reach `CefDragHandler` (Alloy `CefBrowserContentsDelegate` → `CefFrameHostImpl` → `CefBrowserInfo`; Chrome delegate only for frameless PiP).
- cefclient `ViewsWindow::SetDraggableRegions` conversion and overlay no-drag exclusion; `GetInitialBounds` for frameless; "Overlay browser view must always be Alloy style."
- `background_color` header text.
- Issue states via GitHub API: #3790 closed 2024-10-17, #3376 open, #3382 open, cef-rs #469/#297 open.
- Forum quotes t=16314, t=18605 and t=19152 match.

### What was wrong (fixed inline, marked VERIFIED-FIX)
1. **Re-theming on reparent:** "detaching/re-adding re-themes the whole window on every switch" is false. Profile association is ref-counted, and only the re-added view's subtree gets `on_theme_changed`.
2. **Callback order inside `add_child_view`** was reversed. The real order is `on_theme_changed` → `on_parent_view_changed` → `on_window_changed` → browser creation → `on_after_created` → `on_browser_created`, plus a possible async theme pass.
3. **`on_popup_browser_view_created` timing:** it fires *before* `on_after_created`/`on_browser_created` for Alloy popups in 152, which contradicts the header.
4. **Frameless resize band:** the 4-DIP band eats clicks even when `can_resize` = 0 (it returns `HTBORDER`), not "only when CanResize()".
5. **`CloseOverlayViews()` doesn't exist in 152:** the logic is inline in `WindowClosing()`.
6. **Overlay `do_close`:** a 0 return from an overlay BrowserView closes only the overlay Widget, not the top-level Window.
7. **`on_gesture_command`** is macOS-only (never fires on Windows).
8. **`titlebar_height`** is not macOS-only; it affects dialog and find-bar placement on all platforms.
9. **Maintainer quote misattributed:** it is on #3376, not #3790.
10. **Rust type name:** it is `JsdialogHandler`, not `JsDialogHandler`; the signature has been added.
11. **Skeleton bug:** `delegate_for_popup_browser_view` returned an ALLOY delegate for DevTools popups, which logs an error. It now returns `None` when `is_devtools != 0`.
12. **Skeleton comment:** "unload handlers skipped" on window teardown is not guaranteed.
13. **Macro error message** for a swapped block order corrected. `c_int` shorthand noted as `::std::os::raw::c_int`.
14. **Close-flow details added:**
    - `DoClose` = 1 resets `destruction_state_` to NONE.
    - The destroy-on-release behaviour landed in commit "views: Trigger CefBrowser destruction on CefBrowserView release (see #3790)" (M130), newer than #3376 and forum t=19152.
    - `close_browser` on a detached view is a no-op until release.
15. **Chrome-style Window nuance:** a Chrome-style BrowserView is rejected if any BrowserView is already attached (`GetThemeProfile()` check).

### Still uncertain (not runtime-tested)
- The whole close flow in a live 152 build, including whether `CloseBrowser(true)` after a `DoClose` = 1 re-dispatches beforeunload.
- Whether `on_blur` fires reliably for overlay-to-main-window focus moves.
- Aero Snap drag-to-top maximize and caption double-click without `WS_MAXIMIZEBOX` on a frameless window.
- Exact throttling behaviour of hidden (not occluded) tabs.
- Whether blink draggable-region coordinates stay DIP under page zoom.
- `quit_message_loop()` can be called twice in the "return 1 from can_close" path; this is believed harmless but not verified.
