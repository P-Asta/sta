//! What the docked DevTools frontend may send over its protocol session [owner: tabs]
//! (ext design FINAL PLAN §3 "Session S and policy"; SEC-1).
//!
//! The frontend is a `devtools://` document we load ourselves, but it is still the only part of the
//! docked DevTools that runs untrusted-*shaped* code: a compromised renderer (or a DevTools
//! extension, phase 3) would otherwise hold a browser-level DevTools session. So every message it
//! sends is checked here before it reaches Chromium, on session **S** and on every nested session:
//!
//! - **Always refused**, whatever the fixture says: `Browser.*`, `SystemInfo.*`, every `Target.*`
//!   but the four below, `Page.setDownloadBehavior`, `DOM.setFileInputFiles`,
//!   `Network.getAllCookies`, `Storage.*Cookies`, and `Page.navigate` outside `http(s)`, `file` and
//!   `about`.
//! - **Judged by their parameters**: a method that names the data it reads or destroys by **origin,
//!   storage key or cookie URL** may only name one the inspected page itself has
//!   ([`needs_origins`], [`ORIGIN_SCOPED`]). Without this, `Network.getCookies{urls}` read any
//!   other host's cookies — `HttpOnly` included — and `Storage.clearDataForOrigin` emptied any
//!   other origin's jar, which made the refusals of `Network.getAllCookies` and
//!   `Storage.getCookies` next door cosmetic.
//! - **`Target.*`**: only `setAutoAttach`, `autoAttachRelated`, `getTargetInfo` (on S itself) and
//!   `detachFromTarget` (a session under S).
//! - Everything else must be in [`FRONTEND_METHODS`], the S14 inventory: the methods DevTools sends
//!   with every panel opened, measured against the real frontend (gates-p2.md). An unknown method
//!   gets a protocol error and a WARN with its name, and `debug.info`'s `devtools.refused` lists it,
//!   so a panel nobody exercised is a one-line fixture change and never a silent hole.
//!
//! A nested session is admitted only for a target type the frontend legitimately debugs, and never
//! for one of sta's own documents (SEC-1): see [`nested_target_allowed`].
//!
//! Pure functions only — no CEF, no state — so the whole policy is unit-tested. The one thing the
//! caller must supply is *whose* page this is: the origins of the inspected browser's frames, read
//! only for the handful of methods [`needs_origins`] names.

/// Where a message the frontend sent is addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// Session S: the inspected page itself.
    Own,
    /// A session S created under itself (an iframe, worker or worklet target).
    Nested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Refused by the always-refused list or by a `Target.*`/`Page.navigate` rule.
    Refuse(&'static str),
    /// Not in the S14 inventory.
    Unknown,
}

/// Target types a nested session may be created for (`Target.attachedToTarget`).
const NESTED_TYPES: &[&str] = &["iframe", "worker", "shared_worker", "service_worker", "worklet"];

/// Schemes a nested session is never admitted for: sta's own UI and Chromium's internals.
const NESTED_FORBIDDEN_SCHEMES: &[&str] = &["sta", "devtools", "chrome", "chrome-untrusted", "chrome-search"];

/// `Target.*` methods the frontend may send, and on which session.
fn target_method_allowed(method: &str, session: Session) -> Verdict {
    match method {
        // Flat auto-attach is how the frontend learns about iframes, workers and worklets.
        "Target.setAutoAttach" | "Target.autoAttachRelated" => Verdict::Allow,
        "Target.getTargetInfo" if session == Session::Own => Verdict::Allow,
        "Target.getTargetInfo" => Verdict::Refuse("Target.getTargetInfo is only allowed on the inspected page"),
        // Detaching is how the frontend drops a nested session again.
        "Target.detachFromTarget" => Verdict::Allow,
        _ => Verdict::Refuse("this Target method is not available in sta"),
    }
}

/// Schemes `Page.navigate` may send the inspected page to.
///
/// `file` is deliberately among them (FINAL PLAN §3): a developer inspects local pages. It is also
/// the one path by which a compromised frontend renderer could read the disk — recorded as a
/// residual risk in `docs/research/devtools.md`, next to the fact that the frontend's own
/// `openInNewTab` refuses the same scheme (it goes through the web-content rules instead).
fn navigable(url: &str) -> bool {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https" | "file" | "about"))
}

/// One message from the frontend, as the policy judges it.
pub struct Request<'a> {
    pub method: &'a str,
    pub session: Session,
    /// The message's `params`, when it has any (`Page.navigate`'s URL, a cookie URL, an origin…).
    pub params: Option<&'a serde_json::Value>,
    /// The origins of the inspected browser's frames, lowercased `scheme://host[:port]`. Only read
    /// for the methods [`needs_origins`] names, and empty for every other message.
    pub origins: &'a [String],
}

impl<'a> Request<'a> {
    fn param(&self, key: &str) -> Option<&'a str> {
        self.params.and_then(|p| p.get(key)).and_then(serde_json::Value::as_str)
    }

    /// Whether `origin` (or a storage key beginning with one) is one of the inspected page's.
    fn is_own_origin(&self, value: &str) -> bool {
        // A storage key is `<origin>` plus optional `^`-separated partition components.
        let head = value.split('^').next().unwrap_or(value);
        origin_of(head).is_some_and(|o| self.origins.contains(&o))
    }

    /// Whether `host` may be named as a cookie domain: a frame's own host, or a parent domain of it
    /// (a cookie of `bar.example.com` can carry `domain=.example.com`).
    fn is_own_cookie_domain(&self, domain: &str) -> bool {
        let domain = domain.trim_start_matches('.').to_ascii_lowercase();
        if domain.is_empty() {
            return false;
        }
        self.origins.iter().any(|own| {
            let host = own.rsplit_once('/').map(|(_, h)| h).unwrap_or(own);
            let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
            host == domain || host.ends_with(&format!(".{domain}"))
        })
    }
}

