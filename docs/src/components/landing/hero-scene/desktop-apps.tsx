import type { CSSProperties } from 'react';
import { Window } from '../../stageshow/vendor/kit';
import { Minus, Plus } from 'lucide-react';
import { IconImage } from './geist-glyphs';
import { MapArt } from './map-art';
import { wallpaperAt } from './wallpapers';
import { CheckCircleFill } from '../compat';
import type { Targets } from './hero-timeline';
import { DESK_FILE_COUNT, PHOTO_COUNT, PHOTO_PICKS, desktopFrame, desktopPeerPosition, mapsGeometry } from './desktop-timeline';
import './desktop-apps.css';

/** Loose image files: one thumbnail size and shape for all (squarish, 56 × 48),
 * Geist's image icon on an outlined card; no name under it. */
const FILE_TONES = [2, 4, 3] as const;

function DeskThumb() {
  return <span className="ab-desk-thumb"><IconImage size={22} /></span>;
}

/** Photos: a relaxed grid of large squares; about three and a half columns and
 * two rows show. Deleting closes the gaps in reading order. */
const PHOTO_COLS = 6;
const PHOTO_SIZE = 108;
const PHOTO_GAP = 10;
const PHOTO_INSET = 10;
const slot = (k: number) => ({ x: PHOTO_INSET + (k % PHOTO_COLS) * (PHOTO_SIZE + PHOTO_GAP), y: PHOTO_INSET + Math.floor(k / PHOTO_COLS) * (PHOTO_SIZE + PHOTO_GAP) });
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
/** Each photo is a wallpaper from the series (wallpapers.ts), so a click
 * visibly sets it. The two OpenAI clicks show this cycle's next wallpapers;
 * the rest show others, framed a little differently so the grid never repeats
 * at a glance. [wallpaper offset, zoom %, x %, y %] */
const PHOTO_FRAMES: readonly (readonly [number, number, number, number])[] = [
  [3, 140, 20, 30], [5, 170, 70, 40], [0, 100, 50, 50], [6, 150, 40, 60], [2, 200, 50, 80], [4, 130, 30, 70],
  [7, 160, 60, 30], [0, 100, 50, 50], [1, 180, 80, 20], [5, 120, 40, 50], [3, 190, 30, 70], [6, 140, 60, 40],
  [2, 130, 50, 90], [4, 170, 70, 60], [7, 120, 20, 80], [1, 150, 40, 30], [6, 200, 50, 50], [3, 160, 80, 60],
];
/** Photos' view switcher, as macOS draws it in the toolbar */
const PHOTO_VIEWS = ['Years', 'Months', 'All Photos'] as const;

/** The map's two pans, by their press and release points (in the map's pixels). */
const PAN_TARGETS = ['1', '2'] as const;

/** Retained desktop windows. Only the camera changes when the scope changes. */
/** `wallOffset`: the scene's re-basing of the wallpaper series (it only
 * advances in desktop scope), so a clicked photo still matches the wallpaper
 * it sets. */
