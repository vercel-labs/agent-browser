'use client';

import { useRef, type ReactNode, type PointerEventHandler } from 'react';
import { useHairline } from './hairline';
import '../../stageshow/host-layers.css';
import './scenes.css';

export const SCENE_SIZE = { width: 400, height: 300, compact: { below: 390, width: 358, height: 300 }, fill: true };

export function SceneFrame({ kind, caption, children, onPointerEnter }: {
  kind: string; caption: string; children: ReactNode; onPointerEnter?: PointerEventHandler<HTMLElement>;
}) {
  const ref = useRef<HTMLElement>(null);
  useHairline(ref);
  return (
    <figure ref={ref} className="ab-feature-scene" data-kind={kind} onPointerEnter={onPointerEnter}>
      <div aria-hidden="true">{children}</div>
      <figcaption className="ab-feature-caption">{caption}</figcaption>
    </figure>
  );
}
