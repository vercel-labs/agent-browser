/** Keep only the latest request's result and completion callbacks active. */
export function createLatestRequest() {
  let generation = 0;
  return {
    invalidate() {
      generation++;
    },
    async run<T>(
      load: () => Promise<T>,
      apply: (value: T) => void,
      settle: () => void,
    ): Promise<void> {
      const request = ++generation;
      try {
        const value = await load();
        if (request === generation) apply(value);
      } finally {
        if (request === generation) settle();
      }
    },
  };
}
