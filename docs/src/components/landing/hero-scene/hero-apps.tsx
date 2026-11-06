import { Plus, X } from 'lucide-react';
import { Bar, Window } from '../../stageshow/vendor/kit';
import type { HeroFrame } from './hero-timeline';
import './deployment-browser.css';

/** Masonry tiles, abstract images in quiet tones. Heights in px; tone picks the fill. */
type Tile = readonly [height: number, tone: 1 | 2 | 3 | 4];
// Wide, Pinterest-like columns (128px): a side column, the board's two, a side
// column; four across fill the browser, the last runs past browser scope's crop.
const SIDE_LEFT: readonly Tile[] = [[120, 2], [150, 3], [96, 1]];
/** The board: a 2×2 block (264 × 144) whose columns end level, so the capture
 * is a clean rectangle, short enough that the drag ends with room for the
 * cursor's label below it inside browser scope's crop. */
const BOARD: readonly (readonly Tile[])[] = [[[84, 3], [52, 1]], [[60, 2], [76, 4]]];
const SIDE_RIGHT: readonly Tile[] = [[88, 1], [136, 4], [110, 2]];

function Column({ tiles }: { tiles: readonly Tile[] }) {
  return <div className="ab-pin-col">{tiles.map(([h, tone], i) => <span key={i} className="ab-pin-tile" data-tone={tone} style={{ height: h }} />)}</div>;
}

/** The board as captured: its tiles in a white frame with even padding, for
 * the copy that rides with the cursor and lands in the message. */
export function BoardShot() {
  return <div className="ab-board-shot"><div className="ab-board-shot-tiles">{BOARD.map((tiles, i) => <Column key={i} tiles={tiles} />)}</div></div>;
}

const MAIL_ROWS = [92, 74, 104, 66, 86];

function MailRow({ w }: { w: number }) {
  return <div className="ab-mail-row">
    <span className="ab-mail-avatar" />
    <span className="ab-mail-lines"><Bar w={w} h={6} tone={3} /><Bar w={w + 40} h={5} tone={2} /></span>
  </div>;
}

/** A filled value without words: the gray bar fades in whole (a placeholder,
 * not text). The cursor's own I-beam runs along it as it appears. */
function BarValue({ filled, w }: { filled: boolean; w: number }) {
  return <span className="ab-fill" data-on={filled || undefined}><Bar w={w} h={6} tone={3} /></span>;
}

/** A filled field: Geist's input box around a BarValue. Same 28px box as every control. */
function BarField({ filled, w, focused, target }: { filled: boolean; w: number; focused: boolean; target: string }) {
  return <span className="ab-bar-field" data-focused={focused || undefined} data-target={target}>
    <BarValue filled={filled} w={w} />
  </span>;
}


/** Every screen and target stays mounted so timeline seeks are deterministic. */
export function BrowserWindow({ frame }: { frame: HeroFrame }) {
  return (
    <Window
      url={<span className="ab-address-content" />}
      urlShape="pill"
      tabs={[
        <span className="ab-tab-label" key="pin"><Bar w={56} h={5} tone={3} /><span className="ab-tab-close" data-target="close-pin"><X size={10} /></span></span>,
        <span className="ab-tab-label" key="mail"><Bar w={34} h={5} tone={3} /><span className="ab-tab-close" data-target="close-mail"><X size={10} /></span></span>,
        <span className="ab-tab-plus" key="plus" data-target="tab-plus"><Plus size={13} /></span>,
      ]}
      lights="grey"
      className="ab-browser" data-target="browser-window" data-active-tab={frame.active} data-pin-tab={frame.pinTab || undefined} data-mail-tab={frame.mailTab || undefined}
      body={{ className: 'ab-browser-body' }}
    >
      {/* The pin board: logged out, then signed in */}
      {/* Where + sits with no tabs open (measured: 79.5, 14 in the window, 18px): the hand's home at the loop's ends */}
      <span className="ab-plus-home" data-target="plus-home" />
      <div className="ab-page ab-pin-page" data-on={(frame.pinTab && frame.active === 'pin') || undefined} data-signed-in={frame.signedIn || undefined}>
        <div className="ab-pin-header">
          <span className="ab-pin-logo" />
          <span className="ab-pin-search" />
          <span className="ab-pin-rest" data-target="pin-rest" />
          <span className="ab-pin-account">
            <span className="ab-pin-login" data-target="login" />
            <span className="ab-pin-avatar" />
          </span>
        </div>
        <div className="ab-pin-grid">
          <Column tiles={SIDE_LEFT} />
          <div className="ab-pin-board" data-target="board">{BOARD.map((tiles, i) => <Column key={i} tiles={tiles} />)}</div>
          <Column tiles={SIDE_RIGHT} />
        </div>
        <div className="ab-pin-modal" data-on={frame.modal || undefined}>
          <div className="ab-pin-card">
            <Bar w={84} h={8} tone={3} />
            <BarField filled={frame.emailFilled} w={104} focused={frame.emailFocused} target="email" />
            <BarField filled={frame.passwordFilled} w={64} focused={frame.passwordFocused} target="password" />
            <span className="ab-pin-submit" data-target="submit" />
          </div>
        </div>
      </div>

      {/* The inbox in the new tab */}
      <div className="ab-page ab-mail-page" data-on={(frame.mailTab && frame.active === 'mail') || undefined}>
        <aside className="ab-mail-side">
          <span className="ab-mail-compose" data-target="compose" />
          {[54, 42, 48].map((w, i) => <span className="ab-mail-nav" key={i} data-selected={i === 0 || undefined}><Bar w={w} h={5} tone={i === 0 ? 3 : 2} /></span>)}
        </aside>
        <div className="ab-mail-main">
          <div className="ab-mail-slot" style={{ height: 44 * frame.sentRow, opacity: frame.sentRow > .8 ? (frame.sentRow - .8) / .2 : 0 }}><MailRow w={80} /></div>
          {MAIL_ROWS.map((w, i) => <MailRow key={i} w={w} />)}
        </div>
        <span className="ab-mail-rest" data-target="mail-rest" />
        <div className="ab-compose" data-on={frame.composing || undefined}>
          <div className="ab-compose-head"><Bar w={64} h={6} tone={3} /><X size={11} /></div>
          <div className="ab-compose-line" data-target="to"><span className="ab-compose-value"><BarValue filled={frame.toFilled} w={104} /></span></div>
          <div className="ab-compose-line"><Bar w={96} h={6} tone={2} /></div>
          <div className="ab-compose-body">
            <Bar w={128} h={5} tone={2} />
            <span className="ab-compose-attach" data-target="attach" data-on={frame.attached || undefined}><BoardShot /></span>
          </div>
          <div className="ab-compose-foot"><span className="ab-compose-send" data-target="send" /></div>
        </div>
      </div>
    </Window>
  );
}
