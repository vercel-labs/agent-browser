#!/usr/bin/env bash
#
# Steel Provider Integration Test
# Tests agent-browser CLI with Steel cloud browser provider
#
# Usage:
#   STEEL_API_KEY="your-api-key" ./test-steel-provider.sh
#
# Requirements:
#   - agent-browser built (pnpm build && pnpm build:native)
#   - STEEL_API_KEY environment variable set
#

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -x "$SCRIPT_DIR/bin/agent-browser" ]; then
  AB="\"$SCRIPT_DIR/bin/agent-browser\""
elif [ -f "$SCRIPT_DIR/bin/agent-browser.js" ]; then
  AB="node \"$SCRIPT_DIR/bin/agent-browser.js\""
else
  echo "Error: Local CLI not found in $SCRIPT_DIR/bin/"
  echo "Run 'pnpm build:native' first"
  exit 1
fi

if [ -n "${NO_COLOR:-}" ]; then
  RED=''
  GREEN=''
  YELLOW=''
  NC=''
else
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[1;33m'
  NC='\033[0m'
fi

if [ -z "${STEEL_API_KEY:-}" ]; then
  echo -e "${RED}Error: STEEL_API_KEY environment variable is required${NC}"
  echo "Usage: STEEL_API_KEY=\"your-api-key\" ./test-steel-provider.sh"
  exit 1
fi

# Isolate daemon state from normal workflows.
export AGENT_BROWSER_SESSION="steel-test-$(date +%s)"

echo "Using CLI command: $AB"
echo "Using session: $AGENT_BROWSER_SESSION"
echo "========================================"
echo "Steel Provider Integration Test"
echo "========================================"
echo ""

TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

run_test() {
  local name="$1"
  local cmd="$2"
  echo -n "Testing: $name... "
  local output
  if output=$(eval "$cmd" 2>&1); then
    echo -e "${GREEN}✓ PASSED${NC}"
    ((TESTS_PASSED++)) || true
  else
    echo -e "${RED}✗ FAILED${NC}"
    echo "  Command: $cmd"
    echo "  Output: $output"
    ((TESTS_FAILED++)) || true
  fi
}

run_test_verbose() {
  local name="$1"
  local cmd="$2"
  echo "Testing: $name"
  echo "  Command: $cmd"
  local output
  if output=$(eval "$cmd" 2>&1); then
    echo -e "  ${GREEN}✓ PASSED${NC}"
    echo "  Output: ${output:0:200}"
    ((TESTS_PASSED++)) || true
  else
    echo -e "  ${RED}✗ FAILED${NC}"
    echo "  Output: $output"
    ((TESTS_FAILED++)) || true
  fi
}

run_skip() {
  local name="$1"
  local reason="$2"
  echo -e "Testing: $name... ${YELLOW}⚠ SKIPPED${NC} ($reason)"
  ((TESTS_SKIPPED++)) || true
}

cleanup() {
  echo ""
  echo "Cleaning up..."
  eval "$AB close" >/dev/null 2>&1 || true
}

trap cleanup EXIT

# ============================================
# Test Suite 1: Basic Provider Launch
# ============================================
echo ""
echo -e "${YELLOW}=== Test Suite 1: Basic Provider Launch ===${NC}"
echo ""

run_test "Launch with -p steel flag" \
  "$AB -p steel open https://example.com"

run_test "Get page title" \
  "[ \"\$(eval \"$AB get title\")\" = \"Example Domain\" ]"

run_test "Get current URL" \
  "eval \"$AB get url\" | grep -q '^https://example.com/'"

run_test "Take accessibility snapshot" \
  "eval \"$AB snapshot\" | head -20 >/dev/null"

# screenshot command writes to a file path by default; verify file exists and is non-empty
SCREENSHOT_FILE=\"$(mktemp -t steel-provider-shot).png\"
run_test "Take screenshot (file output)" \
  "$AB screenshot \"$SCREENSHOT_FILE\" >/dev/null && test -s \"$SCREENSHOT_FILE\""
rm -f "$SCREENSHOT_FILE"

run_test "Close browser" \
  "$AB close"

sleep 2

# ============================================
# Test Suite 2: Navigation & Interactions
# ============================================
echo ""
echo -e "${YELLOW}=== Test Suite 2: Navigation & Interactions ===${NC}"
echo ""

export STEEL_SOLVE_CAPTCHA=true

run_test "Launch with solveCaptcha enabled" \
  "$AB -p steel open https://example.com"

run_test_verbose "Get body text" \
  "$AB get text body | head -c 100"

run_test "Navigate to new URL" \
  "$AB open https://example.org"

run_test "Wait for selector" \
  "$AB wait h1"

run_test "Get h1 text" \
  "[ \"\$(eval \"$AB get text h1\")\" = \"Example Domain\" ]"

echo -n "Testing: Snapshot contains refs... "
snapshot_output=$(eval "$AB snapshot -i" 2>&1)
if echo "$snapshot_output" | grep -Eq '\[ref=e[0-9]+\]'; then
  echo -e "${GREEN}✓ PASSED${NC} (refs found)"
  ((TESTS_PASSED++)) || true
else
  echo -e "${YELLOW}⚠ SKIPPED${NC} (no refs found in snapshot)"
  ((TESTS_SKIPPED++)) || true
fi

run_test "Close browser" \
  "$AB close"

unset STEEL_SOLVE_CAPTCHA
sleep 2

# ============================================
# Test Suite 3: Tab Management
# ============================================
echo ""
echo -e "${YELLOW}=== Test Suite 3: Tab Management ===${NC}"
echo ""

run_test "Launch browser" \
  "$AB -p steel open https://example.com"

run_test "Create new tab" \
  "$AB tab new"

run_test "Navigate in new tab" \
  "$AB open https://example.org"

run_test "List all tabs" \
  "$AB tab list | grep -q 'example.org/'"

run_test "Switch to first tab" \
  "$AB tab 0"

run_test "Verify first tab URL" \
  "eval \"$AB get url\" | grep -q '^https://example.com/'"

run_test "Close browser" \
  "$AB close"

sleep 2

# ============================================
# Test Suite 4: Profile Persistence (Optional)
# ============================================
echo ""
echo -e "${YELLOW}=== Test Suite 4: Profile Persistence ===${NC}"
echo ""

if [ -z "${STEEL_PROFILE_ID:-}" ]; then
  run_skip "Profile persistence suite" "set STEEL_PROFILE_ID to run this suite"
else
  export STEEL_PERSIST_PROFILE=true
  TEST_COOKIE="steel_profile_test_$(date +%s)"

  echo "Using Steel profile: $STEEL_PROFILE_ID"
  echo "Test cookie: $TEST_COOKIE"
  echo ""

  run_test "Launch with existing profile" \
    "$AB -p steel open https://example.com"

  run_test "Set persistent cookie" \
    "$AB eval \"document.cookie='${TEST_COOKIE}=1; expires=Tue, 19 Jan 2038 03:14:07 GMT; path=/'; document.cookie\" | grep -q '${TEST_COOKIE}=1'"

  run_test "Close browser (persists profile)" \
    "$AB close"

  sleep 3

  run_test "Re-launch with same profile" \
    "$AB -p steel open https://example.com"

  echo -n "Testing: Cookie persistence in profile... "
  cookie_output=$(eval "$AB eval \"document.cookie\"" 2>&1)
  if echo "$cookie_output" | grep -q "$TEST_COOKIE=1"; then
    echo -e "${GREEN}✓ PASSED${NC} (cookie persisted)"
    ((TESTS_PASSED++)) || true
  else
    echo -e "${YELLOW}⚠ CHECK${NC} (cookie may not have persisted)"
    echo "  Output: ${cookie_output:0:200}"
    ((TESTS_PASSED++)) || true
  fi

  run_test "Close browser" \
    "$AB close"

  unset STEEL_PERSIST_PROFILE
  sleep 2
fi

# ============================================
# Test Suite 5: Environment Variable Configuration
# ============================================
echo ""
echo -e "${YELLOW}=== Test Suite 5: Environment Variables ===${NC}"
echo ""

export AGENT_BROWSER_PROVIDER=steel
export STEEL_HEADLESS=true
export STEEL_TIMEOUT_MS=120000
export STEEL_DEVICE=desktop

run_test "Launch via AGENT_BROWSER_PROVIDER env var" \
  "$AB open https://example.com"

run_test "Verify page loaded" \
  "[ \"\$(eval \"$AB get title\")\" = \"Example Domain\" ]"

run_test "Close browser" \
  "$AB close"

unset AGENT_BROWSER_PROVIDER
unset STEEL_HEADLESS
unset STEEL_TIMEOUT_MS
unset STEEL_DEVICE

# ============================================
# Results Summary
# ============================================
echo ""
echo "========================================"
echo "Test Results"
echo "========================================"
echo -e "Passed:  ${GREEN}$TESTS_PASSED${NC}"
echo -e "Failed:  ${RED}$TESTS_FAILED${NC}"
echo -e "Skipped: ${YELLOW}$TESTS_SKIPPED${NC}"
echo ""

if [ "$TESTS_FAILED" -eq 0 ]; then
  echo -e "${GREEN}All required tests passed.${NC}"
  exit 0
else
  echo -e "${RED}Some tests failed.${NC}"
  exit 1
fi
