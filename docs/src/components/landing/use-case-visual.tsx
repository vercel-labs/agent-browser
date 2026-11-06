import { RecordingScene } from './use-case-scenes/recording-scene';
import { PerformanceScene } from './use-case-scenes/performance-scene';
import { SignInScene } from './use-case-scenes/sign-in-scene';

export type UseCaseVisualKind =
  | 'verify'
  | 'performance'
  | 'sign-in'
  | 'snapshots'
  | 'network'
  | 'sessions';

/** The three finished scenes and the three reserved positions in the homepage. */
export function UseCaseVisual({ kind }: { kind: UseCaseVisualKind }) {
  if (kind === 'verify') return <RecordingScene />;
  if (kind === 'performance') return <PerformanceScene />;
  if (kind === 'sign-in') return <SignInScene />;
  return <div className="aspect-[4/3] bg-background-200" data-placeholder={kind} aria-hidden="true" />;
}
