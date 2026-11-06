// Native browser timing shared by the homepage scenes.
export const clock = {
  now: (): number => performance.now(),
  raf: (fn: FrameRequestCallback): number => requestAnimationFrame(fn),
  cancelRaf: (id: number): void => cancelAnimationFrame(id),
  layoutRaf: (fn: FrameRequestCallback): number => requestAnimationFrame(fn),
  cancelLayoutRaf: (id: number): void => cancelAnimationFrame(id),
  timeout: (fn: () => void, ms: number): number => window.setTimeout(fn, ms),
  clearTimeout: (id: number): void => window.clearTimeout(id),
}
