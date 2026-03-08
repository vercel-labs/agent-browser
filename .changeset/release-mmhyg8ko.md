---
"agent-browser": minor
---

### New Features

- **Lightpanda browser engine support** - Added `--engine <name>` flag to select the browser engine (`chrome` by default, or `lightpanda`), implying `--native` mode. Configurable via `AGENT_BROWSER_ENGINE` environment variable (#646)
- **Dialog dismiss command** - Added support for `dismiss` subcommand in dialog command parsing (#605)

### Improvements

- **Daemon startup error reporting** - Daemon startup errors are now surfaced directly instead of showing an opaque timeout message (#614)
- **CDP port discovery** - Replaced broken hand-rolled HTTP client with `reqwest` for more reliable CDP port discovery (#619)
- **Chrome extensions** - Extensions now load correctly by forcing headed mode when extensions are present (#652)
- **Google Translate bar suppression** - Suppressed the Google Translate bar in native headless mode to avoid interference (#649)
- **Auth cookie persistence** - Auth cookies are now persisted on browser close in native mode (#650)

### Bug Fixes

- Fixed native auth login failing due to incompatible encryption format (#648)

### Documentation

- Improved snapshot usage guidance and added reproducibility check (#630)
- Added `--engine` flag to the README options table

### Performance

- Added benchmarks to the CLI codebase (#637)
