import type { EvalResult, EvalSummary, Category } from "./types.ts";

const PASS = "\x1b[32m\u2713\x1b[0m";
const FAIL = "\x1b[31m\u2717\x1b[0m";
const ERR = "\x1b[33m!\x1b[0m";
const DIM = "\x1b[2m";
const RESET = "\x1b[0m";
const BOLD = "\x1b[1m";

function padRight(str: string, len: number): string {
  return str + " ".repeat(Math.max(0, len - str.length));
}

export function printResult(result: EvalResult): void {
  const icon = result.error ? ERR : result.pass ? PASS : FAIL;
  const status = result.error ? "ERROR" : result.pass ? "PASS" : "FAIL";
  const duration = `${DIM}${result.durationMs}ms${RESET}`;

  console.log(`  ${icon} ${padRight(result.caseName, 50)} ${status}  ${duration}`);

  if (result.error) {
    console.log(`    ${DIM}Error: ${result.error}${RESET}`);
    return;
  }

  const failedExpected = result.patternResults.filter(
    (p) => p.type === "expected" && !p.matched,
  );
  const matchedForbidden = result.patternResults.filter(
    (p) => p.type === "forbidden" && p.matched,
  );

  for (const p of failedExpected) {
    console.log(`    ${FAIL} Expected pattern not found: ${DIM}${p.pattern}${RESET}`);
  }
  for (const p of matchedForbidden) {
    console.log(`    ${FAIL} Forbidden pattern matched: ${DIM}${p.pattern}${RESET}`);
  }

  if (result.judge) {
    console.log(
      `    ${DIM}Judge: ${result.judge.score}/5 - ${result.judge.reasoning}${RESET}`,
    );
  }
}

export function printCategoryHeader(category: string): void {
  console.log(`\n${BOLD}${category}${RESET}`);
  console.log(`${"─".repeat(70)}`);
}

export function computeSummary(
  results: EvalResult[],
  totalDurationMs: number,
): EvalSummary {
  const byCategory: Record<Category, { total: number; passed: number }> = {
    "skill-loading": { total: 0, passed: 0 },
    "skill-selection": { total: 0, passed: 0 },
    "command-usage": { total: 0, passed: 0 },
  };

  let passed = 0;
  let failed = 0;
  let errors = 0;

  for (const r of results) {
    byCategory[r.category].total++;
    if (r.error) {
      errors++;
    } else if (r.pass) {
      passed++;
      byCategory[r.category].passed++;
    } else {
      failed++;
    }
  }

  return {
    total: results.length,
    passed,
    failed,
    errors,
    byCategory,
    durationMs: totalDurationMs,
  };
}

export function printSummary(summary: EvalSummary): void {
  console.log(`\n${BOLD}Summary${RESET}`);
  console.log(`${"═".repeat(70)}`);

  for (const [cat, stats] of Object.entries(summary.byCategory)) {
    if (stats.total === 0) continue;
    const pct = Math.round((stats.passed / stats.total) * 100);
    const bar = stats.passed === stats.total ? PASS : FAIL;
    console.log(`  ${bar} ${padRight(cat, 20)} ${stats.passed}/${stats.total} (${pct}%)`);
  }

  console.log(`${"─".repeat(70)}`);
  const totalPct = summary.total > 0
    ? Math.round((summary.passed / summary.total) * 100)
    : 0;
  const icon = summary.failed === 0 && summary.errors === 0 ? PASS : FAIL;
  console.log(
    `  ${icon} ${BOLD}Total: ${summary.passed}/${summary.total} passed (${totalPct}%)${RESET}`,
  );
  if (summary.errors > 0) {
    console.log(`  ${ERR} ${summary.errors} error(s)`);
  }
  console.log(`  ${DIM}Duration: ${(summary.durationMs / 1000).toFixed(1)}s${RESET}\n`);
}

export function printResultsJson(
  results: EvalResult[],
  summary: EvalSummary,
): void {
  const output = {
    summary: {
      total: summary.total,
      passed: summary.passed,
      failed: summary.failed,
      errors: summary.errors,
      passRate: summary.total > 0
        ? Math.round((summary.passed / summary.total) * 100)
        : 0,
      durationMs: summary.durationMs,
      byCategory: summary.byCategory,
    },
    results: results.map((r) => ({
      id: r.caseId,
      name: r.caseName,
      category: r.category,
      pass: r.pass,
      durationMs: r.durationMs,
      error: r.error,
      patterns: r.patternResults,
      judge: r.judge,
      response: r.response,
    })),
  };
  console.log(JSON.stringify(output, null, 2));
}