/// The origin of a URL as `scheme://host[:port]`, lowercased and without a default port. `None` for
/// anything without one (`about:blank`, `data:`, a URL carrying userinfo, a bare host).
///
/// A `file:` URL answers `file://`, which is the origin Chromium's own storage keys use for local
/// pages — inspecting one is exactly why `Page.navigate` may open `file:` at all, and its Application
/// panel must keep working.
pub fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    if scheme.eq_ignore_ascii_case("file") {
        return Some("file://".to_string());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let mut authority = authority.to_ascii_lowercase();
    for (s, port) in [("http", ":80"), ("https", ":443"), ("ws", ":80"), ("wss", ":443")] {
        if scheme == s {
            authority = authority.strip_suffix(port).unwrap_or(&authority).to_string();
        }
    }
    Some(format!("{scheme}://{authority}"))
}

/// Methods whose *parameters* name the data they act on, and the parameter that does it. The
/// frontend may only name an origin or storage key of the page it is inspecting: everything else is
/// another site's data (a cross-origin cookie read, or a wipe of its whole jar).
///
/// `Network.getCookies` (its `urls` array) and `Network.deleteCookies` are handled in [`check`]
/// itself, because their parameters are shaped differently.
pub const ORIGIN_SCOPED: &[(&str, &str)] = &[
    ("Storage.clearDataForOrigin", "origin"),
    ("Storage.clearDataForStorageKey", "storageKey"),
    ("Storage.clearSharedStorageEntries", "ownerOrigin"),
    ("Storage.deleteSharedStorageEntry", "ownerOrigin"),
    ("Storage.getSharedStorageEntries", "ownerOrigin"),
    ("Storage.getSharedStorageMetadata", "ownerOrigin"),
    ("Storage.getUsageAndQuota", "origin"),
    ("Storage.overrideQuotaForOrigin", "origin"),
    ("Storage.resetSharedStorageBudget", "ownerOrigin"),
    ("Storage.setSharedStorageEntry", "ownerOrigin"),
    ("Storage.trackCacheStorageForOrigin", "origin"),
    ("Storage.trackCacheStorageForStorageKey", "storageKey"),
    ("Storage.trackIndexedDBForOrigin", "origin"),
    ("Storage.trackIndexedDBForStorageKey", "storageKey"),
    ("Storage.untrackCacheStorageForOrigin", "origin"),
    ("Storage.untrackCacheStorageForStorageKey", "storageKey"),
    ("Storage.untrackIndexedDBForOrigin", "origin"),
    ("Storage.untrackIndexedDBForStorageKey", "storageKey"),
];

/// Cookie and storage-bucket methods judged by a parameter that is not a plain origin string.
const COOKIE_SCOPED: &[&str] = &["Network.deleteCookies", "Network.getCookies", "Storage.deleteStorageBucket"];

/// Whether a verdict for `method` needs the inspected page's frame origins. Reading them costs a
/// frame walk, so the caller only does it for these few methods (everything else passes an empty
/// list and is judged by name alone).
pub fn needs_origins(method: &str) -> bool {
    COOKIE_SCOPED.contains(&method) || ORIGIN_SCOPED.iter().any(|(m, _)| *m == method)
}

/// The refusal a cross-origin parameter gets. One message for all of them: DevTools shows it, and it
/// says what the rule is rather than what the caller asked for.
const CROSS_ORIGIN: &str = "DevTools may only name the inspected page's own origins in sta";

/// The verdict for one message from the frontend.
pub fn check(req: &Request) -> Verdict {
    let method = req.method;
    if method.is_empty() || method.len() > 128 {
        return Verdict::Unknown;
    }
    let (domain, _) = method.split_once('.').unwrap_or((method, ""));
    match domain {
        "Browser" => return Verdict::Refuse("the browser target is not available to DevTools in sta"),
        "SystemInfo" => return Verdict::Refuse("SystemInfo is not available to DevTools in sta"),
        "Target" => return target_method_allowed(method, req.session),
        _ => {}
    }
    match method {
        "Page.setDownloadBehavior" => return Verdict::Refuse("downloads are sta's"),
        "DOM.setFileInputFiles" => return Verdict::Refuse("file inputs are the user's"),
        "Network.getAllCookies" => return Verdict::Refuse("cookies of other sites are not available"),
        "Storage.getCookies" | "Storage.setCookies" | "Storage.clearCookies" => {
            return Verdict::Refuse("cookies of other sites are not available");
        }
        "Page.navigate" if !req.param("url").is_some_and(navigable) => {
            return Verdict::Refuse("DevTools may only navigate to http(s), file and about URLs");
        }
        // The Application panel asks for the frame tree's own cookies with no `urls` at all; a
        // `urls` array is how another site's cookies were reachable.
        "Network.getCookies" | "Network.deleteCookies" => {
            if let Some(urls) = req.params.and_then(|p| p.get("urls")).and_then(serde_json::Value::as_array) {
                for url in urls {
                    match url.as_str().and_then(origin_of) {
                        Some(origin) if req.origins.contains(&origin) => {}
                        _ => return Verdict::Refuse(CROSS_ORIGIN),
                    }
                }
            }
            if let Some(url) = req.param("url")
                && !req.is_own_origin(url)
            {
                return Verdict::Refuse(CROSS_ORIGIN);
            }
            if let Some(domain) = req.param("domain")
                && !req.is_own_cookie_domain(domain)
            {
                return Verdict::Refuse(CROSS_ORIGIN);
            }
        }
        // `{bucket: {storageKey, name}}`.
        "Storage.deleteStorageBucket" => {
            let key = req.params.and_then(|p| p.get("bucket")).and_then(|b| b.get("storageKey")).and_then(serde_json::Value::as_str);
            match key {
                Some(key) if req.is_own_origin(key) => {}
                _ => return Verdict::Refuse(CROSS_ORIGIN),
            }
        }
        _ => {}
    }
    if let Some((_, param)) = ORIGIN_SCOPED.iter().find(|(m, _)| *m == method) {
        match req.param(param) {
            Some(value) if req.is_own_origin(value) => {}
            _ => return Verdict::Refuse(CROSS_ORIGIN),
        }
    }
    if FRONTEND_METHODS.binary_search(&method).is_ok() { Verdict::Allow } else { Verdict::Unknown }
}

