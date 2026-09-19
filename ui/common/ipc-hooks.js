// Preact hooks over ipc.js.
//
//   const state = useUiState();                                   // latest UiState or null
//   const { data: rows } = useRequest('archive.list', null, [state?.archiveRevision]);

import { useCallback, useEffect, useRef, useState } from './vendor/htm-preact.js';
import { getState, invoke, onState } from './ipc.js';

/** The latest accepted `UiState` (re-renders on every newer snapshot); `null` until the first. */
export function useUiState() {
  const [state, setState] = useState(getState);
  useEffect(() => onState(setState), []);
  return state;
}

/**
 * Run `invoke(cmd, payload)` and track the result. Re-runs when `cmd`, the JSON of `payload`, or
 * any of `deps` change (e.g. `[state.historyRevision]`); stale responses are ignored.
 * @returns {{data: any, error: Error|null, loading: boolean, reload: () => void}}
 */
export function useRequest(cmd, payload = null, deps = []) {
  const [result, setResult] = useState({ data: undefined, error: null, loading: true });
  const [nonce, setNonce] = useState(0);
  const seq = useRef(0);
  const payloadKey = JSON.stringify(payload);

  useEffect(() => {
    const mine = ++seq.current;
    setResult((r) => (r.loading ? r : { ...r, loading: true }));
    invoke(cmd, payload).then(
      (data) => mine === seq.current && setResult({ data, error: null, loading: false }),
      (error) => mine === seq.current && setResult((r) => ({ data: r.data, error, loading: false })),
    );
  }, [cmd, payloadKey, nonce, ...deps]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);
  return { ...result, reload };
}
