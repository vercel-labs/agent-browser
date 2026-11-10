import type { CSSProperties } from 'react';
import { Plus, X } from 'lucide-react';
import { Window, type Layout } from '../../stageshow/vendor/kit';
import { FIXED, PR_NUMBER, PR_TITLE, TYPO, type HeroFrame } from './hero-timeline';
import './deployment-browser.css';

/* Three pages, all in the page's own pixels from its top-left corner. Everything
   that matters sits inside browser scope's crop (--ab-visible of the page's
   width, about 250px of its height).

   Production and the Preview: one page, a nav bar's bottom line, then its hero
   (a two-line headline and two solid buttons). The headline is misspelled on
   production and corrected on the Preview. Each capture takes
   most of the hero.
   GitHub: the pull request, nothing but its title and the description the two
   screenshots are pasted into, Before and After side by side. */

const VISIBLE: Record<Layout, number> = { wide: 460, compact: 280 };
/** The captured region: most of the hero, from under the nav line to near the
 * crop's foot, across the page's width; a fixed box, so the capture never
 * depends on measured text. The hero's content sits inside it (.ab-mock-hero
 * in the CSS places it). */
export const SHOT_REGION: Record<Layout, { x: number; y: number; width: number; height: number }> = {
  wide: { x: 12, y: 40, width: 436, height: 184 },
  compact: { x: 10, y: 40, width: 260, height: 172 },
};
/** The copy is the region in a frame of the page's color, SHOT_PAD all round:
 * exactly the marquee's box (its gap is the same 8px). */
export const SHOT_PAD = 8;
export const shotSize = (layout: Layout) => ({ width: SHOT_REGION[layout].width + SHOT_PAD * 2, height: SHOT_REGION[layout].height + SHOT_PAD * 2 });
/** The pasted images' scale in the description: side by side across its width. */
export const ATTACH_SCALE: Record<Layout, number> = { wide: .468, compact: .47 };

/** The hero, simplified to what a marketing page always has: a two-line
 * headline (the blandest one there is, so only its typos catch the eye;
 * `fixed` spells it right: the Preview) and two buttons as plain solid fills.
 * Placed by CSS (.ab-mock-hero). */
function Hero({ fixed }: { fixed: boolean }) {
  return (
    <div className="ab-mock-hero">
      <div className="ab-headline">Build {(fixed ? FIXED : TYPO)[0]}<br />{(fixed ? FIXED : TYPO)[1]}, faster</div>
      <span className="ab-mock-actions"><span className="ab-mock-primary" /><span className="ab-mock-secondary" /></span>
    </div>
  );
}

/** A screenshot of the hero, as it rides with the cursor and lands in the
 * description: the page itself, cropped to the region and framed in the page's
 * color. `fixed` is the Preview's; otherwise production's, with the typo. */
export function HeadlineShot({ layout, fixed }: { layout: Layout; fixed: boolean }) {
  const region = SHOT_REGION[layout];
  const size = shotSize(layout);
  return (
    <div className="ab-shot" style={{ width: size.width, height: size.height, '--ab-visible': `${VISIBLE[layout]}px` } as CSSProperties}>
      <div className="ab-shot-page" style={{ left: SHOT_PAD - region.x, top: SHOT_PAD - region.y }}>
        <Hero fixed={fixed} />
      </div>
    </div>
  );
}

/** The page around the headline: the nav bar's bottom line. */
const PageFrame = () => <span className="ab-page-nav" />;

const tabLabel = (key: string, title: string) => (
  <span className="ab-tab-label" key={key}><span className="ab-tab-title">{title}</span><span className="ab-tab-close" data-target={`close-${key}`}><X size={10} /></span></span>
);

/** Every screen and target stays mounted so timeline seeks are deterministic. */
export function BrowserWindow({ frame, layout }: { frame: HeroFrame; layout: Layout }) {
  const region = SHOT_REGION[layout];
  const size = shotSize(layout);
  const scale = ATTACH_SCALE[layout];
  const slot = { width: size.width * scale, height: size.height * scale };
  return (
    <Window
      url={<span className="ab-address-content">{frame.url}</span>}
      urlShape="pill"
      tabs={[
        tabLabel('production', 'Acme'),
        tabLabel('preview', 'Acme'),
        tabLabel('github', `${PR_TITLE} ${PR_NUMBER}`),
        <span className="ab-tab-plus" key="plus" data-target="tab-plus"><Plus size={13} /></span>,
      ]}
      lights="color"
      className="ab-browser" data-target="browser-window" data-active-tab={frame.active} data-preview-tab={frame.previewTab || undefined} data-github-tab={frame.githubTab || undefined} data-many-tabs={frame.manyTabs || undefined}
      body={{ className: 'ab-browser-body' }}
    >
      {/* Where + sits beside one tab and beside two: fixed anchors, so the
          hand's trips to it never jump while a tab opens and pushes + along */}
      <span className="ab-plus-home ab-plus-one" data-target="plus-one" />
      <span className="ab-plus-home ab-plus-two" data-target="plus-two" />
      {/* The hand's resting place on production, where the loop opens and ends */}
      <span className="ab-anchor ab-home" data-target="home" />
      {/* The captured region: the same box on production and the preview */}
      <span className="ab-anchor" data-target="shot-region" style={{ left: region.x, top: region.y, width: region.width, height: region.height }} />

      {/* Production: the headline, with a typo in it */}
      <div className="ab-page" data-on={frame.active === 'production' || undefined}>
        <PageFrame />
        <Hero fixed={false} />
      </div>

      {/* The Preview deployment of the fix: the same page, spelled right */}
      <div className="ab-page" data-on={(frame.previewTab && frame.active === 'preview') || undefined}>
        <PageFrame />
        <Hero fixed />
      </div>

      {/* The pull request: its title, and the description the screenshots are pasted into */}
      <div className="ab-page ab-pr" data-on={(frame.githubTab && frame.active === 'github') || undefined}>
        <div className="ab-pr-title">{PR_TITLE} <span className="ab-pr-number">{PR_NUMBER}</span></div>
        <div className="ab-pr-field">
          <span className="ab-pr-placeholder" data-target="pr-field" data-hidden={!frame.fieldEmpty || undefined}>{frame.fieldFocused && <span className="ab-caret ab-caret-lead" />}Add a description</span>
          {/* Each image's place, reserved at its final size so the flying copy lands on it exactly */}
          {(['Before', 'After'] as const).map((label, i) => (
            <div className="ab-pr-shot" key={label}>
              <span className="ab-pr-caption" data-on={frame.copies[i].caption || undefined}>{label}</span>
              <span className="ab-pr-attach" data-target={`slot-${i}`} data-on={frame.copies[i].attached || undefined} style={{ width: slot.width, height: slot.height }}>
                <span className="ab-pr-attach-shot" style={{ scale: `${scale}` } as CSSProperties}>
                  <HeadlineShot layout={layout} fixed={i === 1} />
                </span>
              </span>
            </div>
          ))}
        </div>
        <span className="ab-anchor ab-pr-rest" data-target="pr-rest" />
      </div>
    </Window>
  );
}
