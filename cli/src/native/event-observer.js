(() => {
  const binding = __BINDING_JSON__;
  const selector = __SELECTOR_JSON__;
  const nonce = "__NONCE__";
  if (globalThis !== globalThis.top) return;
  const stopKey = `${binding}_stop`;
  const observeKey = `${binding}_observe`;
  const doc = Math.random().toString(36).slice(2);
  let seq = 0;
  let previous = null;
  const read = kind => {
    const element = Array.from(document.querySelectorAll(selector)).find(
      element => element.getClientRects().length > 0 && element.checkVisibility({
        checkOpacity: true,
        checkVisibilityCSS: true
      })
    );
    const state = element ? 'visible' : 'hidden';
    const value = element?.getAttribute('aria-label') || element?.textContent?.trim() || null;
    const label = value === null ? null : value.match(/^.{0,160}/su)[0];
    previous = state;
    return {nonce, kind, state, label, truncated: value !== null && label.length < value.length};
  };
  const report = kind => {
    const priorState = previous;
    const observation = read(kind);
    if (kind === 'change' && observation.state === priorState) return null;
    globalThis[binding](JSON.stringify({...observation, doc, seq: ++seq}));
    return observation;
  };
  const check = () => report('change');
  const observer = new MutationObserver(check);
  observer.observe(document, {subtree: true, childList: true, attributes: true, characterData: true});
  const signals = ['transitionend', 'animationend', 'toggle', 'change', 'input'];
  for (const signal of signals) document.addEventListener(signal, check, true);
  window.addEventListener('resize', check);
  globalThis[observeKey] = () => read('initial');
  globalThis[stopKey] = () => {
    observer.disconnect();
    for (const signal of signals) document.removeEventListener(signal, check, true);
    window.removeEventListener('resize', check);
    delete globalThis[observeKey];
    delete globalThis[stopKey];
  };
  return report('initial');
})()
