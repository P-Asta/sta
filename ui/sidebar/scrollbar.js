// Thin overlay scrollbar for the sidebar lists (PROTOCOL §5.8): the native scrollbar is hidden so
// rows keep their full width and stay aligned with the fixed rows above; this 4 px thumb floats
// over the right frame gap, shows while scrolling or hovering the list, widens on hover and can be
// dragged.

import { html, useEffect, useRef } from '/common/vendor/htm-preact.js';
import * as motion from '/common/motion.js';

const INSET = 4;
const MIN_THUMB = 28;

/** @param {{scrollerRef: {current: HTMLElement|null}, contentRef: {current: HTMLElement|null}}} props */
export function OverlayScrollbar({ scrollerRef, contentRef }) {
  const track = useRef(null);
  const thumb = useRef(null);

  useEffect(() => {
    const sc = scrollerRef.current;
    const tr = track.current;
    const th = thumb.current;
    if (!sc || !tr || !th) return undefined;
    let hideTimer = 0;
    let geometry = { trackH: 0, thumbH: 0, range: 0 };

    const update = () => {
      const { scrollTop, scrollHeight, clientHeight } = sc;
      const range = scrollHeight - clientHeight;
      // Soft edge fades only where there is more content to scroll to.
      sc.classList.toggle('fade-top', range > 1 && scrollTop > 1);
      sc.classList.toggle('fade-bottom', range > 1 && scrollTop < range - 1);
      if (range <= 1) {
        tr.hidden = true;
        return;
      }
      tr.hidden = false;
      const trackH = clientHeight - INSET * 2;
      const thumbH = Math.max(MIN_THUMB, (trackH * clientHeight) / scrollHeight);
      const y = ((trackH - thumbH) * scrollTop) / range;
      geometry = { trackH, thumbH, range };
      th.style.height = `${thumbH}px`;
      th.style.transform = `translateY(${y}px)`;
    };
    const flash = () => {
      tr.classList.add('is-active');
      clearTimeout(hideTimer);
      hideTimer = setTimeout(() => tr.classList.remove('is-active'), 900);
    };
    const onScroll = () => {
      update();
      flash();
    };

    const onThumbDown = (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      th.setPointerCapture(e.pointerId);
      tr.classList.add('is-dragging');
      const startY = e.clientY;
      const startTop = sc.scrollTop;
      const perPx = geometry.range / Math.max(1, geometry.trackH - geometry.thumbH);
      const move = (ev) => {
        sc.scrollTop = startTop + (ev.clientY - startY) * perPx;
      };
      const up = () => {
        th.removeEventListener('pointermove', move);
        tr.classList.remove('is-dragging');
        flash();
      };
      th.addEventListener('pointermove', move);
      th.addEventListener('pointerup', up, { once: true });
      th.addEventListener('pointercancel', up, { once: true });
    };
    const onTrackDown = (e) => {
      if (e.target !== tr || e.button !== 0) return;
      e.preventDefault();
      const r = th.getBoundingClientRect();
      sc.scrollBy({ top: e.clientY < r.top ? -sc.clientHeight * 0.9 : sc.clientHeight * 0.9, behavior: motion.scrollBehavior() });
    };

    sc.addEventListener('scroll', onScroll, { passive: true });
    th.addEventListener('pointerdown', onThumbDown);
    tr.addEventListener('pointerdown', onTrackDown);
    const ro = new ResizeObserver(update);
    ro.observe(sc);
    if (contentRef.current) ro.observe(contentRef.current);
    update();
    return () => {
      clearTimeout(hideTimer);
      ro.disconnect();
      sc.removeEventListener('scroll', onScroll);
      th.removeEventListener('pointerdown', onThumbDown);
      tr.removeEventListener('pointerdown', onTrackDown);
    };
  }, []);

  return html`<div ref=${track} class="overlay-scrollbar" hidden aria-hidden="true">
    <div ref=${thumb} class="overlay-scrollbar-thumb" />
  </div>`;
}
