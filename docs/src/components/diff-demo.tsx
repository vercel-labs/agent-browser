"use client";

function DiffLine({ line }: { line: string }) {
  if (line.startsWith("+ ")) {
    return <div className="text-green-400">{line}</div>;
  }
  if (line.startsWith("- ")) {
    return <div className="text-red-400">{line}</div>;
  }
  return <div className="opacity-50">{line}</div>;
}

function CommandLine({ children }: { children: string }) {
  return (
    <div>
      <span className="opacity-40">$ </span>
      {children}
    </div>
  );
}

function Terminal({ children }: { children: React.ReactNode }) {
  return (
    <div
      className="rounded border font-mono text-[0.8125rem] leading-[1.7] overflow-x-auto"
      style={{
        background: "var(--card)",
        borderColor: "var(--border)",
        padding: "0.875rem",
      }}
    >
      {children}
    </div>
  );
}

function PageMockup({
  label,
  buttonColor,
  diffMode,
}: {
  label: string;
  buttonColor: string;
  diffMode?: boolean;
}) {
  const dimOpacity = diffMode ? 0.15 : 1;
  return (
    <div className="flex-1 min-w-0">
      <div
        className="text-[0.6875rem] font-medium mb-1.5 text-center"
        style={{ color: "var(--muted-foreground)" }}
      >
        {label}
      </div>
      <svg
        viewBox="0 0 160 120"
        className="w-full rounded border"
        style={{ borderColor: "var(--border)" }}
      >
        <rect width="160" height="120" fill={diffMode ? "#1a1a1a" : "#111"} />

        {/* Nav bar */}
        <rect
          x="0"
          y="0"
          width="160"
          height="16"
          fill="#222"
          opacity={dimOpacity}
        />
        <rect
          x="8"
          y="5"
          width="24"
          height="6"
          rx="1"
          fill="#555"
          opacity={dimOpacity}
        />
        <rect
          x="120"
          y="5"
          width="12"
          height="6"
          rx="1"
          fill="#444"
          opacity={dimOpacity}
        />
        <rect
          x="136"
          y="5"
          width="12"
          height="6"
          rx="1"
          fill="#444"
          opacity={dimOpacity}
        />

        {/* Heading */}
        <rect
          x="20"
          y="26"
          width="80"
          height="6"
          rx="1"
          fill="#666"
          opacity={dimOpacity}
        />

        {/* Subtext */}
        <rect
          x="30"
          y="38"
          width="60"
          height="4"
          rx="1"
          fill="#444"
          opacity={dimOpacity}
        />

        {/* Input field */}
        <rect
          x="30"
          y="52"
          width="100"
          height="14"
          rx="2"
          fill="#1a1a1a"
          stroke="#333"
          strokeWidth="0.5"
          opacity={dimOpacity}
        />

        {/* Button -- this is what changes */}
        {diffMode ? (
          <>
            <rect
              x="55"
              y="76"
              width="50"
              height="14"
              rx="2"
              fill="#ef4444"
              opacity="0.85"
            />
            <rect
              x="55"
              y="76"
              width="50"
              height="14"
              rx="2"
              fill="none"
              stroke="#ef4444"
              strokeWidth="1.5"
              strokeDasharray="3 2"
            />
          </>
        ) : (
          <rect
            x="55"
            y="76"
            width="50"
            height="14"
            rx="2"
            fill={buttonColor}
          />
        )}
        <text
          x="80"
          y="85.5"
          textAnchor="middle"
          fill="white"
          fontSize="6"
          fontFamily="system-ui, sans-serif"
          opacity={diffMode ? 0.9 : 1}
        >
          Submit
        </text>

        {/* Footer line */}
        <rect
          x="40"
          y="102"
          width="80"
          height="3"
          rx="1"
          fill="#333"
          opacity={dimOpacity}
        />
      </svg>
    </div>
  );
}

const snapshotDiffLines = [
  "  heading \"Sign Up\" [ref=e1]",
  "  text \"Create your account\" [ref=e2]",
  "- textbox \"Email\" [ref=e3]",
  "+ textbox \"Email\" [ref=e3]: \"test@example.com\"",
  "- button \"Submit\" [ref=e4]",
  "+ button \"Submit\" [ref=e4] [disabled]",
  "+ status \"Sending...\" [ref=e7]",
  "  link \"Already have an account?\" [ref=e5]",
];

export function DiffDemo() {
  return (
    <div className="grid gap-8 my-8">
      {/* Panel 1: Snapshot diff */}
      <div>
        <div
          className="text-xs font-medium uppercase tracking-wider mb-3"
          style={{ color: "var(--muted-foreground)" }}
        >
          Verify an action changed the page
        </div>
        <Terminal>
          <div className="opacity-60 mb-2">
            <CommandLine>agent-browser snapshot -i</CommandLine>
            <CommandLine>
              agent-browser fill @e3 &quot;test@example.com&quot;
            </CommandLine>
            <CommandLine>agent-browser click @e4</CommandLine>
          </div>
          <div className="mb-3">
            <CommandLine>agent-browser diff snapshot</CommandLine>
          </div>
          <div
            className="border-t pt-3"
            style={{ borderColor: "var(--border)" }}
          >
            {snapshotDiffLines.map((line, i) => (
              <DiffLine key={i} line={line} />
            ))}
            <div className="mt-2 opacity-60">
              <span className="text-green-400">3</span> additions,{" "}
              <span className="text-red-400">2</span> removals,{" "}
              <span>3</span> unchanged
            </div>
          </div>
        </Terminal>
      </div>

      {/* Panel 2: Screenshot diff */}
      <div>
        <div
          className="text-xs font-medium uppercase tracking-wider mb-3"
          style={{ color: "var(--muted-foreground)" }}
        >
          Catch a visual regression
        </div>
        <Terminal>
          <div className="mb-3">
            <CommandLine>
              agent-browser diff screenshot --baseline before-deploy.png
            </CommandLine>
          </div>
          <div
            className="border-t pt-3"
            style={{ borderColor: "var(--border)" }}
          >
            <div className="text-red-400">
              &#x2717; 2.37% pixels differ
            </div>
            <div className="opacity-50">
              Diff image: ~/.agent-browser/tmp/diffs/diff-1708473621.png
            </div>
            <div className="opacity-50">
              <span className="text-red-400">1,137</span> different /{" "}
              48,000 total pixels
            </div>
          </div>
        </Terminal>
        <div className="flex gap-2 mt-3">
          <PageMockup label="Baseline" buttonColor="#3b82f6" />
          <PageMockup label="Current" buttonColor="#22c55e" />
          <PageMockup label="Diff" buttonColor="#ef4444" diffMode />
        </div>
      </div>
    </div>
  );
}
