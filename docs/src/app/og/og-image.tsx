import { ImageResponse } from "next/og";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

export { getPageTitle } from "@/lib/page-titles";

let fontCache: { geistRegular: Buffer } | null = null;

async function loadFonts() {
  if (fontCache) return fontCache;
  const geistRegular = await readFile(
    join(process.cwd(), "public/Geist-Regular.ttf"),
  );
  fontCache = { geistRegular };
  return fontCache;
}

export async function renderOgImage(title: string) {
  const { geistRegular } = await loadFonts();

  return new ImageResponse(
    <div
      style={{
        width: "100%",
        height: "100%",
        display: "flex",
        flexDirection: "column",
        backgroundColor: "black",
        padding: "60px 80px",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: "16px",
        }}
      >
        <svg width="36" height="36" viewBox="0 0 16 16" fill="white">
          <path fillRule="evenodd" clipRule="evenodd" d="M8 1L16 15H0L8 1Z" />
        </svg>
        <span
          style={{
            fontSize: 36,
            color: "#666",
            fontFamily: "Geist",
            fontWeight: 400,
          }}
        >
          /
        </span>
        <span
          style={{
            fontSize: 36,
            fontFamily: "Geist",
            fontWeight: 400,
            color: "white",
          }}
        >
          agent-browser
        </span>
      </div>

      <div
        style={{
          display: "flex",
          flex: 1,
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {title.split("\n").map((line, i) => (
          <span
            key={i}
            style={{
              fontSize: 72,
              fontFamily: "Geist",
              fontWeight: 400,
              color: "white",
              letterSpacing: "-0.02em",
              textAlign: "center",
              lineHeight: 1.2,
            }}
          >
            {line}
          </span>
        ))}
      </div>
    </div>,
    {
      width: 1200,
      height: 630,
      fonts: [
        {
          name: "Geist",
          data: geistRegular.buffer as ArrayBuffer,
          style: "normal",
          weight: 400,
        },
      ],
    },
  );
}
