/** The desktop's wallpapers, and the Photos library made of them: soft gray
 * gradients in one family, each a different composition so a change of
 * wallpaper reads at a glance, and none with a hard edge or a point. CSS only:
 * no image to fetch, crisp at any zoom (Photos frames them closer). Written
 * against six lightness tones, darkest to lightest (--ab-wall-0 … --ab-wall-5,
 * browser-desktop-scene.css), which each theme sets: pale grays on light,
 * near-black with quiet lifts on dark. */
const tone = (level: 0 | 1 | 2 | 3 | 4 | 5) => `var(--ab-wall-${level})`;

export const WALLPAPERS: readonly string[] = [
  // Sweep: one long diagonal, light top left to deep lower right
  `linear-gradient(155deg, ${tone(5)} 0%, ${tone(3)} 45%, ${tone(1)} 100%)`,
  // Halo: a soft pool of light high on the right in a deep field
  `radial-gradient(75% 95% at 82% 12%, ${tone(5)} 0%, ${tone(3)} 35%, ${tone(1)} 70%, ${tone(0)} 100%)`,
  // Mist: two glows drifting apart, upper left and lower right, over a dim field
  `radial-gradient(55% 60% at 18% 22%, ${tone(4)} 0%, transparent 70%), radial-gradient(60% 65% at 85% 85%, ${tone(5)} 0%, transparent 70%), linear-gradient(${tone(1)}, ${tone(1)})`,
  // Dusk: light rising low across the middle, behind the dock
  `radial-gradient(120% 70% at 50% 108%, ${tone(5)} 0%, ${tone(3)} 40%, ${tone(1)} 75%, ${tone(0)} 100%)`,
];

export const wallpaperAt = (index: number) => WALLPAPERS[((index % WALLPAPERS.length) + WALLPAPERS.length) % WALLPAPERS.length];
