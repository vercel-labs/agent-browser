"use client";

import type { EveDynamicToolPart } from "eve/react";
import { Shimmer } from "@/components/ai-elements/shimmer";
import {
  AppWindow,
  ArrowDownUp,
  ArrowLeft,
  ArrowRight,
  BookOpen,
  Camera,
  ClipboardList,
  Code2,
  Globe,
  Hourglass,
  Info,
  Keyboard,
  ListChecks,
  MousePointer,
  MousePointerClick,
  Move,
  Network,
  PowerOff,
  RotateCw,
  ScanSearch,
  Search,
  SquareCheck,
  Terminal,
  TextCursorInput,
  Upload,
  Wrench,
  type LucideIcon,
} from "lucide-react";

/**
 * Friendly, end-user-facing rendering for tool calls: an icon and a
 * plain-language activity line instead of tool names and JSON. The
 * `browser__*` tools from @agent-browser/eve get specific icons and labels
 * built from their inputs; other tools get a humanized generic line.
 */
export function ToolActivity({ part }: { readonly part: EveDynamicToolPart }) {
  const running =
    part.state !== "output-available" &&
    part.state !== "output-error" &&
    part.state !== "output-denied";
  const { icon: Icon, label } = describeActivity(part, running);
  const screenshot = screenshotFrom(part);
  const liveViewUrl = liveViewFrom(part);

  // Failures render exactly like completed steps: the agent handles its own
  // errors and end users don't need to see them.
  return (
    <div className="@container not-prose mb-1.5 py-0.5 text-sm">
      <div className="flex items-center gap-2.5">
      <Icon className="size-4 shrink-0 text-muted-foreground" />
      {running ? (
        <Shimmer as="span" className="text-sm">
          {label}
        </Shimmer>
      ) : (
        <span className="text-muted-foreground">{label}</span>
      )}
      </div>
      {liveViewUrl ? (
        <div className="mt-2 overflow-hidden rounded-lg border bg-black">
          <iframe
            allow="clipboard-read; clipboard-write"
            className="h-[clamp(16rem,40vh,28rem)] w-full"
            sandbox="allow-same-origin allow-scripts"
            src={liveViewUrl}
            title="Interactive Browserbase live view"
          />
        </div>
      ) : null}
      {screenshot ? (
        // Cap the viewer at 16:9 of the chat width (56.25cqw); shorter images
        // keep their natural height, taller ones scroll vertically.
        <div className="mt-2 max-h-[56.25cqw] w-full overflow-y-auto rounded-lg border">
          {/* eslint-disable-next-line @next/next/no-img-element -- data URL, not an optimizable asset */}
          <img
            alt="Screenshot taken by the browser agent"
            className="h-auto w-full"
            src={screenshot}
          />
        </div>
      ) : null}
    </div>
  );
}

function screenshotFrom(part: EveDynamicToolPart): string | undefined {
  if (part.state !== "output-available" || typeof part.output !== "object" || part.output === null) {
    return undefined;
  }
  const { imageDataUrl } = part.output as { imageDataUrl?: unknown };
  return typeof imageDataUrl === "string" && imageDataUrl.startsWith("data:image/")
    ? imageDataUrl
    : undefined;
}

/** Browserbase live-view URL from navigation channel output (stripped from model output). */
function liveViewFrom(part: EveDynamicToolPart): string | undefined {
  if (part.state !== "output-available" || typeof part.output !== "object" || part.output === null) {
    return undefined;
  }
  if (part.toolName !== "browser__navigate") {
    return undefined;
  }
  const output = part.output as {
    provider?: unknown;
    providerMetadata?: { debuggerFullscreenUrl?: unknown };
  };
  if (output.provider !== "browserbase") {
    return undefined;
  }
  const url = output.providerMetadata?.debuggerFullscreenUrl;
  return typeof url === "string" && url.startsWith("https://") ? url : undefined;
}

