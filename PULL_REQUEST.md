# Add JavaScript-based Click for React SPA Compatibility

## Problem

The current `click` command uses CDP's coordinate-based mouse events (`Input.dispatchMouseEvent`) which don't properly trigger React's `onClick` handlers in Single Page Applications (SPAs). This affects Material-UI, Ant Design, and other React component libraries.

**Issue:** #XXX (link to issue when created)

## Solution

Added `click_js` command that uses JavaScript's `element.click()` method via CDP's `Runtime.callFunctionOn`, ensuring proper event bubbling through React's SyntheticEvent system.

## Changes

### 1. Core Implementation (`cli/src/native/interaction.rs`)

- Added `click_js()` function that calls `element.click()` via JavaScript
- Added comprehensive documentation explaining when and why to use this method
- Explains React SyntheticEvent system and why coordinate-based clicks fail

### 2. Action Handler (`cli/src/native/actions.rs`)

- Added `handle_click_js()` to process click_js commands
- Registered new action in the command dispatch match statement

### 3. CLI Integration (`cli/src/commands.rs`)

- Added `click_js` command parsing
- Supports standard selector syntax (CSS, XPath, @ref)

### 4. Documentation (`cli/src/output.rs`)

- Added detailed help text for `click_js` command
- Explains React SPA compatibility
- Provides usage examples

### 5. README Update (`README.md`)

- Added `click_js` to core commands list

## Usage

```bash
# Standard click (coordinate-based, faster)
agent-browser click "button"

# JavaScript click (React SPA compatible)
agent-browser click_js "button"

# Works with all selector types
agent-browser click_js @e1
agent-browser click_js "[data-testid='add-button']"
agent-browser click_js "//button[@type='submit']"
```

## When to Use

**Use `click_js` instead of `click` when:**

- Testing React Single Page Applications (SPAs)
- Clicking Material-UI, Ant Design, or Chakra UI components
- Standard `click` reports success but nothing happens
- Event handlers attached via React's `onClick` prop

**Technical Details:**
React attaches event listeners to the root container and uses event delegation. Events must bubble through React's event system to trigger `onClick` handlers. The native `element.click()` method ensures proper event bubbling, while coordinate-based mouse events may not.

## Testing

The implementation has been tested against:

- React 18 applications
- Material-UI Floating Action Buttons (FAB)
- Standard React onClick handlers
- Complex React event delegation scenarios

## Performance

`click_js` is slightly slower than `click` (JavaScript execution overhead), but the difference is negligible for most use cases. For non-React applications, continue using `click` for optimal performance.

## Future Enhancements

Potential future improvements (out of scope for this PR):

1. Auto-detection: Automatically use JavaScript click when React is detected
2. Smart click: Hybrid approach that tries coordinate first, falls back to JS
3. Framework-specific optimizations for Vue, Angular, etc.

## Checklist

- [x] Code follows Rust style guidelines (`cargo fmt`)
- [x] Documentation updated (README, --help output, inline docs)
- [x] New command registered in CLI parser
- [x] Action handler implemented
- [x] Help text added
- [x] No breaking changes to existing functionality

## Related

- React SyntheticEvent documentation: https://react.dev/reference/react-dom/components/common#react-event-object
- Chrome DevTools Protocol: https://chromedevtools.github.io/devtools-protocol/
- Material-UI Event Handling: https://mui.com/material-ui/getting-started/learn-more/#event-handling

---

**Impact:** High - Enables testing of modern React SPAs that were previously untestable with agent-browser
