import { Children, type ReactNode } from 'react';
import styles from './tweet-rail.module.css';
import { TweetRailTrack } from './tweet-rail-track';

export function TweetRail({ children }: { children: ReactNode }) {
  const items = Children.toArray(children);

  return (
    <div role="region" aria-label="Posts from developers" aria-roledescription="carousel">
      <div className={styles.rail}>
        <TweetRailTrack>
          {items.map((item, index) => (
            <li className={styles.item} key={index}>{item}</li>
          ))}
        </TweetRailTrack>
        <div aria-hidden className={`${styles.scrim} ${styles.start}`} />
        <div aria-hidden className={`${styles.scrim} ${styles.end}`} />
      </div>
    </div>
  );
}
