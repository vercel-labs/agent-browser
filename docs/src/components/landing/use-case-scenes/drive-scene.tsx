'use client';

// The action card. No window: a short Geist form on the card's own ground,
// whole and centered, fields that name themselves by placeholder. Name is
// already filled in. The agent clicks Email and types into it naturally,
// opens the Team select and moves Chris from Marketing to Engineering, then
// presses Save, which settles into Saved with a check. The entered values
// fade out and it runs again; the poster is the form filled in with the
// pointer on Save, about to press it.
//
// The Team menu lists the current team, Marketing, first and the rest in
// alphabetical order, and pops over its select with that first row on the
// select's own line, as a native select does: nothing appears to move as it
// opens, it drops downward over Save (never over the Email just typed), and
// stays inside the card. Engineering is two rows down.
//
// Real strings: the name, the email and its placeholder, the teams, Save/Saved.
import { IconCheck, IconChevronDown } from '@vercel/geistdocs/assets/icons';
import { CheckCircleFill } from '../compat';
import { Field, Stage, type Act, type Point } from '../../stageshow/vendor/kit';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import { StageAgentLabel } from './stage-agent-label';
import '../shared/vercel.css';
import './drive-scene.css';

type Team = 'Design' | 'Engineering' | 'Marketing';
type State = {
  email: string; focused: boolean;
  /** The menu is open; `ticked` is the team it shows checked, fixed while it
   *  is open and closing, so the check never jumps as the menu fades */
  open: boolean; ticked: Team; hl: Team | null;
  team: Team;
  over: boolean; saved: boolean; live: boolean; clearing: boolean;
};

const NAME = 'Chris Tate';
const EMAIL = 'chris@acme.com';
/** The current team first, the rest alphabetical */
const TEAMS: Team[] = ['Marketing', 'Design', 'Engineering'];

/** Before the agent starts: Name filled, email empty, the team it starts on. */
const fresh: State = { email: '', focused: false, open: false, ticked: 'Marketing', hl: null, team: 'Marketing', over: false, saved: false, live: false, clearing: false };
/** The form filled in, the pointer resting on Save, about to press it. */
const poster: State = { ...fresh, email: EMAIL, team: 'Engineering', ticked: 'Engineering', over: true };
/** Where a cued tab opens: the form as it stands at the `email` mark (Name
 *  filled, Email empty, Team on Marketing), straight into typing. Not `reset`:
 *  its clearing beat hides the placeholders, so they would blink. */
const setup = { at: 'email', state: fresh };

/** Right of where the typed address ends, so the I-beam never covers it */
const EMAIL_AT = { at: 'email', x: .7, y: .5 };
/** Right of the value, so the arrow never covers it or the row over it */
const TEAM_AT = { at: 'team', x: .62, y: .55 };
const ENG_AT = { at: 'opt-Engineering', x: .62, y: .55 };
/** Right of the label, so the arrow never covers Save or Saved */
const SAVE_AT = { at: 'save', x: .92, y: .62 };
/** Beside the button on its right, so Saved reads and the agent's badge
 *  stays clear of the card's foot */
const ASIDE_AT = { at: 'save', x: 1.75, y: .45 };

