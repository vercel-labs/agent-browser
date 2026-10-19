(() => {
  const binding = __BINDING_JSON__;
  const selector = __SELECTOR_JSON__;
  const stopKey = `${binding}_stop`;
  const visible = () => Array.from(document.querySelectorAll(selector)).find(
    element => element.getClientRects().length > 0 && element.checkVisibility({
      checkOpacity: true,
      checkVisibilityCSS: true
    })
  );
  let previous = Boolean(visible());
  const check = () => {
    const element = visible();
    const current = Boolean(element);
    if (current === previous) return;
    previous = current;
    globalThis[binding](JSON.stringify({
      selector,
      state: current ? 'visible' : 'hidden',
      label: element?.getAttribute('aria-label') || element?.textContent?.trim().slice(0, 160) || null,
      observedAt: new Date().toISOString()
    }));
  };
  const observer = new MutationObserver(check);
  observer.observe(document, {subtree: true, childList: true, attributes: true});
  const signals = ['transitionend', 'animationend', 'toggle'];
  for (const signal of signals) document.addEventListener(signal, check, true);
  globalThis[stopKey] = () => {
    observer.disconnect();
    for (const signal of signals) document.removeEventListener(signal, check, true);
    delete globalThis[stopKey];
  };
  return {selector, state: previous ? 'visible' : 'hidden'};
})()
