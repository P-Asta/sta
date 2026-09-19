// The page side of "Translate page" (crates/sta/src/translate.rs).
//
// Installed with `Runtime.evaluate` into a web tab's main world, because the whole point is to
// rewrite what the page itself renders. It keeps every original string, so a second run can put
// the page back exactly as it was instead of translating a translation.
//
// Nothing here talks to the network: sta collects the strings, translates them in the browser
// process and hands the results back in the same order.
(() => {
  const NS = '__staTranslate';
  if (globalThis[NS]) return globalThis[NS].version;

  /** Elements whose text is not prose and must survive untouched. */
  const SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEXTAREA', 'CODE', 'PRE', 'KBD', 'SAMP', 'VAR', 'TT']);
  /** A string worth a round trip: it has a letter somewhere (numbers and punctuation are not prose). */
  const HAS_LETTER = /\p{L}/u;
  /** Leading and trailing whitespace, which the translator strips and we put back. */
  const EDGES = /^(\s*)([\s\S]*?)(\s*)$/;

  /** Text nodes, in document order, that carry translatable prose. */
  const collectNodes = (root) => {
    const out = [];
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        const parent = node.parentElement;
        if (!parent) return NodeFilter.FILTER_REJECT;
        if (SKIP_TAGS.has(parent.tagName)) return NodeFilter.FILTER_REJECT;
        if (parent.isContentEditable) return NodeFilter.FILTER_REJECT;
        if (parent.closest('[translate="no"], .notranslate')) return NodeFilter.FILTER_REJECT;
        if (!HAS_LETTER.test(node.nodeValue || '')) return NodeFilter.FILTER_REJECT;
        return NodeFilter.FILTER_ACCEPT;
      },
    });
    for (let n = walker.nextNode(); n; n = walker.nextNode()) out.push(n);
    return out;
  };

  const state = {
    /** `{ node, original, translated }` per collected text node, indexed as handed to sta. */
    entries: [],
    translated: false,
    /** The elements `imageTargets()` chose, indexed by the id it handed out. */
    imageEls: [],
    /** The live overlay: its root and the listeners keeping it glued to the pictures. */
    imageSync: null,
  };

  /** Takes the overlay down and stops it following the page. */
  const removeOverlay = () => {
    if (state.imageSync) {
      removeEventListener('scroll', state.imageSync.schedule, { capture: true });
      removeEventListener('resize', state.imageSync.schedule);
      state.imageSync.observer.disconnect();
      state.imageSync = null;
    }
    for (const layer of document.querySelectorAll('.__sta-ocr-layer')) layer.remove();
  };

  /**
   * The page's own language, for picking a text recogniser. `<html lang>` is only a tiebreaker:
   * plenty of Korean and Japanese pages ship `lang="en"`, so the script actually on the page wins.
   */
  const detectLanguage = () => {
    const sample = (document.body?.innerText || '').slice(0, 2000);
    const counts = [
      ['ko', /[가-힣]/g],
      ['ja', /[぀-ヿ]/g],
      ['zh', /[一-鿿]/g],
      ['ru', /[Ѐ-ӿ]/g],
      ['ar', /[؀-ۿ]/g],
      ['hi', /[ऀ-ॿ]/g],
    ].map(([tag, re]) => [tag, (sample.match(re) || []).length]);
    const [tag, n] = counts.sort((a, b) => b[1] - a[1])[0];
    // Japanese text is mostly kanji with some kana; kana present at all beats a raw CJK count.
    if (n > 0 && n >= sample.length * 0.05) return tag === 'zh' && counts.find(([t]) => t === 'ja')[1] > 0 ? 'ja' : tag;
    return (document.documentElement.lang || 'en').toLowerCase();
  };

  const api = {
    version: 1,

    /** The strings to translate, in order. Also remembers the nodes they came from. */
    collect() {
      // A second collect while translated would read our own output back.
      if (state.translated) return { texts: [], already: true };
      state.entries = collectNodes(document.body || document.documentElement).map((node) => ({
        node,
        original: node.nodeValue,
      }));
      return {
        texts: state.entries.map((e) => e.original.match(EDGES)[2]),
        already: false,
      };
    },

    /**
     * Write `texts[i]` into the (offset + i)-th collected node, keeping its original edge
     * whitespace. sta calls this once per batch as the batch lands, so a run that fails near the
     * end keeps everything that already worked.
     */
    apply(offset, texts) {
      let changed = 0;
      for (let i = 0; i < texts.length; i++) {
        const entry = state.entries[offset + i];
        if (!entry) break;
        const text = texts[i];
        if (typeof text !== 'string' || !text) continue;
        // The node may have been re-rendered by the page since collect(); leave those alone.
        if (entry.node.nodeValue !== entry.original || !entry.node.isConnected) continue;
        const [, lead, , trail] = entry.original.match(EDGES);
        entry.node.nodeValue = lead + text + trail;
        entry.translated = entry.node.nodeValue;
        changed++;
      }
      // Sticky: a later batch that changes nothing must not un-mark a page that is translated.
      if (changed > 0) state.translated = true;
      return changed;
    },

    /** Put every string we changed back. */
    restore() {
      let restored = 0;
      for (const entry of state.entries) {
        if (entry.translated === undefined) continue;
        if (entry.node.isConnected && entry.node.nodeValue === entry.translated) {
          entry.node.nodeValue = entry.original;
          restored++;
        }
      }
      removeOverlay();
      state.entries = [];
      state.imageEls = [];
      state.translated = false;
      return restored;
    },

    translated: () => state.translated,

    /**
     * Pictures worth reading, in CSS viewport pixels, plus what sta needs to crop a screenshot of
     * the viewport to each of them. Called only after the text pass, so the overlay this later
     * draws can never be collected and sent off to be translated again.
     */
    imageTargets() {
      const seen = new Set();
      const items = [];
      const push = (el) => {
        if (seen.has(el)) return;
        seen.add(el);
        const r = el.getBoundingClientRect();
        // Too small to carry readable text, or off screen: the screenshot is the viewport only.
        if (r.width < 64 || r.height < 64) return;
        if (r.bottom <= 0 || r.right <= 0 || r.top >= innerHeight || r.left >= innerWidth) return;
        const style = getComputedStyle(el);
        if (style.visibility === 'hidden' || Number(style.opacity) <= 0.1) return;
        // A lazy-load placeholder has no pixels yet.
        if (el.tagName === 'IMG' && (!el.complete || el.naturalWidth <= 1)) return;
        items.push({
          id: items.length,
          x: Math.max(0, r.left),
          y: Math.max(0, r.top),
          w: Math.min(r.right, innerWidth) - Math.max(0, r.left),
          h: Math.min(r.bottom, innerHeight) - Math.max(0, r.top),
          area: r.width * r.height,
        });
        state.imageEls.push(el);
      };
      state.imageEls = [];
      for (const el of document.images) push(el);
      for (const el of document.querySelectorAll('canvas')) push(el);
      // `<svg>` is deliberately absent: its <text> is real DOM text collectNodes already took.
      for (const el of document.querySelectorAll('*')) {
        const bg = getComputedStyle(el).backgroundImage;
        if (bg && /^url\((?!["']?data:image\/svg)/i.test(bg) && bg.split('url(').length === 2) push(el);
      }
      // Biggest first, then a hard cap: OCR costs a process and the user is waiting.
      const order = items.map((it, i) => [it, state.imageEls[i]]).sort((a, b) => b[0].area - a[0].area).slice(0, 12);
      state.imageEls = order.map(([, el]) => el);
      return {
        dpr: devicePixelRatio,
        vw: innerWidth,
        vh: innerHeight,
        visible: document.visibilityState === 'visible',
        lang: detectLanguage(),
        items: order.map(([it], id) => ({ id, x: it.x, y: it.y, w: it.w, h: it.h })),
      };
    },

    /**
     * Draw translated text over the text found in the pictures. `layers` is
     * `[{ image, boxes: [{ x, y, w, h, text, ink, paper, rtl }] }]` with the box in the picture's
     * own CSS pixels, measured from its top-left corner.
     */
    applyImages(layers) {
      removeOverlay();
      const root = document.createElement('div');
      // `notranslate` + translate="no" so a later collect() refuses our own output (see acceptNode).
      root.className = '__sta-ocr-layer notranslate';
      root.setAttribute('translate', 'no');
      root.style.cssText = 'position:fixed;inset:0;z-index:2147483646;pointer-events:none;contain:layout style';
      const groups = [];
      let placed = 0;
      for (const layer of layers) {
        const el = state.imageEls[layer.image];
        if (!el || !el.isConnected) continue;
        const boxes = [];
        for (const box of layer.boxes) {
          if (!box.text) continue;
          const div = document.createElement('div');
          div.textContent = box.text;
          div.dir = 'auto';
          div.style.cssText =
            'position:absolute;display:flex;align-items:center;line-height:1;white-space:pre;overflow:hidden;' +
            `justify-content:${box.rtl ? 'flex-end' : 'flex-start'};` +
            `background:${box.paper};color:${box.ink};` +
            `width:${box.w}px;height:${box.h}px;font-size:100px;font-family:system-ui,sans-serif`;
          root.appendChild(div);
          boxes.push({ div, box });
          placed++;
        }
        if (boxes.length) groups.push({ el, boxes });
      }
      document.body.appendChild(root);
      // One measurement each, no shrink loop: scale 100px down by how much it overflowed.
      for (const { boxes } of groups) {
        for (const { div, box } of boxes) {
          const over = div.scrollWidth || 1;
          div.style.fontSize = `${Math.max(6, Math.min(box.h * 0.78, (100 * box.w) / over))}px`;
        }
      }
      const sync = () => {
        for (const { el, boxes } of groups) {
          if (!el.isConnected) {
            for (const { div } of boxes) div.style.display = 'none';
            continue;
          }
          // getBoundingClientRect folds in every ancestor transform and scroll offset already.
          const r = el.getBoundingClientRect();
          for (const { div, box } of boxes) {
            div.style.display = '';
            div.style.transform = `translate(${r.left + box.x}px, ${r.top + box.y}px)`;
          }
        }
      };
      sync();
      let frame = 0;
      const schedule = () => {
        if (frame) return;
        frame = requestAnimationFrame(() => {
          frame = 0;
          sync();
        });
      };
      // Capture: an image inside its own scroller does not bubble a scroll event to the window.
      addEventListener('scroll', schedule, { passive: true, capture: true });
      addEventListener('resize', schedule, { passive: true });
      const observer = new ResizeObserver(schedule);
      for (const { el } of groups) observer.observe(el);
      state.imageSync = { schedule, observer, root };
      return placed;
    },
  };

  globalThis[NS] = api;
  return api.version;
})();
