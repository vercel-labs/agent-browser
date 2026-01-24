#!/bin/bash
# Template: Download Workflow
# Downloads files triggered by clicking elements (PDFs, exports, reports)

set -euo pipefail

URL="${1:?Usage: $0 <page-url> [output-dir]}"
OUTPUT_DIR="${2:-./downloads}"

mkdir -p "$OUTPUT_DIR"

echo "Starting download workflow from: $URL"

# Navigate to page with downloadable content
agent-browser open "$URL"
agent-browser wait --load networkidle

# Get interactive snapshot to identify download triggers
echo "Analyzing page for download links..."
agent-browser snapshot -i

# Example: Download by clicking element
# Uncomment and modify refs based on snapshot output

# Method 1: Download command (click element and save file)
# agent-browser download @e1 "$OUTPUT_DIR/report.pdf"

# Method 2: Wait for download after click
# agent-browser click @e1
# agent-browser wait --download "$OUTPUT_DIR/export.xlsx"

# Method 3: Download with timeout
# agent-browser click @e1
# agent-browser wait --download "$OUTPUT_DIR/large-file.zip" --timeout 60000

# Method 4: Multiple downloads
# for i in 1 2 3; do
#     agent-browser click "@e$i"
#     agent-browser wait --download "$OUTPUT_DIR/file-$i.pdf"
# done

# Example: Export workflow (common pattern)
# 1. Click export button
# agent-browser find role button click --name "Export"
#
# 2. Select format from dropdown/modal
# agent-browser wait 500
# agent-browser snapshot -i
# agent-browser find text "PDF" click
#
# 3. Confirm and download
# agent-browser find role button click --name "Download"
# agent-browser wait --download "$OUTPUT_DIR/export.pdf"

# Verify downloads
echo "Downloads saved to: $OUTPUT_DIR"
ls -la "$OUTPUT_DIR"

# Take screenshot of final state
agent-browser screenshot "$OUTPUT_DIR/download-complete.png"

# Cleanup
agent-browser close

echo "Download workflow complete"
