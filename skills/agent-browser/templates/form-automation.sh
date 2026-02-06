#!/bin/bash
# Template: Form Automation Workflow
# Fills and submits web forms with validation

set -euo pipefail

FORM_URL="${1:?Usage: $0 <form-url>}"

echo "Automating form at: $FORM_URL"

# Navigate to form page
npx agent-browser open "$FORM_URL"
npx agent-browser wait --load networkidle

# Get interactive snapshot to identify form fields
echo "Analyzing form structure..."
npx agent-browser snapshot -i

# Example: Fill common form fields
# Uncomment and modify refs based on snapshot output

# Text inputs
# npx agent-browser fill @e1 "John Doe"           # Name field
# npx agent-browser fill @e2 "user@example.com"   # Email field
# npx agent-browser fill @e3 "+1-555-123-4567"    # Phone field

# Password fields
# npx agent-browser fill @e4 "SecureP@ssw0rd!"

# Dropdowns
# npx agent-browser select @e5 "Option Value"

# Checkboxes
# npx agent-browser check @e6                      # Check
# npx agent-browser uncheck @e7                    # Uncheck

# Radio buttons
# npx agent-browser click @e8                      # Select radio option

# Text areas
# npx agent-browser fill @e9 "Multi-line text content here"

# File uploads
# npx agent-browser upload @e10 /path/to/file.pdf

# Submit form
# npx agent-browser click @e11                     # Submit button

# Wait for response
# npx agent-browser wait --load networkidle
# npx agent-browser wait --url "**/success"        # Or wait for redirect

# Verify submission
echo "Form submission result:"
npx agent-browser get url
npx agent-browser snapshot -i

# Take screenshot of result
npx agent-browser screenshot /tmp/form-result.png

# Cleanup
npx agent-browser close

echo "Form automation complete"
