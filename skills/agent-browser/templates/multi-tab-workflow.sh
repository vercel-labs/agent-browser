#!/bin/bash
# Template: Multi-Tab Workflow
# Work with multiple tabs for comparison testing, parallel operations, and complex flows

set -euo pipefail

URL1="${1:?Usage: $0 <url1> [url2] [output-dir]}"
URL2="${2:-$URL1}"
OUTPUT_DIR="${3:-./multi-tab-output}"

mkdir -p "$OUTPUT_DIR"

echo "Starting multi-tab workflow"

# ============================================
# Basic Multi-Tab Operations
# ============================================

# Open first tab
agent-browser open "$URL1"
agent-browser wait --load networkidle
echo "Tab 1 opened: $URL1"

# List current tabs
agent-browser tab

# Open second tab
agent-browser tab new "$URL2"
agent-browser wait --load networkidle
echo "Tab 2 opened: $URL2"

# List tabs again
agent-browser tab

# ============================================
# Example 1: Comparison Screenshots
# ============================================
echo "Taking comparison screenshots..."

# Screenshot tab 2 (current tab)
agent-browser screenshot "$OUTPUT_DIR/tab2.png"

# Switch to tab 1 (index 0)
agent-browser tab 0
agent-browser screenshot "$OUTPUT_DIR/tab1.png"

# ============================================
# Example 2: Cross-Tab Data Transfer
# ============================================
echo "Extracting data across tabs..."

# Get data from tab 1
agent-browser tab 0
agent-browser snapshot -i
# TEXT1=$(agent-browser get text @e1)

# Switch to tab 2 and use the data
agent-browser tab 1
agent-browser snapshot -i
# agent-browser fill @e1 "$TEXT1"

# ============================================
# Example 3: A/B Comparison Testing
# ============================================
echo "A/B comparison example..."

# Tab 1: Version A
agent-browser tab 0
# agent-browser open "$URL1?variant=a"
agent-browser snapshot -i
agent-browser screenshot "$OUTPUT_DIR/variant-a.png"

# Tab 2: Version B
agent-browser tab 1
# agent-browser open "$URL1?variant=b"
agent-browser snapshot -i
agent-browser screenshot "$OUTPUT_DIR/variant-b.png"

# ============================================
# Example 4: Parallel Form Filling
# ============================================
echo "Parallel operations example..."

# Fill form in tab 1
agent-browser tab 0
agent-browser snapshot -i
# agent-browser fill @e1 "Form 1 Data"
# agent-browser click @e2

# Fill form in tab 2
agent-browser tab 1
agent-browser snapshot -i
# agent-browser fill @e1 "Form 2 Data"
# agent-browser click @e2

# ============================================
# Example 5: Open Multiple Tabs Programmatically
# ============================================
echo "Opening multiple tabs..."

URLS=(
    "https://example.com/page1"
    "https://example.com/page2"
    "https://example.com/page3"
)

# Uncomment to use:
# for url in "${URLS[@]}"; do
#     agent-browser tab new "$url"
#     agent-browser wait --load networkidle
# done

# Process each tab
# for i in "${!URLS[@]}"; do
#     agent-browser tab "$i"
#     agent-browser screenshot "$OUTPUT_DIR/page-$i.png"
# done

# ============================================
# Tab Management
# ============================================
echo "Managing tabs..."

# List all tabs
agent-browser tab

# Close specific tab by index
# agent-browser tab close 2

# Close current tab
# agent-browser tab close

# ============================================
# Cleanup
# ============================================
echo "Cleaning up..."

# Close all tabs (closes browser)
agent-browser close

echo "Multi-tab workflow complete"
echo "Output saved to: $OUTPUT_DIR"
ls -la "$OUTPUT_DIR"
