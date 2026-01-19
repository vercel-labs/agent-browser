#!/bin/bash
# Template: Form Automation Workflow
# Fills and submits web forms with validation

set -euo pipefail

FORM_URL="${1:?Usage: $0 <form-url>}"

echo "Automating form at: $FORM_URL"

# Navigate to form page
agent-browser open "$FORM_URL"
agent-browser wait --load networkidle

# Get interactive snapshot to identify form fields
echo "Analyzing form structure..."
agent-browser snapshot -i

# Example: Fill common form fields
# Uncomment and modify refs based on snapshot output

# Text inputs
# agent-browser fill @e1 "John Doe"           # Name field
# agent-browser fill @e2 "user@example.com"   # Email field
# agent-browser fill @e3 "+1-555-123-4567"    # Phone field

# Password fields
# agent-browser fill @e4 "SecureP@ssw0rd!"

# Dropdowns
# agent-browser select @e5 "Option Value"

# Checkboxes
# agent-browser check @e6                      # Check
# agent-browser uncheck @e7                    # Uncheck

# Radio buttons
# agent-browser click @e8                      # Select radio option

# Text areas
# agent-browser fill @e9 "Multi-line text content here"

# File uploads
# agent-browser upload @e10 /path/to/file.pdf

# Submit form
# agent-browser click @e11                     # Submit button

# Wait for response
# agent-browser wait --load networkidle
# agent-browser wait --url "**/success"        # Or wait for redirect

# Verify submission
echo "Form submission result:"
agent-browser get url
agent-browser snapshot -i

# Take screenshot of result
agent-browser screenshot /tmp/form-result.png

# Cleanup
agent-browser close

echo "Form automation complete"