/** A repeatable 0 to 1 value per keystroke, so every loop types alike. */
const jitter = (i: number) => {
  const x = Math.sin(i * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
};

/**
 * Types the way a person does rather than at one even pace: a short reach
 * before the first key, quick bursts within a word, and small hesitations
 * after the characters that end a part (`@`, `.`).
 */
async function typeNaturally(a: Act<State>, text: string, base: number) {
  await a.wait(140);
  for (let i = 1; i <= text.length; i++) {
    a.put({ email: text.slice(0, i) });
    const ch = text[i - 1];
    let delay = base * (0.6 + 0.8 * jitter(i + text.length));
    if (ch === '@') delay += 150;
    else if (ch === '.') delay += 90;
    await a.wait(delay);
  }
}

/** The menu row under the pointer, so the highlight follows the hand the
 *  frame it crosses a row, not when the move ends. */
function rowAt(a: Act<State>, at: Point): Team | null {
  for (const t of TEAMS) {
    const r = a.rect(`opt-${t}`);
    if (at.y >= r.y && at.y < r.y + r.height && at.x >= r.x && at.x < r.x + r.width) return t;
  }
  return null;
}

async function script(a: Act<State>) {
  // Resume where the poster pauses: the form filled, the pointer on Save.
  a.mark('resume');
  await a.wait(320);
  await a.click(SAVE_AT);
  await a.set({ over: false, saved: true, live: true });
  // Saved has rolled in on the button: the payoff is complete.
  await a.wait(420);
  a.mark('saved');
  // The hand drifts off the button so Saved reads, and it holds.
  await a.move(ASIDE_AT, { duration: 560 });
  await a.wait(900);

  // The entered values fade out together, Saved rolls back to Save, and the
  // button's hover shade (if the hand is still on it) eases off with them.
  a.mark('reset');
  await a.set({ clearing: true, saved: false, over: false });
  await a.wait(320);
  await a.snap({ ...fresh, clearing: true });
  await a.set({ clearing: false });
  await a.wait(200);

  a.mark('email');
  await a.move(EMAIL_AT, { duration: 720 });
  await a.click(EMAIL_AT);
  await a.set({ focused: true });
  await typeNaturally(a, EMAIL, 58);
  await a.wait(300);

  // The menu pops with Marketing, checked and highlighted, on the select's
  // line under the pointer; the highlight follows the hand down past Design
  // to Engineering.
  a.mark('team');
  await a.move(TEAM_AT, { duration: 520 });
  await a.click(TEAM_AT);
  await a.set({ focused: false, open: true, ticked: 'Marketing', hl: 'Marketing' });
  await a.wait(260);
  await a.move(ENG_AT, { duration: 460, onMove: at => { const hl = rowAt(a, at); if (hl) a.put({ hl }); } });
  a.put({ hl: 'Engineering' });
  await a.wait(180);
  await a.click(ENG_AT);
  // The picked row blinks once, then the menu closes at once with the new
  // team already in the select under it, as a native select does. Check
  // and highlight stay put until it is gone, so nothing inside it moves.
  a.put({ hl: null });
  await a.wait(70);
  a.put({ hl: 'Engineering' });
  await a.wait(90);
  await a.set({ open: false, team: 'Engineering' });
  await a.wait(120);
  a.put({ hl: null, ticked: 'Engineering' });

  a.mark('save');
  await a.move(SAVE_AT, { duration: 600 });
  await a.set({ over: true });
  a.mark('done');
}

export function DriveScene() {
  const { onPointerEnter, run, ...playback } = useScenePlayback({ script, poster, setup });
  // Opening on the setup, the hand rests where the loop leaves it at `email`
  // (beside Save), so its first move is the same reach up into Email
  const restAt = playback.initial === fresh ? ASIDE_AT : SAVE_AT;
  return <SceneFrame kind="drive" onPointerEnter={onPointerEnter} caption="In a short form with Chris Tate's name already filled in, an agent types chris@acme.com into Email, moves Chris from the Marketing team to Engineering, then presses Save, which confirms Saved.">
    <Stage key={run} {...SCENE_SIZE} {...playback} poster={poster} theme="light" className="look-vercel ab-drive" cursor={{ at: restAt, style: 'arrow', still: true, idle: false }}>
      {s => <>
        <div className="ab-drive-form" data-clearing={s.clearing || undefined}>
          <Field value={NAME} placeholder="Name" className="ab-drive-name" />
          <Field value={s.email} placeholder="Email" focused={s.focused} className="ab-drive-email" box={{ 'data-target': 'email' }} />
          <div className="ab-drive-select" data-target="team" data-cursor="pointer" data-open={s.open || undefined}>
            {/* Every team mounted in one cell; the value changes under the
                menu as it closes, so it never visibly swaps */}
            <span className="ab-drive-value">
              {TEAMS.map(t => <span key={t} className="ab-drive-team" data-on={s.team === t || undefined}>{t}</span>)}
            </span>
            <IconChevronDown size={16} className="ab-drive-chevron" />
          </div>
          {/* Both labels share one cell, so the button never resizes; they roll
              only once it has been pressed, never on mount */}
          <div className="ab-drive-save" data-target="save" data-cursor="pointer" data-over={s.over || undefined} data-live={s.live || undefined}>
            <span className="ab-drive-btn-roll" data-on={!s.saved || undefined}>Save</span>
            <span className="ab-drive-btn-roll" data-on={s.saved || undefined}><CheckCircleFill size={14} />Saved</span>
          </div>
          {/* Pops over the select, the first row on the select's line */}
          <div className="ab-drive-menu" data-open={s.open || undefined} role="listbox">
            {TEAMS.map(t => (
              <div key={t} className="ab-drive-option" role="option" aria-selected={s.ticked === t} data-target={`opt-${t}`} data-cursor="pointer" data-hl={s.hl === t || undefined}>
                {t}
                <IconCheck size={16} className="ab-drive-tick" data-on={s.ticked === t || undefined} />
              </div>
            ))}
          </div>
        </div>
        <StageAgentLabel />
      </>}
    </Stage>
  </SceneFrame>;
}
