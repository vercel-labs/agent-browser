import { ImageResponse } from "next/og";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { parse, type Font } from "opentype.js";

const folder = join(process.cwd(), "src/app/og");

export const size = { width: 1200, height: 630 };

type Theme = "light" | "dark";

const themes = {
  light: {
    background: "#f8f8f8",
    text: "#0a0a0a",
    slash: "#b7b7b7",
    logo: "#000000",
    description: "#6e6e6e",
  },
  dark: {
    background: "#000000",
    text: "#ffffff",
    slash: "#666666",
    logo: "#ffffff",
    description: "#a1a1a1",
  },
} as const;

// Header geometry, halved from the reference card's 2x canvas.
const logoScale = 115 / 47.0185 / 2;
const logoWidth = 183 * logoScale;
const logoHeight = 42 * logoScale;
const logoTop = 111 - 41.44 * logoScale;
const slashX = 64 + logoWidth + 31.5;
const wordX = slashX + 17.75 + 22;
const wordBaseline = 110;

const titleLeft = 61.6875;
const titleBaseline = 280.045;
const titleSize = 80.4;
const titleTracking = -0.0635;
const titleLinePitch = 76;
const titleMaxWidth = 700;
const maxTitleLines = 3;
const titleShrinkStep = 4;
const titleMinSize = 56;
const maxTitleLinesAtMin = 4;
const titleCenterShift = 38.04;

// A two-line title carries the reference card's per-line metrics.
const titleLayouts = [
  { left: 61.6875, baseline: 280.045, size: 80.095, letterSpacing: -5.085 },
  { left: 61.405, baseline: 356.125, size: 80.69, letterSpacing: -5.125 },
];

const descriptionLeft = 61.6875;
const descriptionSize = 26;
const descriptionTracking = -0.52;
const descriptionLinePitch = descriptionSize * 1.32;
const descriptionMaxWidth = 460;
const descriptionGap = 22;

type Assets = { regular: Font; semibold: Font; logo: string };

let assets: Promise<Assets> | undefined;

function loadAssets() {
  assets ??= Promise.all([
    readFile(join(folder, "Geist-Regular.ttf")),
    readFile(join(folder, "Geist-SemiBold.ttf")),
    readFile(join(folder, "labs-logo.svg"), "utf-8"),
  ]).then(([regular, semibold, logo]) => ({
    regular: parse(toArrayBuffer(regular)),
    semibold: parse(toArrayBuffer(semibold)),
    logo,
  }));
  return assets;
}

function toArrayBuffer(buffer: Buffer) {
  return buffer.buffer.slice(buffer.byteOffset, buffer.byteOffset + buffer.byteLength) as ArrayBuffer;
}

type Line = { text: string; left: number; baseline: number; size: number; letterSpacing: number };

function wrap(font: Font, text: string, lineSize: number, tracking: number, maxWidth: number) {
  const lines: string[] = [];
  let line = "";
  for (const word of text.split(/\s+/).filter(Boolean)) {
    const next = line ? `${line} ${word}` : word;
    if (line && font.getAdvanceWidth(next, lineSize, { kerning: true, letterSpacing: tracking }) > maxWidth) {
      lines.push(line);
      line = word;
    } else {
      line = next;
    }
  }
  if (line) lines.push(line);
  return lines;
}

function titleLine(text: string, index: number): Line {
  const layout = titleLayouts[index];
  if (layout) return { text, ...layout };
  const last = titleLayouts[titleLayouts.length - 1]!;
  const offset = index - titleLayouts.length + 1;
  return { text, ...last, baseline: last.baseline + offset * titleLinePitch };
}

