# Probe extensions for `extensions-e2e.mjs`

Small MV3 test extensions written for sta's end-to-end tests. They are **not** copies of real
extensions. Each manifest carries a `key` (a public key only), so its id is fixed; `ids.json` lists
them and a shell unit test (`extension_files.rs`) checks that the keys still produce those ids.

| directory | id | used for |
|---|---|---|
| `probe-windows` | `nbjocpdeikjicjcicjjgijlaekmikkdm` | Loaded with `--load-extension`. Its service worker exposes `self.probe` (called over the DevTools protocol): `tabs.create`, `windows.create` (normal, popup, incognito), `runtime.openOptionsPage` (embedded `options_ui` → `chrome://extensions/?options=<id>`), `identity.launchWebAuthFlow`. Declared pages: `options.html`, `popup.html`; `welcome.html` is web-accessible to every site; `undeclared.html` is neither. Its **action popup** (`popup.html` + `popup.js`, 280 px wide) writes and reads `chrome.storage` and records what `chrome.tabs.query({active, currentWindow})` answers, which is gate S3's measurement; `popup-blank.html` renders nothing on purpose, for the card's honest-failure line, and `popup-slow.html` + `popup-slow.js` render 2 s after loading (what an MV3 popup waiting for a cold service worker looks like), so the card is measured *at* its deadline rather than on a stale answer. `options-hash.html` + `options-hash.js` route themselves to `#general` on load, which is how "one options tab per extension" is checked against a real options page's behaviour. |
| `probe-options` | `ejkbkbldfhcflldjfhkiehclchddjlmn` | Loaded with `--load-extension`: `options_page` opened in a tab by `runtime.openOptionsPage`. |
| `probe-installed` | `hnfmblkigkicddkmmpbafhpkkfoedcbi` | Copied into the test profile's `Extensions/<id>/1.0_0/` to stand for a Web Store install (the post-install path; see `C:/ast/tmp/ext-design/gates-p1.md` S13). Localized name (`_locales`). |

The service worker only acts when a test calls it; it never opens anything by itself.
