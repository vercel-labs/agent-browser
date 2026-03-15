import { NextRequest, NextResponse } from "next/server";
import * as sandbox from "@/lib/agent-browser-sandbox";
import type { StepEvent } from "@/lib/agent-browser-sandbox";
import { ALLOWED_URLS } from "@/lib/constants";
import { minuteRateLimit, dailyRateLimit } from "@/lib/rate-limit";

export async function POST(req: NextRequest) {
  const ip =
    req.headers.get("x-forwarded-for")?.split(",")[0] ?? "anonymous";

  const minute = await minuteRateLimit.limit(ip);
  if (!minute.success) {
    return NextResponse.json(
      { error: "Too many requests. Please wait a moment before trying again." },
      { status: 429 },
    );
  }

  const daily = await dailyRateLimit.limit(ip);
  if (!daily.success) {
    return NextResponse.json(
      { error: "Daily limit reached. Please try again tomorrow." },
      { status: 429 },
    );
  }

  const body = await req.json();
  const { url, action } = body;

  if (!url) {
    return NextResponse.json({ error: "Provide a 'url'" }, { status: 400 });
  }

  if (!(ALLOWED_URLS as readonly string[]).includes(url)) {
    return NextResponse.json({ error: "URL not allowed" }, { status: 400 });
  }

  if (action !== "screenshot" && action !== "snapshot") {
    return NextResponse.json(
      { error: "Provide 'action' as 'screenshot' or 'snapshot'" },
      { status: 400 },
    );
  }

  const encoder = new TextEncoder();

  const stream = new ReadableStream({
    async start(controller) {
      const send = (event: string, data: unknown) => {
        controller.enqueue(
          encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`),
        );
      };

      const onStep = (step: StepEvent) => {
        send("step", step);
      };

      try {
        if (action === "screenshot") {
          const result = await sandbox.screenshotUrl(url, {
            fullPage: body.fullPage,
            onStep,
          });
          send("result", { ok: true, ...result });
        } else {
          const result = await sandbox.snapshotUrl(url, {
            interactive: true,
            compact: true,
            onStep,
          });
          send("result", { ok: true, ...result });
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        send("result", { ok: false, error: message });
      }

      controller.close();
    },
  });

  return new Response(stream, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    },
  });
}
