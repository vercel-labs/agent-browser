# Semantic Locators

Use semantic locators as an alternative to refs for stable, readable element selection.

## Quick Start

```bash
# By role and name
agent-browser find role button click --name "Submit"

# By visible text
agent-browser find text "Sign In" click

# By label
agent-browser find label "Email" fill "user@example.com"
```

## Refs vs Semantic Locators

| Feature | Refs (`@e1`) | Semantic Locators |
|---------|--------------|-------------------|
| Speed | Fastest | Slightly slower |
| Stability | Change on DOM update | Stable across changes |
| Readability | Requires snapshot context | Self-documenting |
| Best for | Interactive exploration | Scripted automation |

### When to Use Refs

```bash
# Interactive session - explore and act quickly
agent-browser snapshot -i
# See: @e5 [button] "Submit"
agent-browser click @e5
```

### When to Use Semantic Locators

```bash
# Scripted automation - stable across page changes
agent-browser find role button click --name "Submit"
```

## Locator Types

### Role Locator

Find by ARIA role (accessibility role):

```bash
agent-browser find role button click --name "Submit"
agent-browser find role textbox fill "hello" --name "Search"
agent-browser find role link click --name "Learn more"
agent-browser find role checkbox check --name "Remember me"
agent-browser find role combobox click --name "Country"
```

Common roles: `button`, `textbox`, `link`, `checkbox`, `radio`, `combobox`, `listbox`, `menu`, `menuitem`, `tab`, `dialog`, `alert`

### Text Locator

Find by visible text content:

```bash
agent-browser find text "Sign In" click
agent-browser find text "Welcome back" get text
agent-browser find text "Add to Cart" click

# Exact match only (no partial matching)
agent-browser find text "Sign In" click --exact
```

### Label Locator

Find form fields by their label:

```bash
agent-browser find label "Email" fill "user@example.com"
agent-browser find label "Password" fill "secret123"
agent-browser find label "Remember me" check
```

### Placeholder Locator

Find inputs by placeholder text:

```bash
agent-browser find placeholder "Search..." type "query"
agent-browser find placeholder "Enter your email" fill "test@example.com"
```

### Alt Locator

Find images by alt text:

```bash
agent-browser find alt "Company Logo" click
agent-browser find alt "User avatar" get attr src
```

### Title Locator

Find elements by title attribute:

```bash
agent-browser find title "Close dialog" click
agent-browser find title "More options" hover
```

### Test ID Locator

Find by data-testid attribute (common in React/Vue apps):

```bash
agent-browser find testid "submit-button" click
agent-browser find testid "user-email-input" fill "test@example.com"
agent-browser find testid "error-message" get text
```

### Position Locators

Find by position when multiple matches exist:

```bash
# First matching element
agent-browser find first ".card" click

# Last matching element
agent-browser find last ".card" click

# Nth element (0-indexed)
agent-browser find nth 2 ".card" click
```

## Actions with Locators

All standard actions work with semantic locators:

```bash
# Click actions
agent-browser find text "Submit" click
agent-browser find text "Submit" dblclick

# Input actions
agent-browser find label "Name" fill "John Doe"
agent-browser find label "Name" type "additional text"
agent-browser find label "Name" clear

# Form controls
agent-browser find label "Country" select "United States"
agent-browser find label "Agree" check
agent-browser find label "Newsletter" uncheck

# Information
agent-browser find text "Total" get text
agent-browser find role img get attr src

# Visibility
agent-browser find text "Error" is visible
agent-browser find label "Submit" is enabled
```

## Exact Matching

By default, text matching is partial. Use `--exact` for exact matches:

```bash
# Partial match - finds "Sign In", "Sign In Now", "Please Sign In"
agent-browser find text "Sign In" click

# Exact match - only finds exactly "Sign In"
agent-browser find text "Sign In" click --exact
```

## Chaining Locators

Combine locators for precise selection:

```bash
# Find button within a specific form
agent-browser find role form --name "Login" find role button click --name "Submit"

# Find link in specific section
agent-browser find role navigation find text "Home" click
```

## Common Patterns

### Form Filling

```bash
#!/bin/bash
# Fill a registration form using semantic locators

agent-browser open https://example.com/register

agent-browser find label "First Name" fill "John"
agent-browser find label "Last Name" fill "Doe"
agent-browser find label "Email" fill "john@example.com"
agent-browser find label "Password" fill "SecurePass123!"
agent-browser find label "Confirm Password" fill "SecurePass123!"
agent-browser find label "I agree to the terms" check
agent-browser find role button click --name "Create Account"
```

### Navigation

```bash
#!/bin/bash
# Navigate using semantic locators

agent-browser open https://example.com

# Use nav links
agent-browser find role link click --name "Products"
agent-browser find role link click --name "Pricing"
agent-browser find role link click --name "Contact"

# Use menu
agent-browser find role button click --name "Menu"
agent-browser find role menuitem click --name "Settings"
```

### Accessibility Testing

```bash
#!/bin/bash
# Verify accessibility attributes

# Check all buttons have accessible names
agent-browser find role button get text --all

# Verify form labels
agent-browser find role textbox get attr aria-label --all

# Check images have alt text
agent-browser find role img get attr alt --all
```

### Testing Multiple Items

```bash
#!/bin/bash
# Interact with lists

# Click first item
agent-browser find first ".product-card" click

# Click last item
agent-browser find last ".product-card" click

# Click specific item by index
agent-browser find nth 2 ".product-card" click
```

## Best Practices

1. **Prefer role + name for interactive elements**
   ```bash
   # Good - stable and semantic
   agent-browser find role button click --name "Submit"

   # Avoid - fragile CSS selector
   agent-browser click "#form-submit-btn"
   ```

2. **Use testid for complex UIs**
   ```bash
   # When role/text isn't sufficient
   agent-browser find testid "checkout-submit" click
   ```

3. **Use exact matching for precision**
   ```bash
   # Avoid matching "Sign In Now" when you want "Sign In"
   agent-browser find text "Sign In" click --exact
   ```

4. **Combine with waits for dynamic content**
   ```bash
   agent-browser wait --text "Loading complete"
   agent-browser find role button click --name "Continue"
   ```

5. **Fall back to refs for complex scenarios**
   ```bash
   # When semantic locators don't work, snapshot and use refs
   agent-browser snapshot -i
   agent-browser click @e5
   ```

## Troubleshooting

### Element Not Found

```bash
# Use snapshot to see what's available
agent-browser snapshot -i

# Try less specific locator
agent-browser find text "Sign" click  # Instead of "Sign In"
```

### Multiple Matches

```bash
# Use position locators
agent-browser find first ".btn" click

# Or add more specificity
agent-browser find role button click --name "Submit Form"
```

### Dynamic Content

```bash
# Wait for element to appear
agent-browser wait --text "Submit"
agent-browser find text "Submit" click
```
