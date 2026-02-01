# Network Mocking & Interception

Intercept, mock, and block network requests for API testing, error simulation, and offline development.

## Quick Start

```bash
# Mock an API response
agent-browser network route "https://api.example.com/users" --body '{"users": []}'

# Block analytics/tracking
agent-browser network route "**/analytics**" --abort

# View intercepted requests
agent-browser network requests
```

## Route Commands

```bash
# Intercept and log requests (no modification)
agent-browser network route <url-pattern>

# Block requests entirely
agent-browser network route <url-pattern> --abort

# Return custom response body
agent-browser network route <url-pattern> --body '<json>'

# Remove a route
agent-browser network unroute <url-pattern>

# Remove all routes
agent-browser network unroute
```

## URL Patterns

```bash
# Exact URL
agent-browser network route "https://api.example.com/users"

# Wildcard matching
agent-browser network route "**/api/**"           # Any path containing /api/
agent-browser network route "**/*.png"            # All PNG images
agent-browser network route "**/analytics**"      # Anything with "analytics"

# Domain matching
agent-browser network route "https://*.example.com/**"
```

## Common Patterns

### Mock API Responses

```bash
# Mock user list
agent-browser network route "https://api.example.com/users" \
    --body '{"users": [{"id": 1, "name": "Test User"}]}'

# Mock empty state
agent-browser network route "https://api.example.com/notifications" \
    --body '{"notifications": [], "count": 0}'

# Mock error response (body only - status codes not configurable)
agent-browser network route "https://api.example.com/profile" \
    --body '{"error": "Not found"}'
```

### Block Unwanted Requests

```bash
# Block analytics and tracking
agent-browser network route "**/google-analytics.com/**" --abort
agent-browser network route "**/facebook.com/tr**" --abort
agent-browser network route "**/hotjar.com/**" --abort

# Block ads
agent-browser network route "**/ads.**" --abort
agent-browser network route "**/doubleclick.net/**" --abort

# Block media for faster loading
agent-browser network route "**/*.mp4" --abort
agent-browser network route "**/*.webm" --abort
```

### Simulate Network Conditions

```bash
#!/bin/bash
# Test offline behavior

# Block all external APIs
agent-browser network route "**/api.example.com/**" --abort

# Navigate and test
agent-browser open https://app.example.com
agent-browser snapshot -i

# Check for offline indicators
agent-browser get text ".error-message"
```

### Test Error Handling

```bash
#!/bin/bash
# Simulate various API errors

# Simulate server error response
agent-browser network route "https://api.example.com/submit" \
    --body '{"error": "Internal server error"}'

agent-browser open https://app.example.com/form
agent-browser snapshot -i
agent-browser fill @e1 "test data"
agent-browser click @e2  # Submit button

# Verify error handling UI
agent-browser snapshot -i
agent-browser screenshot ./error-handling.png
```

### Mock Authentication

```bash
# Mock successful auth response
agent-browser network route "https://api.example.com/auth/login" \
    --body '{"token": "mock-jwt-token", "user": {"id": 1, "email": "test@example.com"}}'

# Mock auth failure
agent-browser network route "https://api.example.com/auth/login" \
    --body '{"error": "Invalid credentials"}'
```

## Viewing Requests

```bash
# View all tracked requests
agent-browser network requests

# Filter by pattern
agent-browser network requests --filter api
agent-browser network requests --filter ".json"

# Clear request log
agent-browser network requests --clear
```

### Request Log Format

```
[200] GET https://api.example.com/users (45ms)
[POST] https://api.example.com/login (120ms) -> 200
[BLOCKED] https://analytics.example.com/track
```

## Advanced Patterns

### API Version Testing

```bash
#!/bin/bash
# Test app against different API versions

# Mock v1 response format
agent-browser network route "https://api.example.com/v1/data" \
    --body '{"items": [...]}'

agent-browser open https://app.example.com
agent-browser screenshot ./v1-response.png

# Clear and mock v2 response format
agent-browser network unroute "https://api.example.com/v1/data"
agent-browser network route "https://api.example.com/v2/data" \
    --body '{"data": {"items": [...], "meta": {...}}}'

agent-browser reload
agent-browser screenshot ./v2-response.png
```

### Rate Limit Simulation

```bash
# Return rate limit error response
agent-browser network route "https://api.example.com/search" \
    --body '{"error": "Rate limit exceeded", "retry_after": 60}'
```

### Slow Network Simulation

```bash
# Use set offline to simulate disconnection
agent-browser set offline on
# ... test offline behavior ...
agent-browser set offline off
```

## Best Practices

1. **Clean up routes after tests**
   ```bash
   agent-browser network unroute  # Remove all routes
   ```

2. **Use specific patterns** - Avoid overly broad patterns that might block essential resources

3. **Test with real APIs first** - Mock after understanding the actual API behavior

4. **Log requests during development**
   ```bash
   agent-browser network route "**/api/**"  # Just log, don't modify
   agent-browser network requests
   ```

5. **Combine with state save** - Save state after mocking for reproducible tests
   ```bash
   agent-browser network route "..." --body "..."
   agent-browser open https://app.example.com
   agent-browser state save ./mocked-state.json
   ```

## Troubleshooting

### Routes Not Working

```bash
# Check active routes
agent-browser network requests

# Ensure pattern matches - test with exact URL first
agent-browser network route "https://exact.url.com/path"
```

### Requests Still Going Through

```bash
# Some requests may bypass routing (WebSocket, etc.)
# Use --abort for hard blocking
agent-browser network route "**/unwanted/**" --abort
```

### CORS Issues with Mocked Responses

```bash
# Mock response may need CORS headers for browser to accept
agent-browser network route "https://api.example.com/data" \
    --body '{"data": []}' \
    --headers '{"Access-Control-Allow-Origin": "*"}'
```
