// Topbar translate chip (PROTOCOL §8 "Translation"): says what "Translate page" is doing to the
// page you are looking at, and is the one-click way to turn it on and off.
//
// Unlike the agent chip this is shown on every live web tab, including when nothing is happening:
// "turn translation on" needs something to press, not just an indicator that it is already on. The
// main button always means "do the other thing" — translate an untranslated page, restore a
// translated one — which is exactly what the shell does when it is asked to translate twice.

import { html } from '/common/vendor/htm-preact.js';
import { dispatch } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { classNames, translateLanguageName } from '/common/util.js';

const fire = (command) => dispatch(command).catch((e) => console.error('[topbar] dispatch failed', command, e));

/** 0-100, and never 100 until it really is done. */
const percent = (done, total) => (total > 0 ? Math.min(99, Math.round((done / total) * 100)) : 0);

export function TranslateChip({ current, settings }) {
  // Nothing to translate on sta's own pages, and nothing at all without a tab.
  if (!current || current.internal) return null;
  const status = current.translate ?? { stage: 'idle' };
  const target = status.target || settings?.translateLanguage || 'en';
  const name = translateLanguageName(target);

  let tone = 'is-idle';
  let label = 'Translate';
  let title = `Translate this page to ${name}`;
  let action = null;

  if (status.stage === 'working') {
    tone = 'is-working';
    const phase = status.phase === 'images' ? 'Reading images' : status.phase === 'collecting' ? 'Reading page' : 'Translating';
    label = status.total > 0 ? `${phase} ${percent(status.done, status.total)}%` : `${phase}…`;
    title = `Translating this page to ${name}`;
    action = html`<button type="button" class="tr-chip-action is-stop" tabindex="-1" title="Stop translating" onClick=${() => fire({ type: 'cancelTranslate' })}>
      <span class="tr-stop-square" aria-hidden="true" /><span>Stop</span>
    </button>`;
  } else if (status.stage === 'translated') {
    tone = 'is-on';
    label = `Translated · ${target.toUpperCase()}`;
    title = [
      `This page is translated to ${name}`,
      status.images > 0 ? `${status.strings} texts and ${status.images} images` : `${status.strings} texts`,
      status.note || null,
    ]
      .filter(Boolean)
      .join('\n');
    action = html`<button type="button" class="tr-chip-action is-original" tabindex="-1" title="Show the original page" onClick=${() => fire({ type: 'translatePage' })}>
      <span>Original</span>
    </button>`;
  } else if (status.stage === 'failed') {
    tone = 'is-error';
    label = 'Translation failed';
    title = `${status.message}\nClick to try again`;
  }

  return html`<div class=${classNames('tr-chip', tone)} role="group" aria-label="Translation">
    <button type="button" class="tr-chip-main" tabindex="-1" title=${title} onClick=${() => fire({ type: 'translatePage' })}>
      <${Icon} name="globe" size=${14} strokeWidth=${1.8} />
      <span class="tr-chip-label" role="status" aria-live="polite">${label}</span>
    </button>
    ${action}
  </div>`;
}
