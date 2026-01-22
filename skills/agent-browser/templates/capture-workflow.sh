#!/bin/bash
# Template: Content Capture Workflow
# Extract content from web pages with optional authentication

set -euo pipefail

TARGET_URL="${1:?Usage: $0 <url> [output-dir]}"
OUTPUT_DIR="${2:-.}"

echo "Capturing content from: $TARGET_URL"
mkdir -p "$OUTPUT_DIR"

# Optional: Load authentication state if needed
# if [[ -f "./auth-state.json" ]]; then
#     agent-browser state load "./auth-state.json"
# fi

# Navigate to target page
agent-browser open "$TARGET_URL"
agent-browser wait --load networkidle

# Get page metadata
echo "Page title: $(agent-browser get title)"
echo "Page URL: $(agent-browser get url)"

# Capture full page screenshot
agent-browser screenshot --full "$OUTPUT_DIR/page-full.png"
echo "Screenshot saved: $OUTPUT_DIR/page-full.png"

# Get page structure
agent-browser snapshot -i > "$OUTPUT_DIR/page-structure.txt"
echo "Structure saved: $OUTPUT_DIR/page-structure.txt"

# Extract main content
# Adjust selector based on target site structure
# agent-browser get text @e1 > "$OUTPUT_DIR/main-content.txt"

# Extract specific elements (uncomment as needed)
# agent-browser get text "article" > "$OUTPUT_DIR/article.txt"
# agent-browser get text "main" > "$OUTPUT_DIR/main.txt"
# agent-browser get text ".content" > "$OUTPUT_DIR/content.txt"

# Get full page text
agent-browser get text body > "$OUTPUT_DIR/page-text.txt"
echo "Text content saved: $OUTPUT_DIR/page-text.txt"

# Optional: Save as PDF
agent-browser pdf "$OUTPUT_DIR/page.pdf"
echo "PDF saved: $OUTPUT_DIR/page.pdf"

# Optional: Capture with scrolling for infinite scroll pages
# scroll_and_capture() {
#     local count=0
#     while [[ $count -lt 5 ]]; do
#         agent-browser scroll down 1000
#         agent-browser wait 1000
#         ((count++))
#     done
#     agent-browser screenshot --full "$OUTPUT_DIR/page-scrolled.png"
# }
# scroll_and_capture

# Cleanup
agent-browser close

echo ""
echo "Capture complete! Files saved to: $OUTPUT_DIR"
ls -la "$OUTPUT_DIR"
