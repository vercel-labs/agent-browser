import { RecordingScene } from './use-case-scenes/recording-scene';
import { PerformanceScene } from './use-case-scenes/performance-scene';
import { DriveScene } from './use-case-scenes/drive-scene';
import { SnapshotsScene } from './use-case-scenes/snapshots-scene';
import { NetworkScene } from './use-case-scenes/network-scene';
import { SessionsScene } from './use-case-scenes/sessions-scene';

export type UseCaseVisualKind =
  | 'drive'
  | 'snapshots'
  | 'verify'
  | 'performance'
  | 'network'
  | 'sessions';

/** One scene per homepage card. */
export function UseCaseVisual({ kind }: { kind: UseCaseVisualKind }) {
  if (kind === 'drive') return <DriveScene />;
  if (kind === 'snapshots') return <SnapshotsScene />;
  if (kind === 'verify') return <RecordingScene />;
  if (kind === 'performance') return <PerformanceScene />;
  if (kind === 'network') return <NetworkScene />;
  return <SessionsScene />;
}