function describeActivity(
  part: EveDynamicToolPart,
  running: boolean,
): { icon: LucideIcon; label: string } {
  const input = (part.input ?? {}) as Record<string, unknown>;
  const pick = (ing: string, ed: string) => (running ? `${ing}…` : ed);

  if (!part.toolName.startsWith("browser__")) {
    if (part.toolName === "todo") {
      return { icon: ClipboardList, label: pick("Planning next steps", "Planned next steps") };
    }
    const humanized = humanizeToolName(part.toolName);
    return { icon: Wrench, label: running ? `${humanized}…` : humanized };
  }

  const tool = part.toolName.replace(/^browser__/, "");
  switch (tool) {
    case "navigate": {
      const action = typeof input.action === "string" ? input.action : "goto";
      if (action === "back") return { icon: ArrowLeft, label: pick("Going back", "Went back") };
      if (action === "forward") {
        return { icon: ArrowRight, label: pick("Going forward", "Went forward") };
      }
      if (action === "reload") {
        return { icon: RotateCw, label: pick("Reloading the page", "Reloaded the page") };
      }
      const site = hostOf(input.url);
      return {
        icon: Globe,
        label: site ? pick(`Opening ${site}`, `Opened ${site}`) : pick("Opening a page", "Opened a page"),
      };
    }
    case "snapshot":
      return { icon: ScanSearch, label: pick("Looking at the page", "Looked at the page") };
    case "read": {
      const site = hostOf(input.url);
      return {
        icon: BookOpen,
        label: site ? pick(`Reading ${site}`, `Read ${site}`) : pick("Reading the page", "Read the page"),
      };
    }
    case "click":
      return { icon: MousePointerClick, label: pick("Clicking", "Clicked") };
    case "fill":
      return { icon: TextCursorInput, label: pick("Filling in a field", "Filled in a field") };
    case "press_key": {
      const key = typeof input.key === "string" ? input.key : undefined;
      return {
        icon: Keyboard,
        label: key ? pick(`Pressing ${key}`, `Pressed ${key}`) : pick("Pressing a key", "Pressed a key"),
      };
    }
    case "hover":
      return { icon: MousePointer, label: pick("Hovering over an element", "Hovered over an element") };
    case "select_option":
      return { icon: ListChecks, label: pick("Picking an option", "Picked an option") };
    case "set_checked":
      return {
        icon: SquareCheck,
        label: input.checked === false ? pick("Unchecking a box", "Unchecked a box") : pick("Checking a box", "Checked a box"),
      };
    case "scroll":
      return {
        icon: ArrowDownUp,
        label:
          input.direction === undefined
            ? pick("Scrolling to an element", "Scrolled to an element")
            : pick("Scrolling the page", "Scrolled the page"),
      };
    case "drag":
      return { icon: Move, label: pick("Dragging an item", "Dragged an item") };
    case "upload": {
      const count = Array.isArray(input.files) ? input.files.length : 0;
      const noun = count > 1 ? `${count} files` : "a file";
      return { icon: Upload, label: pick(`Attaching ${noun}`, `Attached ${noun}`) };
    }
    case "wait_for":
      return { icon: Hourglass, label: pick("Waiting for the page", "Waited for the page") };
    case "get":
      return { icon: Info, label: pick("Reading page details", "Read page details") };
    case "find": {
      const query = typeof input.query === "string" ? truncate(input.query, 32) : undefined;
      const verbs: Record<string, [string, string]> = {
        check: ["Checking", "Checked"],
        click: ["Clicking", "Clicked"],
        fill: ["Filling in", "Filled in"],
        focus: ["Focusing", "Focused"],
        hover: ["Hovering over", "Hovered over"],
        text: ["Reading", "Read"],
        type: ["Typing into", "Typed into"],
        uncheck: ["Unchecking", "Unchecked"],
      };
      const [ing, ed] =
        typeof input.action === "string" && verbs[input.action]
          ? verbs[input.action]
          : ["Finding", "Found"];
      return {
        icon: Search,
        label: query ? pick(`${ing} “${query}”`, `${ed} “${query}”`) : pick("Finding an element", "Found an element"),
      };
    }
    case "screenshot":
      return { icon: Camera, label: pick("Taking a screenshot", "Took a screenshot") };
    case "evaluate":
      return { icon: Code2, label: pick("Running a page script", "Ran a page script") };
    case "tabs": {
      const labels: Record<string, [string, string]> = {
        close: ["Closing a tab", "Closed a tab"],
        list: ["Checking open tabs", "Checked open tabs"],
        new: ["Opening a new tab", "Opened a new tab"],
        switch: ["Switching tabs", "Switched tabs"],
      };
      const [ing, ed] =
        typeof input.action === "string" && labels[input.action]
          ? labels[input.action]
          : labels.list;
      return { icon: AppWindow, label: pick(ing, ed) };
    }
    case "console":
      return { icon: Terminal, label: pick("Checking for page errors", "Checked for page errors") };
    case "network_requests":
      return { icon: Network, label: pick("Inspecting network activity", "Inspected network activity") };
    case "close":
      return { icon: PowerOff, label: pick("Closing the browser", "Closed the browser") };
    default:
      return { icon: Globe, label: pick("Using the browser", "Used the browser") };
  }
}

function humanizeToolName(toolName: string): string {
  const bare = toolName.split("__").at(-1) ?? toolName;
  const words = bare.replaceAll(/[_-]+/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

function hostOf(url: unknown): string | undefined {
  if (typeof url !== "string" || url.length === 0) {
    return undefined;
  }
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return truncate(url, 40);
  }
}

function truncate(value: string, max: number): string {
  return value.length > max ? `${value.slice(0, max - 1)}…` : value;
}