export function DesktopApps({ time, loop = 0, wallOffset = 0, targets }: { time: number; loop?: number; wallOffset?: number; targets: Targets }) {
  const frame = desktopFrame(time, loop);
  const gemini = desktopPeerPosition('gemini', time, targets);
  const region = targets['desk-marquee'];
  // The box-select grows from the region's corner to Gemini's hand
  const box = region && frame.marquee ? {
    x: Math.round(region.x), y: Math.round(region.y),
    w: Math.round(Math.max(0, Math.min(gemini.x, region.x + region.width) - region.x)),
    h: Math.round(Math.max(0, Math.min(gemini.y, region.y + region.height) - region.y)),
  } : null;
  // The map moves by each drag's hand travel (the three sum to zero, so it ends centered)
  const pan = PAN_TARGETS.reduce((sum, id, i) => {
    const a = targets[`map-pan-${id}a`], b = targets[`map-pan-${id}b`];
    if (!a || !b) return sum;
    return { x: sum.x + (b.x - a.x) * frame.pans[i], y: sum.y + (b.y - a.y) * frame.pans[i] };
  }, { x: 0, y: 0 });
  const picks: readonly number[] = PHOTO_PICKS;
  /** Only the first click is deleted (once the second is the wallpaper) */
  const removed: readonly number[] = [PHOTO_PICKS[0]];
  // Claude drags Maps by its title bar and widens it from its left edge: a
  // transform plus width, so the targets inside keep measuring at home
  const maps = mapsGeometry(time);

  return (
    <>
      {/* Loose files under the browser */}
      <span className="ab-desk-marquee-region" data-target="desk-marquee" />
      {Array.from({ length: DESK_FILE_COUNT }, (_, i) => {
        const file = frame.files[i];
        return <div className={`ab-desk-file ab-desk-file-${i}`} key={i} data-target={`desk-file-${i}`} data-selected={file.selected || undefined}
          style={{ opacity: file.shown * (file.lifted ? 1 - .65 * frame.liftT : 1), transform: file.arrive < 1 ? `translateY(${(-12 * (1 - file.arrive)).toFixed(2)}px) scale(${(.96 + .04 * file.arrive).toFixed(4)})` : undefined }}>
          <span className="ab-desk-thumb-wrap"><DeskThumb /></span>
        </div>;
      })}
      {box && <span className="ab-desk-select" style={{ left: box.x, top: box.y, width: box.w, height: box.h, opacity: frame.marqueeAlpha }} />}

      {/* Maps, top right */}
      <Window lights="color" className="ab-maps" body={{ className: 'ab-maps-body' }}
        style={{ transform: `translate(${(maps.dx - maps.grow).toFixed(2)}px, ${maps.dy.toFixed(2)}px)`, '--maps-grow': `${maps.grow.toFixed(2)}px` } as CSSProperties}>
        {/* Where Claude takes hold: the title bar (in its empty middle) and the left edge */}
        <span className="ab-maps-bar" data-target="maps-bar" />
        <span className="ab-maps-edge" data-target="maps-edge" />
        <div className="ab-map">
          {/* Oversized so a pan never shows an edge; every zoom is about the window's center */}
          <div className="ab-map-layer" style={{ transform: `translate(${pan.x.toFixed(2)}px, ${pan.y.toFixed(2)}px) scale(${frame.zoom.toFixed(4)})` }}>
            <MapArt />
          </div>
          <span className="ab-map-zoom">
            <span className="ab-map-zoom-button" data-target="map-zoom-in"><Plus size={14} /></span>
            <span className="ab-map-zoom-button" data-target="map-zoom-out"><Minus size={14} /></span>
          </span>
          {PAN_TARGETS.map((id) => <span key={id}><span className={`ab-map-pan ab-map-pan-${id}a`} data-target={`map-pan-${id}a`} /><span className={`ab-map-pan ab-map-pan-${id}b`} data-target={`map-pan-${id}b`} /></span>)}
          <span className="ab-maps-rest" data-target="maps-rest" />
        </div>
      </Window>

      {/* Photos: opened from the dock, on top of the browser and Maps, closed again */}
      {/* Opaque early (a window never fades through), zooming out of its dock icon */}
      <div className="ab-photos-frame" data-open={frame.photosOpen || undefined} style={{ opacity: Math.min(1, frame.photosT / .3), transform: `scale(${(.1 + .9 * frame.photosT).toFixed(4)})` }}>
        <Window lights="color" className="ab-photos" title="Library" body={{ className: 'ab-photos-body' }}
          toolbar={<span className="ab-photos-views">{PHOTO_VIEWS.map((view) => <span key={view} className="ab-photos-view" data-on={view === 'All Photos' || undefined}>{view}</span>)}</span>}>
          {/* The close light, where OpenAI clicks to quit (measured from the kit's bar) */}
          <span className="ab-photos-close" data-target="photos-close" />
          <div className="ab-photos-grid">
            {Array.from({ length: PHOTO_COUNT }, (_, i) => {
              const pick = picks.indexOf(i);
              const isPick = pick >= 0;
              // Kept photos move up in reading order by the number deleted before them
              const gone = removed.includes(i);
              const before = frame.deleted ? removed.filter((p) => p < i).length : 0;
              const from = slot(i), to = slot(i - before);
              const at = gone ? from : { x: lerp(from.x, to.x, frame.closeT), y: lerp(from.y, to.y, frame.closeT) };
              const out = gone && frame.deleted ? frame.deleteT : 0;
              // This cycle's two clicks show the next wallpapers; the rest, the series around them
              const [offset, zoom, fx, fy] = PHOTO_FRAMES[i];
              const wall = (isPick ? frame.photoWalls[pick] : frame.cycle * 2 + offset) - wallOffset;
              // Placed by left/top, not a transform: targets are measured from
              // layout, and a translate would put every pick at the grid's corner
              return <span key={i} className="ab-photo" data-target={isPick ? `photo-${pick}` : undefined} data-picked={(isPick && frame.picked[pick]) || undefined}
                style={{ left: Math.round(at.x * 100) / 100, top: Math.round(at.y * 100) / 100, transform: out ? `scale(${(1 - out * .3).toFixed(4)})` : undefined, opacity: 1 - out }}>
                <span className="ab-photo-image" style={{ backgroundImage: wallpaperAt(wall), backgroundSize: isPick ? undefined : `${zoom}%`, backgroundPosition: isPick ? undefined : `${fx}% ${fy}%` }} />
                <span className="ab-photo-check"><CheckCircleFill size={20} /></span>
              </span>;
            })}
          </div>
        </Window>
      </div>

      {/* The dock, as macOS draws it: a glass shelf with big rounded corners,
          the real app icons (Chrome, Maps, Photos), a divider, then the Trash;
          dark dots under running apps */}
      <div className="ab-dock">
        <span className="ab-dock-app" data-running><span className="ab-dock-icon" data-app="chrome" /></span>
        <span className="ab-dock-app" data-running><span className="ab-dock-icon" data-app="maps" /></span>
        <span className="ab-dock-app" data-target="dock-photos" data-running={frame.photosRunning || undefined} data-pressed={frame.dockPhotosPress || undefined}><span className="ab-dock-icon" data-app="photos" /></span>
        <span className="ab-dock-divider" />
        <span className="ab-dock-app ab-dock-trash" data-target="dock-trash" data-hover={frame.trashHover || undefined} data-bump={frame.trashBump || undefined}>
          {/* Empty and full, crossfaded: the bins are glass, so one never shows through the other */}
          <span className="ab-dock-icon" data-app="trash" style={{ opacity: 1 - frame.trashFull }} />
          <span className="ab-dock-icon ab-dock-icon-over" data-app="trash-full" style={{ opacity: frame.trashFull }} />
        </span>
      </div>
      <span className="ab-gemini-rest" data-target="gemini-rest" />
      <span className="ab-openai-rest" data-target="openai-rest" />

      {/* All three files ride under Gemini's hand as a small fanned stack, then shrink into the Trash */}
      {(frame.carrying || (frame.dropT > 0 && frame.dropT < 1)) && <div className="ab-desk-stack" style={{ transform: `translate3d(${(gemini.x + STACK_GRAB.x).toFixed(2)}px, ${(gemini.y + STACK_GRAB.y).toFixed(2)}px, 0) scale(${((.92 + .08 * frame.liftT) * (1 - .6 * frame.dropT)).toFixed(4)})`, opacity: frame.liftT * (1 - frame.dropT) }}>
        {FILE_TONES.map((_, i) => <span key={i} className="ab-desk-stack-item" style={{ transform: `translate(${(i - 1) * 6}px, ${(1 - i) * 4}px) rotate(${(i - 1) * 4}deg)` }}><DeskThumb /></span>)}
      </div>}
    </>
  );
}

/** Where the held file's thumbnail sits from the hand: the hand takes the file
 * at (.85, .7) of its 72 × 74 box (61.2, 51.8), and the thumbnail is at (8, 4)
 * inside it, so the stack's middle thumbnail starts exactly where the file's
 * is, up and left of the hand (the label stays below-right). */
const STACK_GRAB = { x: 8 - 61.2, y: 4 - 51.8 };