/// Whether a nested session may be admitted for a target `Target.attachedToTarget` reported.
pub fn nested_target_allowed(target_type: &str, url: &str) -> bool {
    if !NESTED_TYPES.contains(&target_type) {
        return false;
    }
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    !scheme.is_some_and(|s| NESTED_FORBIDDEN_SCHEMES.contains(&s.as_str()))
}

/// The S14 inventory: every protocol method the DevTools frontend of CEF 152 / Chromium 152 sends
/// with each panel opened and used, measured through the bridge (`C:/ast/tmp/ext-design/gates-p2.md`
/// §S14). **Sorted** — [`check`] binary-searches it — and each entry is a method the frontend, not
/// sta, sends.
///
/// A method missing here is refused with a protocol error and logged, so extending this list is the
/// fix for "a DevTools feature says it failed"; it is deliberately not a domain prefix list, so a
/// method Chromium adds later cannot arrive allowed (SEC-1).
pub const FRONTEND_METHODS: &[&str] = &[
    "Accessibility.disable",
    "Accessibility.enable",
    "Accessibility.getAXNodeAndAncestors",
    "Accessibility.getChildAXNodes",
    "Accessibility.getFullAXTree",
    "Accessibility.getPartialAXTree",
    "Accessibility.getRootAXNode",
    "Accessibility.queryAXTree",
    "Animation.disable",
    "Animation.enable",
    "Animation.resolveAnimation",
    "Animation.seekAnimations",
    "Animation.setPaused",
    "Animation.setPlaybackRate",
    "Audits.checkContrast",
    "Audits.checkFormsIssues",
    "Audits.disable",
    "Audits.enable",
    "Audits.getEncodedResponse",
    "Autofill.disable",
    "Autofill.enable",
    "Autofill.setAddresses",
    "BackgroundService.clearEvents",
    "BackgroundService.setRecording",
    "BackgroundService.startObserving",
    "BackgroundService.stopObserving",
    "CSS.addRule",
    "CSS.collectClassNames",
    "CSS.createStyleSheet",
    "CSS.disable",
    "CSS.enable",
    "CSS.forcePseudoState",
    "CSS.forceStartingStyle",
    "CSS.getAnimatedStylesForNode",
    "CSS.getBackgroundColors",
    "CSS.getComputedStyleForNode",
    "CSS.getEnvironmentVariables",
    "CSS.getInlineStylesForNode",
    "CSS.getLayersForNode",
    "CSS.getLocationForSelector",
    "CSS.getLonghandProperties",
    "CSS.getMatchedStylesForNode",
    "CSS.getMediaQueries",
    "CSS.getPlatformFontsForNode",
    "CSS.getStyleSheetText",
    "CSS.resolveValues",
    "CSS.setContainerQueryText",
    "CSS.setEffectivePropertyValueForNode",
    "CSS.setKeyframeKey",
    "CSS.setLocalFontsEnabled",
    "CSS.setMediaText",
    "CSS.setPropertyRulePropertyName",
    "CSS.setRuleSelector",
    "CSS.setScopeText",
    "CSS.setStyleSheetText",
    "CSS.setStyleTexts",
    "CSS.setSupportsText",
    "CSS.startRuleUsageTracking",
    "CSS.stopRuleUsageTracking",
    "CSS.takeComputedStyleUpdates",
    "CSS.takeCoverageDelta",
    "CSS.trackComputedStyleUpdates",
    "CSS.trackComputedStyleUpdatesForNode",
    "CacheStorage.deleteCache",
    "CacheStorage.deleteEntry",
    "CacheStorage.requestCacheNames",
    "CacheStorage.requestCachedResponse",
    "CacheStorage.requestEntries",
    "Console.clearMessages",
    "Console.disable",
    "Console.enable",
    "DOM.collectClassNamesFromSubtree",
    "DOM.copyTo",
    "DOM.describeNode",
    "DOM.disable",
    "DOM.discardSearchResults",
    "DOM.enable",
    "DOM.focus",
    "DOM.getAnchorElement",
    "DOM.getAttributes",
    "DOM.getBoxModel",
    "DOM.getContainerForNode",
    "DOM.getContentQuads",
    "DOM.getDetachedDomNodes",
    "DOM.getDocument",
    "DOM.getElementByRelation",
    "DOM.getFileInfo",
    "DOM.getFlattenedDocument",
    "DOM.getFrameOwner",
    "DOM.getNodeForLocation",
    "DOM.getNodeStackTraces",
    "DOM.getNodesForSubtreeByStyle",
    "DOM.getOuterHTML",
    "DOM.getQueryingDescendantsForContainer",
    "DOM.getRelayoutBoundary",
    "DOM.getSearchResults",
    "DOM.getTopLayerElements",
    "DOM.hideHighlight",
    "DOM.markUndoableState",
    "DOM.moveTo",
    "DOM.performSearch",
    "DOM.pushNodeByPathToFrontend",
    "DOM.pushNodesByBackendIdsToFrontend",
    "DOM.querySelector",
    "DOM.querySelectorAll",
    "DOM.redo",
    "DOM.removeAttribute",
    "DOM.removeNode",
    "DOM.requestChildNodes",
    "DOM.requestNode",
    "DOM.resolveNode",
    "DOM.scrollIntoViewIfNeeded",
    "DOM.setAttributeValue",
    "DOM.setAttributesAsText",
    "DOM.setInspectedNode",
    "DOM.setNodeName",
    "DOM.setNodeStackTracesEnabled",
    "DOM.setNodeValue",
    "DOM.setOuterHTML",
    "DOM.undo",
    "DOMDebugger.getEventListeners",
    "DOMDebugger.removeDOMBreakpoint",
    "DOMDebugger.removeEventListenerBreakpoint",
    "DOMDebugger.removeInstrumentationBreakpoint",
    "DOMDebugger.removeXHRBreakpoint",
    "DOMDebugger.setBreakOnCSPViolation",
    "DOMDebugger.setDOMBreakpoint",
    "DOMDebugger.setEventListenerBreakpoint",
    "DOMDebugger.setInstrumentationBreakpoint",
    "DOMDebugger.setXHRBreakpoint",
    "DOMSnapshot.captureSnapshot",
    "DOMStorage.clear",
    "DOMStorage.disable",
    "DOMStorage.enable",
    "DOMStorage.getDOMStorageItems",
    "DOMStorage.removeDOMStorageItem",
    "DOMStorage.setDOMStorageItem",
    "Database.disable",
    "Database.enable",
    "Debugger.continueToLocation",
    "Debugger.disable",
    "Debugger.disassembleWasmModule",
    "Debugger.enable",
    "Debugger.evaluateOnCallFrame",
    "Debugger.getPossibleBreakpoints",
    "Debugger.getScriptSource",
    "Debugger.getStackTrace",
    "Debugger.pause",
    "Debugger.removeBreakpoint",
    "Debugger.restartFrame",
    "Debugger.resume",
    "Debugger.searchInContent",
    "Debugger.setAsyncCallStackDepth",
    "Debugger.setBlackboxExecutionContexts",
    "Debugger.setBlackboxPatterns",
    "Debugger.setBlackboxedRanges",
    "Debugger.setBreakpoint",
    "Debugger.setBreakpointByUrl",
    "Debugger.setBreakpointOnFunctionCall",
    "Debugger.setBreakpointsActive",
    "Debugger.setInstrumentationBreakpoint",
    "Debugger.setPauseOnExceptions",
    "Debugger.setReturnValue",
    "Debugger.setScriptSource",
    "Debugger.setSkipAllPauses",
    "Debugger.setVariableValue",
    "Debugger.stepInto",
    "Debugger.stepOut",
    "Debugger.stepOver",
    "DeviceOrientation.clearDeviceOrientationOverride",
    "DeviceOrientation.setDeviceOrientationOverride",
    "Emulation.clearDeviceMetricsOverride",
    "Emulation.clearIdleOverride",
    "Emulation.getOverriddenSensorInformation",
    "Emulation.resetPageScaleFactor",
    "Emulation.setAutoDarkModeOverride",
    "Emulation.setAutomationOverride",
    "Emulation.setCPUThrottlingRate",
    "Emulation.setDataSaverOverride",
    "Emulation.setDefaultBackgroundColorOverride",
    "Emulation.setDeviceMetricsOverride",
    "Emulation.setDevicePostureOverride",
    "Emulation.setDisabledImageTypes",
    "Emulation.setEmitTouchEventsForMouse",
    "Emulation.setEmulatedMedia",
    "Emulation.setEmulatedOSTextScale",
    "Emulation.setEmulatedVisionDeficiency",
    "Emulation.setFocusEmulationEnabled",
    "Emulation.setGeolocationOverride",
    "Emulation.setHardwareConcurrencyOverride",
    "Emulation.setIdleOverride",
    "Emulation.setLocaleOverride",
    "Emulation.setPageScaleFactor",
    "Emulation.setPressureSourceOverrideEnabled",
    "Emulation.setPressureStateOverride",
    "Emulation.setSafeAreaInsetsOverride",
    "Emulation.setScriptExecutionDisabled",
    "Emulation.setScrollbarsHidden",
    "Emulation.setSensorOverrideEnabled",
    "Emulation.setSensorOverrideReadings",
    "Emulation.setSmallViewportHeightDifferenceOverride",
    "Emulation.setTimezoneOverride",
    "Emulation.setTouchEmulationEnabled",
    "Emulation.setUserAgentOverride",
    "EventBreakpoints.disable",
    "EventBreakpoints.removeInstrumentationBreakpoint",
    "EventBreakpoints.setInstrumentationBreakpoint",
    "Extensions.getStorageItems",
    "Fetch.continueRequest",
    "Fetch.continueResponse",
    "Fetch.continueWithAuth",
    "Fetch.disable",
    "Fetch.enable",
    "Fetch.failRequest",
    "Fetch.fulfillRequest",
    "Fetch.getResponseBody",
    "Fetch.takeResponseBodyAsStream",
    "FileSystem.getDirectory",
    "HeapProfiler.addInspectedHeapObject",
    "HeapProfiler.collectGarbage",
    "HeapProfiler.disable",
    "HeapProfiler.enable",
    "HeapProfiler.getHeapObjectId",
    "HeapProfiler.getObjectByHeapObjectId",
    "HeapProfiler.getSamplingProfile",
    "HeapProfiler.startSampling",
    "HeapProfiler.startTrackingHeapObjects",
    "HeapProfiler.stopSampling",
    "HeapProfiler.stopTrackingHeapObjects",
    "HeapProfiler.takeHeapSnapshot",
    "IO.close",
    "IO.read",
    "IO.resolveBlob",
    "IndexedDB.clearObjectStore",
    "IndexedDB.deleteDatabase",
    "IndexedDB.disable",
    "IndexedDB.enable",
    "IndexedDB.requestData",
    "IndexedDB.requestDatabase",
    "IndexedDB.requestDatabaseNames",
    "Input.dispatchKeyEvent",
    "Input.dispatchMouseEvent",
    "Input.dispatchTouchEvent",
    "Input.emulateTouchFromMouseEvent",
    "Input.setIgnoreInputEvents",
    "Inspector.disable",
    "Inspector.enable",
    "LayerTree.compositingReasons",
    "LayerTree.disable",
    "LayerTree.enable",
    "LayerTree.loadSnapshot",
    "LayerTree.makeSnapshot",
    "LayerTree.profileSnapshot",
    "LayerTree.releaseSnapshot",
    "LayerTree.replaySnapshot",
    "LayerTree.snapshotCommandLog",
    "Log.clear",
    "Log.disable",
    "Log.enable",
    "Log.startViolationsReport",
    "Log.stopViolationsReport",
    "Media.disable",
    "Media.enable",
    "Memory.getDOMCounters",
    "Memory.prepareForLeakDetection",
    "Network.clearAcceptedEncodingsOverride",
    "Network.clearBrowserCache",
    "Network.deleteCookies",
    "Network.disable",
    "Network.emulateNetworkConditions",
    "Network.emulateNetworkConditionsByRule",
    "Network.enable",
    "Network.enableDeviceBoundSessions",
    "Network.enableReportingApi",
    "Network.fetchSchemefulSite",
    "Network.getCertificate",
    "Network.getCookies",
    "Network.getRequestPostData",
    "Network.getResponseBody",
    "Network.getSecurityIsolationStatus",
    "Network.loadNetworkResource",
    "Network.overrideNetworkState",
    "Network.replayXHR",
    "Network.searchInResponseBody",
    "Network.setAcceptedEncodings",
    "Network.setAttachDebugStack",
    "Network.setBlockedURLs",
    "Network.setBypassServiceWorker",
    "Network.setCacheDisabled",
    "Network.setCookieControls",
    "Network.setExtraHTTPHeaders",
    "Network.setUserAgentOverride",
    "Network.streamResourceContent",
    "Overlay.disable",
    "Overlay.enable",
    "Overlay.getGridHighlightObjectsForTest",
    "Overlay.getHighlightObjectForTest",
    "Overlay.getSourceOrderHighlightObjectForTest",
    "Overlay.hideHighlight",
    "Overlay.highlightFrame",
    "Overlay.highlightNode",
    "Overlay.highlightQuad",
    "Overlay.highlightRect",
    "Overlay.highlightSourceOrder",
    "Overlay.setInspectMode",
    "Overlay.setPausedInDebuggerMessage",
    "Overlay.setShowAdHighlights",
    "Overlay.setShowContainerQueryOverlays",
    "Overlay.setShowDebugBorders",
    "Overlay.setShowDisplayCutout",
    "Overlay.setShowFPSCounter",
    "Overlay.setShowFlexOverlays",
    "Overlay.setShowGridOverlays",
    "Overlay.setShowHinge",
    "Overlay.setShowHitTestBorders",
    "Overlay.setShowIsolatedElements",
    "Overlay.setShowLayoutShiftRegions",
    "Overlay.setShowPaintRects",
    "Overlay.setShowScrollBottleneckRects",
    "Overlay.setShowScrollSnapOverlays",
    "Overlay.setShowViewportSizeOnResize",
    "Overlay.setShowWebVitals",
    "Overlay.setShowWindowControlsOverlay",
    "Page.addScriptToEvaluateOnNewDocument",
    "Page.bringToFront",
    "Page.captureScreenshot",
    "Page.captureSnapshot",
    "Page.crash",
    "Page.createIsolatedWorld",
    "Page.deleteCookie",
    "Page.disable",
    "Page.enable",
    "Page.getAppId",
    "Page.getAppManifest",
    "Page.getFrameTree",
    "Page.getInstallabilityErrors",
    "Page.getLayoutMetrics",
    "Page.getManifestIcons",
    "Page.getNavigationHistory",
    "Page.getOriginTrials",
    "Page.getPermissionsPolicyState",
    "Page.getResourceContent",
    "Page.getResourceTree",
    "Page.navigate",
    "Page.navigateToHistoryEntry",
    "Page.produceCompilationCache",
    "Page.reload",
    "Page.removeScriptToEvaluateOnNewDocument",
    "Page.resetNavigationHistory",
    "Page.searchInResource",
    "Page.setAdBlockingEnabled",
    "Page.setBypassCSP",
    "Page.setDocumentContent",
    "Page.setFontFamilies",
    "Page.setFontSizes",
    "Page.setInterceptFileChooserDialog",
    "Page.setLifecycleEventsEnabled",
    "Page.setPrerenderingAllowed",
    "Page.setRPHRegistrationMode",
    "Page.setSPCTransactionMode",
    "Page.setWebLifecycleState",
    "Page.stopLoading",
    "Performance.disable",
    "Performance.enable",
    "Performance.getMetrics",
    "PerformanceTimeline.enable",
    "Preload.disable",
    "Preload.enable",
    "Profiler.disable",
    "Profiler.enable",
    "Profiler.getBestEffortCoverage",
    "Profiler.setSamplingInterval",
    "Profiler.start",
    "Profiler.startPreciseCoverage",
    "Profiler.stop",
    "Profiler.stopPreciseCoverage",
    "Profiler.takePreciseCoverage",
    "Runtime.addBinding",
    "Runtime.awaitPromise",
    "Runtime.callFunctionOn",
    "Runtime.compileScript",
    "Runtime.disable",
    "Runtime.discardConsoleEntries",
    "Runtime.enable",
    "Runtime.evaluate",
    "Runtime.getExceptionDetails",
    "Runtime.getHeapUsage",
    "Runtime.getIsolateId",
    "Runtime.getProperties",
    "Runtime.globalLexicalScopeNames",
    "Runtime.queryObjects",
    "Runtime.releaseObject",
    "Runtime.releaseObjectGroup",
    "Runtime.removeBinding",
    "Runtime.runIfWaitingForDebugger",
    "Runtime.runScript",
    "Runtime.setAsyncCallStackDepth",
    "Runtime.setCustomObjectFormatterEnabled",
    "Runtime.setMaxCallStackSizeToCapture",
    "Runtime.terminateExecution",
    "Security.disable",
    "Security.enable",
    "Security.setIgnoreCertificateErrors",
    "ServiceWorker.deliverPushMessage",
    "ServiceWorker.disable",
    "ServiceWorker.dispatchSyncEvent",
    "ServiceWorker.enable",
    "ServiceWorker.setForceUpdateOnPageLoad",
    "ServiceWorker.skipWaiting",
    "ServiceWorker.startWorker",
    "ServiceWorker.stopAllWorkers",
    "ServiceWorker.stopWorker",
    "ServiceWorker.unregister",
    "ServiceWorker.updateRegistration",
    "Storage.clearDataForOrigin",
    "Storage.clearDataForStorageKey",
    "Storage.clearSharedStorageEntries",
    "Storage.clearTrustTokens",
    "Storage.deleteSharedStorageEntry",
    "Storage.deleteStorageBucket",
    "Storage.getInterestGroupDetails",
    "Storage.getRelatedWebsiteSets",
    "Storage.getSharedStorageEntries",
    "Storage.getSharedStorageMetadata",
    "Storage.getStorageKey",
    "Storage.getStorageKeyForFrame",
    "Storage.getTrustTokens",
    "Storage.getUsageAndQuota",
    "Storage.overrideQuotaForOrigin",
    "Storage.resetSharedStorageBudget",
    "Storage.runBounceTrackingMitigations",
    "Storage.sendPendingAttributionReports",
    "Storage.setAttributionReportingLocalTestingMode",
    "Storage.setAttributionReportingTracking",
    "Storage.setInterestGroupAuctionTracking",
    "Storage.setInterestGroupTracking",
    "Storage.setSharedStorageEntry",
    "Storage.setSharedStorageTracking",
    "Storage.setStorageBucketTracking",
    "Storage.trackCacheStorageForOrigin",
    "Storage.trackCacheStorageForStorageKey",
    "Storage.trackIndexedDBForOrigin",
    "Storage.trackIndexedDBForStorageKey",
    "Storage.untrackCacheStorageForOrigin",
    "Storage.untrackCacheStorageForStorageKey",
    "Storage.untrackIndexedDBForOrigin",
    "Storage.untrackIndexedDBForStorageKey",
    "Tracing.end",
    "Tracing.getCategories",
    "Tracing.recordClockSyncMarker",
    "Tracing.requestMemoryDump",
    "Tracing.start",
    "WebAudio.disable",
    "WebAudio.enable",
    "WebAudio.getRealtimeData",
    "WebAuthn.addCredential",
    "WebAuthn.addVirtualAuthenticator",
    "WebAuthn.clearCredentials",
    "WebAuthn.disable",
    "WebAuthn.enable",
    "WebAuthn.getCredential",
    "WebAuthn.getCredentials",
    "WebAuthn.removeCredential",
    "WebAuthn.removeVirtualAuthenticator",
    "WebAuthn.setAutomaticPresenceSimulation",
    "WebAuthn.setCredentialProperties",
    "WebAuthn.setResponseOverrideBits",
    "WebAuthn.setUserVerified",
    "WebMCP.disable",
    "WebMCP.enable",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One verdict, the way `devtools_cdp` asks for it.
    fn judge(method: &str, session: Session, params: Option<serde_json::Value>, origins: &[&str]) -> Verdict {
        let own: Vec<String> = origins.iter().map(|o| (*o).to_string()).collect();
        check(&Request { method, session, params: params.as_ref(), origins: &own })
    }

    fn own(method: &str, session: Session) -> Verdict {
        judge(method, session, None, &[])
    }

    #[test]
    fn the_inventory_is_sorted_and_unique() {
        for pair in FRONTEND_METHODS.windows(2) {
            assert!(pair[0] < pair[1], "{} must sort before {}", pair[0], pair[1]);
        }
        // Every entry is `Domain.method`, and no entry is in a domain the always-refused list owns.
        for m in FRONTEND_METHODS {
            let (domain, rest) = m.split_once('.').unwrap_or((m, ""));
            assert!(!rest.is_empty() && !domain.is_empty(), "{m} is not Domain.method");
            assert!(!matches!(domain, "Browser" | "SystemInfo" | "Target"), "{m} can never be allowed");
        }
        // Every method judged by a parameter is in the fixture (otherwise the rule is dead code)
        // and pays for the frame walk that judges it.
        for (m, _) in ORIGIN_SCOPED {
            assert!(FRONTEND_METHODS.binary_search(m).is_ok(), "{m} is scoped but not in the fixture");
            assert!(needs_origins(m), "{m}");
        }
        for m in COOKIE_SCOPED {
            assert!(FRONTEND_METHODS.binary_search(m).is_ok(), "{m} is scoped but not in the fixture");
            assert!(needs_origins(m), "{m}");
        }
    }

    #[test]
    fn the_inventory_is_allowed_on_both_session_kinds() {
        for m in FRONTEND_METHODS {
            // The few entries judged by a parameter get one that names the inspected page itself.
            let params = match *m {
                "Page.navigate" => Some(json!({ "url": "https://example.com/" })),
                "Storage.deleteStorageBucket" => Some(json!({ "bucket": { "storageKey": "https://example.com/", "name": "b" } })),
                "Network.deleteCookies" => Some(json!({ "name": "c", "url": "https://example.com/p" })),
                _ => ORIGIN_SCOPED.iter().find(|(name, _)| name == m).map(|(_, param)| {
                    let mut params = serde_json::Map::new();
                    params.insert((*param).to_string(), json!("https://example.com/"));
                    serde_json::Value::Object(params)
                }),
            };
            for session in [Session::Own, Session::Nested] {
                assert_eq!(judge(m, session, params.clone(), &["https://example.com"]), Verdict::Allow, "{m} on {session:?}");
            }
        }
    }

    #[test]
    fn the_always_refused_methods_are_refused_everywhere() {
        let refused = [
            "Browser.close",
            "Browser.getVersion",
            "Browser.setDownloadBehavior",
            "Browser.setWindowBounds",
            "SystemInfo.getInfo",
            "SystemInfo.getProcessInfo",
            "Target.createTarget",
            "Target.createBrowserContext",
            "Target.attachToTarget",
            "Target.attachToBrowserTarget",
            "Target.exposeDevToolsProtocol",
            "Target.setDiscoverTargets",
            "Target.setRemoteLocations",
            "Target.closeTarget",
            "Target.activateTarget",
            "Page.setDownloadBehavior",
            "DOM.setFileInputFiles",
            "Network.getAllCookies",
            "Storage.getCookies",
            "Storage.setCookies",
            "Storage.clearCookies",
        ];
        for m in refused {
            for session in [Session::Own, Session::Nested] {
                assert!(matches!(own(m, session), Verdict::Refuse(_)), "{m} on {session:?} must be refused");
            }
        }
    }

    #[test]
    fn target_methods_follow_their_session() {
        assert_eq!(own("Target.setAutoAttach", Session::Own), Verdict::Allow);
        assert_eq!(own("Target.setAutoAttach", Session::Nested), Verdict::Allow);
        assert_eq!(own("Target.autoAttachRelated", Session::Nested), Verdict::Allow);
        assert_eq!(own("Target.getTargetInfo", Session::Own), Verdict::Allow);
        assert!(matches!(own("Target.getTargetInfo", Session::Nested), Verdict::Refuse(_)));
        assert_eq!(own("Target.detachFromTarget", Session::Own), Verdict::Allow);
    }

    #[test]
    fn page_navigate_is_limited_to_real_pages() {
        for url in ["https://a.com/", "http://a.com/", "about:blank", "file:///C:/x.html", "FILE:///C:/x.html"] {
            assert_eq!(judge("Page.navigate", Session::Own, Some(json!({ "url": url })), &[]), Verdict::Allow, "{url}");
        }
        for url in ["sta://settings/", "chrome://version", "devtools://devtools/bundled/x.html", "javascript:1", "data:text/html,x", ""] {
            assert!(matches!(judge("Page.navigate", Session::Own, Some(json!({ "url": url })), &[]), Verdict::Refuse(_)), "{url}");
        }
        assert!(matches!(own("Page.navigate", Session::Own), Verdict::Refuse(_)), "no URL at all");
    }

    /// T1/T3: the parameters of a cookie or storage method are what makes it another site's data.
    #[test]
    fn cookies_and_storage_stay_inside_the_inspected_page() {
        let page = ["http://127.0.0.1:8841", "https://sub.example.com"];
        // The call the Application panel really makes: no `urls` at all.
        assert_eq!(judge("Network.getCookies", Session::Own, Some(json!({})), &page), Verdict::Allow);
        assert_eq!(judge("Network.getCookies", Session::Own, None, &page), Verdict::Allow);
        // Its own frames' URLs are fine, in any spelling of the default port.
        for url in ["http://127.0.0.1:8841/p.html", "HTTP://127.0.0.1:8841/", "https://sub.example.com:443/x"] {
            assert_eq!(judge("Network.getCookies", Session::Own, Some(json!({ "urls": [url] })), &page), Verdict::Allow, "{url}");
        }
        // Another host, another port, another scheme: all another site.
        for url in ["http://localhost:8841/", "http://127.0.0.1:9000/", "https://127.0.0.1:8841/", "http://example.com/", "about:blank", ""] {
            assert!(matches!(judge("Network.getCookies", Session::Own, Some(json!({ "urls": [url] })), &page), Verdict::Refuse(_)), "{url}");
        }
        // One bad URL in a list of good ones is enough.
        assert!(matches!(
            judge("Network.getCookies", Session::Own, Some(json!({ "urls": ["http://127.0.0.1:8841/", "http://localhost:8841/"] })), &page),
            Verdict::Refuse(_)
        ));
        // Deleting a cookie: by URL, or by the cookie domain DevTools shows.
        assert_eq!(judge("Network.deleteCookies", Session::Own, Some(json!({ "name": "a", "url": "https://sub.example.com/x" })), &page), Verdict::Allow);
        assert_eq!(judge("Network.deleteCookies", Session::Own, Some(json!({ "name": "a", "domain": ".example.com" })), &page), Verdict::Allow);
        assert_eq!(judge("Network.deleteCookies", Session::Own, Some(json!({ "name": "a", "domain": "sub.example.com" })), &page), Verdict::Allow);
        for bad in [json!({ "name": "a", "url": "http://localhost:8841/" }), json!({ "name": "a", "domain": "other.com" }), json!({ "name": "a", "domain": "" })] {
            assert!(matches!(judge("Network.deleteCookies", Session::Own, Some(bad.clone()), &page), Verdict::Refuse(_)), "{bad}");
        }
        // Storage: the destructive twin, and the quota/track family.
        assert_eq!(
            judge("Storage.clearDataForOrigin", Session::Own, Some(json!({ "origin": "http://127.0.0.1:8841", "storageTypes": "cookies" })), &page),
            Verdict::Allow
        );
        assert!(matches!(
            judge("Storage.clearDataForOrigin", Session::Own, Some(json!({ "origin": "http://localhost:8841", "storageTypes": "cookies" })), &page),
            Verdict::Refuse(_)
        ));
        // A partitioned storage key keeps its own origin at the front.
        assert_eq!(
            judge("Storage.clearDataForStorageKey", Session::Own, Some(json!({ "storageKey": "https://sub.example.com/^31https://top.example" })), &page),
            Verdict::Allow
        );
        assert!(matches!(
            judge("Storage.clearDataForStorageKey", Session::Own, Some(json!({ "storageKey": "https://evil.example/^31https://sub.example.com" })), &page),
            Verdict::Refuse(_)
        ));
        assert_eq!(
            judge("Storage.deleteStorageBucket", Session::Own, Some(json!({ "bucket": { "storageKey": "https://sub.example.com/", "name": "b" } })), &page),
            Verdict::Allow
        );
        assert!(matches!(
            judge("Storage.deleteStorageBucket", Session::Own, Some(json!({ "bucket": { "storageKey": "https://evil.example/", "name": "b" } })), &page),
            Verdict::Refuse(_)
        ));
        // A missing parameter is refused, not allowed by omission, and so is an empty origin list
        // (a page whose frames could not be read is not a licence to name any origin).
        for m in ORIGIN_SCOPED.iter().map(|(m, _)| *m).chain(["Storage.deleteStorageBucket"]) {
            assert!(matches!(judge(m, Session::Own, Some(json!({})), &page), Verdict::Refuse(_)), "{m} without its parameter");
            assert!(
                matches!(
                    judge(m, Session::Own, Some(json!({ "origin": "https://sub.example.com", "storageKey": "https://sub.example.com", "ownerOrigin": "https://sub.example.com" })), &[]),
                    Verdict::Refuse(_)
                ),
                "{m} with no known origins"
            );
        }
    }

    #[test]
    fn only_the_scoped_methods_pay_for_a_frame_walk() {
        for m in ["Network.getCookies", "Network.deleteCookies", "Storage.clearDataForOrigin", "Storage.deleteStorageBucket", "Storage.getUsageAndQuota"] {
            assert!(needs_origins(m), "{m}");
        }
        for m in ["Runtime.evaluate", "DOM.getDocument", "Page.navigate", "Storage.getStorageKeyForFrame", "Network.getResponseBody"] {
            assert!(!needs_origins(m), "{m}");
        }
    }

    #[test]
    fn origins_are_compared_by_scheme_host_and_port() {
        assert_eq!(origin_of("https://a.com/x?y#z").as_deref(), Some("https://a.com"));
        assert_eq!(origin_of("HTTPS://A.COM:443/").as_deref(), Some("https://a.com"));
        assert_eq!(origin_of("http://a.com:80").as_deref(), Some("http://a.com"));
        assert_eq!(origin_of("http://a.com:8080/").as_deref(), Some("http://a.com:8080"));
        // A local page's frames are all `file://`, which is also the storage key Chromium uses.
        assert_eq!(origin_of("file:///C:/x.html").as_deref(), Some("file://"));
        assert_eq!(origin_of("FILE:///C:/x.html").as_deref(), Some("file://"));
        for bad in ["about:blank", "data:text/html,x", "javascript:1", "", "://x", "https://user@a.com/"] {
            assert_eq!(origin_of(bad), None, "{bad}");
        }
    }

    /// The methods ordinary use needed and the S14 sweep had missed (FID-2, FID-3, FID-7).
    #[test]
    fn the_fixture_covers_what_using_a_panel_needs() {
        for m in [
            "Runtime.compileScript", // an incomplete console expression continues instead of failing
            "Runtime.runScript",
            "Emulation.resetPageScaleFactor", // leaving device mode
            "Debugger.setBreakpointOnFunctionCall",
            "Debugger.disassembleWasmModule",
            "DOM.getDetachedDomNodes",
            "Audits.getEncodedResponse",
            "Emulation.setSensorOverrideEnabled",
            "Emulation.getOverriddenSensorInformation",
            "Fetch.enable",
            "Fetch.fulfillRequest",
            "Network.getCertificate",
        ] {
            assert_eq!(own(m, Session::Own), Verdict::Allow, "{m}");
        }
        // A method the frontend cannot use on a page session is not in the fixture: Chromium answers
        // "wasn't found" for it, and listing it only implied sta supported it (FID-8).
        assert_eq!(own("ServiceWorker.inspectWorker", Session::Own), Verdict::Unknown);
    }

    #[test]
    fn unknown_methods_are_not_refusals() {
        // The difference matters: a refusal is a rule, an unknown method is a gap in the fixture.
        assert_eq!(own("Fantasy.doThing", Session::Own), Verdict::Unknown);
        assert_eq!(own("Runtime.notAMethod", Session::Own), Verdict::Unknown);
        assert_eq!(own("", Session::Own), Verdict::Unknown);
        assert_eq!(own(&"x".repeat(200), Session::Own), Verdict::Unknown);
    }

    #[test]
    fn nested_sessions_are_admitted_by_type_and_scheme() {
        assert!(nested_target_allowed("iframe", "https://other.example/frame"));
        assert!(nested_target_allowed("worker", "https://a.com/w.js"));
        assert!(nested_target_allowed("service_worker", "https://a.com/sw.js"));
        assert!(nested_target_allowed("shared_worker", "https://a.com/s.js"));
        assert!(nested_target_allowed("worklet", "https://a.com/p.js"));
        assert!(nested_target_allowed("iframe", "about:blank"));
        for t in ["page", "browser", "tab", "other", "webview", "background_page", ""] {
            assert!(!nested_target_allowed(t, "https://a.com/"), "{t}");
        }
        for url in ["sta://sidebar/", "devtools://devtools/bundled/devtools_app.html", "chrome://version", "chrome-untrusted://x", "chrome-search://local-ntp"] {
            assert!(!nested_target_allowed("iframe", url), "{url}");
        }
    }
}
