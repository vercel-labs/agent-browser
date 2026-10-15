import { Resvg } from '@resvg/resvg-js';
import { readFileSync, writeFileSync, mkdirSync, readdirSync } from 'node:fs';
import { join, basename } from 'node:path';

const dir = new URL('.', import.meta.url).pathname;
const outDir = join(dir, 'out');
mkdirSync(outDir, { recursive: true });

const targets = process.argv.slice(2);
const svgs = (targets.length ? targets : readdirSync(dir).filter(f => f.endsWith('.svg')));

for (const file of svgs) {
  const name = basename(file, '.svg');
  const svg = readFileSync(join(dir, file), 'utf8');
  for (const size of [512, 24]) {
    const resvg = new Resvg(svg, {
      fitTo: { mode: 'width', value: size },
      background: '#ffffff',
    });
    const png = resvg.render().asPng();
    const out = join(outDir, `${name}-${size}.png`);
    writeFileSync(out, png);
    console.log(`${out}  ${size}x${size}  ${png.length} bytes`);
  }
}
