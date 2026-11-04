'use client';

import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';

interface Sequence {
  active: number;
  count: number;
  finish: (index: number) => void;
}

const SequenceContext = createContext<Sequence | null>(null);
const IndexContext = createContext(-1);

/** Stage handles visibility; the sequence only grants each scene its turn. */
export function SceneSequence({ count, className, children }: {
  count: number; className: string; children: ReactNode;
}) {
  const [active, setActive] = useState(0);
  const finish = useCallback((index: number) => {
    setActive(current => current === index ? current + 1 : current);
  }, []);
  const value = useMemo(() => ({ active, count, finish }), [active, count, finish]);
  return <SequenceContext.Provider value={value}><ul className={className}>{children}</ul></SequenceContext.Provider>;
}

export function SceneSequenceItem({ index, children }: { index: number; children: ReactNode }) {
  return <IndexContext.Provider value={index}>{children}</IndexContext.Provider>;
}

export function useSceneSequence() {
  const sequence = useContext(SequenceContext);
  const index = useContext(IndexContext);
  return { sequence, index };
}
