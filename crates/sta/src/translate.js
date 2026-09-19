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
      for (const layer of document.querySelectorAll('.__sta-ocr-layer')) layer.remove();
      state.entries = [];
      state.translated = false;
      return restored;
    },

    translated: () => state.translated,
  };

  globalThis[NS] = api;
  return api.version;
})();
