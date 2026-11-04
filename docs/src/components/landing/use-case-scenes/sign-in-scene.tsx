'use client';

// Adapted from Stageshow's form-error: kit Fields with real text, and a
// full-width button whose idle label and busy group are layers in one slot
// that crossfade, so nothing slides sideways. Beats: the email is typed, the
// password is typed as dots, Sign in is pressed, it settles on Signed in.
//
// Real strings: the two placeholders, the typed email, the button's labels.
import { Check, Field, Stage, type Act } from '../../stageshow/vendor/kit';
import { CheckCircleFill } from '../compat';
import { SceneFrame, SCENE_SIZE } from './scene-frame';
import { useScenePlayback } from './scene-playback';
import { StageAgentLabel } from './stage-agent-label';
import './vercel.css';
import './sign-in-scene.css';

type Focus = 'email' | 'password' | 'none';
type State = { email: string; password: string; field: Focus; sending: boolean; done: boolean };

const EMAIL = 'me@example.com';
const PASSWORD = 'correct-horse';

const initial: State = { email: '', password: '', field: 'none', sending: false, done: false };
const poster: State = { email: EMAIL, password: PASSWORD.slice(0, 6), field: 'password', sending: false, done: false };

/** A repeatable 0 to 1 value per keystroke, so every loop types alike. */
const jitter = (i: number) => {
  const x = Math.sin(i * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
};

/**
 * Types the way a person does rather than at one even pace: a short reach
 * before the first key, quick bursts within a word, and small hesitations
 * after the characters that end a part (`@`, `.`, `-`).
 */
async function typeNaturally(a: Act<State>, key: 'email' | 'password', text: string, base: number, from = 0) {
  await a.wait(140);
  for (let i = from + 1; i <= text.length; i++) {
    a.put({ [key]: text.slice(0, i) } as Partial<State>);
    const ch = text[i - 1];
    let delay = base * (0.6 + 0.8 * jitter(i + text.length));
    if (ch === '@') delay += 150;
    else if (ch === '.' || ch === '-') delay += 90;
    await a.wait(delay);
  }
}

async function script(a: Act<State>) {
  // Resume exactly where the poster pauses, midway through the password.
  a.mark('resume');
  await typeNaturally(a, 'password', PASSWORD, 52, poster.password.length);
  await a.wait(320);
  a.mark('sign-in');
  await a.click({ at: 'sign-in', x: .78, y: .5 });
  await a.set({ field: 'none', sending: true });
  await a.move({ at: 'sign-in', x: .78, y: 1.3 }, { duration: 500 });
  await a.wait(800);
  a.mark('signed-in');
  await a.set({ done: true });
  await a.wait(1400);

  // Wrap through the beginning, stopping at the same half-typed password.
  a.mark('reset');
  await a.snap(initial);
  a.mark('email');
  await a.click({ at: 'email', x: .78, y: .5 });
  await a.set({ field: 'email' });
  await typeNaturally(a, 'email', EMAIL, 62);
  await a.wait(260);
  a.mark('password');
  await a.click({ at: 'password', x: .78, y: .5 });
  await a.set({ field: 'password' });
  await typeNaturally(a, 'password', poster.password, 52);
  await a.set(poster);
  await a.cursor('pointer');
  a.mark('done');
}

export function SignInScene() {
  const { onPointerEnter, run, ...playback } = useScenePlayback({ script, poster });
  return <SceneFrame kind="sign-in" onPointerEnter={onPointerEnter} caption="An agent types an email and a password into a sign-in form and presses Sign in. The button settles on Signed in with a check.">
    <Stage key={run} {...SCENE_SIZE} {...playback} poster={poster} theme="light" unframed className="look-vercel ab-sign-in" cursor={{ at: { at: 'password', x: .78, y: .5 }, style: 'arrow', still: true, idle: false }}>
      {s => <>
        <div className="ab-sign-in-form">
          <Field placeholder="Email" value={s.email} focused={s.field === 'email'} box={{ 'data-target': 'email' }} />
          <Field placeholder="Password" value={s.password} password focused={s.field === 'password'} box={{ 'data-target': 'password' }} />
          <span className="ab-sign-in-submit" data-target="sign-in" data-cursor="pointer">
            <span className="ab-sign-in-layer" data-on={!s.sending || undefined}>Sign in</span>
            <span className="ab-sign-in-layer" data-on={s.sending || undefined}>
              {/* The kit's spinner while it works, then Geist's own check-circle */}
              <span className="ab-sign-in-status">
                <span className="ab-sign-in-spinner" data-on={!s.done || undefined}><Check on={false} pending={s.sending && !s.done} size={14} /></span>
                <span className="ab-sign-in-done" data-on={s.done || undefined}><CheckCircleFill size={14} /></span>
              </span>
              {/* Left-aligned in a cell as wide as the longer label, so the
                  check holds still while the words crossfade beside it */}
              <span className="ab-sign-in-labels">
                <span className="ab-sign-in-label" data-on={!s.done || undefined}>Signing in</span>
                <span className="ab-sign-in-label" data-on={s.done || undefined}>Signed in</span>
              </span>
            </span>
          </span>
        </div>
        <StageAgentLabel />
      </>}
    </Stage>
  </SceneFrame>;
}