function titleLines(font: Font, title: string | string[]): Line[] {
  if (Array.isArray(title)) {
    const shift = -titleCenterShift * (title.length - 2);
    return title.map((text, index) => {
      const line = titleLine(text, index);
      return { ...line, baseline: line.baseline + shift };
    });
  }
  let lineSize = titleSize;
  let maxLines = maxTitleLines;
  let lines = wrap(font, title, lineSize, titleTracking, titleMaxWidth);
  while (lines.length > maxLines && lineSize > titleMinSize) {
    lineSize = Math.max(titleMinSize, lineSize - titleShrinkStep);
    if (lineSize === titleMinSize) maxLines = maxTitleLinesAtMin;
    lines = wrap(font, title, lineSize, titleTracking, titleMaxWidth);
  }
  const pitch = (lineSize * titleLinePitch) / titleSize;
  const firstBaseline = titleBaseline - (pitch / 2) * (lines.length - 2);
  return lines.map((text, index) => ({
    text,
    left: titleLeft,
    baseline: firstBaseline + index * pitch,
    size: lineSize,
    letterSpacing: titleTracking * lineSize,
  }));
}

function descriptionLine(text: string, index: number, firstBaseline: number): Line {
  return {
    text,
    left: descriptionLeft,
    baseline: firstBaseline + index * descriptionLinePitch,
    size: descriptionSize,
    letterSpacing: descriptionTracking,
  };
}

function linePath(font: Font, lines: Line[]) {
  return lines
    .map((line) =>
      font
        .getPath(line.text, line.left, line.baseline, line.size, {
          kerning: true,
          letterSpacing: line.letterSpacing / line.size,
        })
        .toPathData(3),
    )
    .join("");
}

function logoSource(svg: string, fill: string) {
  const sized = svg
    .replace(/width="[^"]*"/, `width="${logoWidth}"`)
    .replace(/height="[^"]*"/, `height="${logoHeight}"`)
    .replace(/fill="#000000"/g, `fill="${fill}"`);
  return `data:image/svg+xml;base64,${Buffer.from(sized).toString("base64")}`;
}

async function renderLabsOg({
  product,
  title,
  description,
  theme = "light",
}: {
  product: string;
  title: string | string[];
  description?: string;
  theme?: Theme;
}) {
  const { regular, semibold, logo } = await loadAssets();
  const palette = themes[theme];
  const capHeight = 40.4273 * logoScale;
  const wordSize = capHeight / (semibold.tables.os2.sCapHeight / semibold.unitsPerEm);
  const slash = `M${slashX + 14.75} 64.5L${slashX + 17.75} 64.5L${slashX + 3.25} 106.75L${slashX} 106.75Z`;
  const titles = titleLines(regular, title);
  const lastBaseline = titles[titles.length - 1]?.baseline ?? titleBaseline;
  const details = description
    ? wrap(regular, description, descriptionSize, descriptionTracking / descriptionSize, descriptionMaxWidth).map(
        (text, index) => descriptionLine(text, index, lastBaseline + descriptionGap + descriptionSize),
      )
    : [];
  const paths =
    linePath(regular, titles) +
    semibold.getPath(product, wordX, wordBaseline, wordSize, { kerning: true, letterSpacing: -0.05 }).toPathData(3);

  return new ImageResponse(
    <div
      style={{
        width: "100%",
        height: "100%",
        display: "flex",
        position: "relative",
        backgroundColor: palette.background,
      }}
    >
      {/* oxlint-disable-next-line next/no-img-element */}
      <img
        alt=""
        width={logoWidth}
        height={logoHeight}
        src={logoSource(logo, palette.logo)}
        style={{ position: "absolute", left: 64, top: logoTop }}
      />
      <svg
        width={size.width}
        height={size.height}
        viewBox={`0 0 ${size.width} ${size.height}`}
        style={{ position: "absolute", left: 0, top: 0 }}
        xmlns="http://www.w3.org/2000/svg"
      >
        <path d={slash} fill={palette.slash} />
        <path d={paths} fill={palette.text} />
        {details.length > 0 && <path d={linePath(regular, details)} fill={palette.description} />}
      </svg>
    </div>,
    { ...size },
  );
}

export { renderLabsOg };
