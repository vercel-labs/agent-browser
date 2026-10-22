(() => {
  const binding = __BINDING_JSON__;
  const selector = __SELECTOR_JSON__;
  const nonce = "__NONCE__";
  if (globalThis !== globalThis.top) return;
  const stopKey = `${binding}_stop`;
  const observeKey = `${binding}_observe`;
  const visible = () => Array.from(document.querySelectorAll(selector)).find(
    element => element.getClientRects().length > 0 && element.checkVisibility({
      checkOpacity: true,
      checkVisibilityCSS: true
    })
  );
  let previous = null;
  const read = kind => {
    const element = visible();
    const state = element ? 'visible' : 'hidden';
    const value = element?.getAttribute('aria-label') || element?.textContent?.trim() || null;
    let label = value;
    let truncated = false;
    if (value !== null) {
      let end = 0;
      let characters = 0;
      while (end < value.length && characters < 160) {
        end += value.codePointAt(end) > 0xFFFF ? 2 : 1;
        characters += 1;
      }
      truncated = end < value.length;
      label = value.slice(0, end);
    }
    return {nonce, kind, state, label, truncated};
  };
  const report = kind => {
    const observation = read(kind);
    if (kind === 'change' && observation.state === previous) return null;
    previous = observation.state;
    globalThis[binding](JSON.stringify(observation));
    return observation;
  };
  const check = () => report('change');
  const observer = new MutationObserver(check);
  observer.observe(document, {subtree: true, childList: true, attributes: true});
  const signals = ['transitionend', 'animationend', 'toggle'];
  for (const signal of signals) document.addEventListener(signal, check, true);
  globalThis[observeKey] = () => {
    const observation = read('initial');
    previous = observation.state;
    return observation;
  };
  globalThis[stopKey] = () => {
    observer.disconnect();
    for (const signal of signals) document.removeEventListener(signal, check, true);
    delete globalThis[observeKey];
    delete globalThis[stopKey];
  };
  return report('initial');
})()
