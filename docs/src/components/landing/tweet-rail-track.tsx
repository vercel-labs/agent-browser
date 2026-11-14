'use client';

import { useEffect, useRef, type ReactNode } from 'react';
import styles from './tweet-rail.module.css';

/** The quotes remain server-rendered; only scroll activity needs a listener. */
export function TweetRailTrack({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLUListElement>(null);

  useEffect(() => {
    const track = ref.current;
    if (!track) return;
    let idle: ReturnType<typeof setTimeout>;

    const scrolling = () => {
      track.dataset.scrolling = '';
      clearTimeout(idle);
      idle = setTimeout(() => delete track.dataset.scrolling, 180);
    };

    track.addEventListener('scroll', scrolling, { passive: true });
    return () => {
      track.removeEventListener('scroll', scrolling);
      clearTimeout(idle);
    };
  }, []);

  return <ul ref={ref} className={styles.track}>{children}</ul>;
}
