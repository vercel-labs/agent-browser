# Changelog

All notable changes to agent-browser-mcp-server will be documented in this file.

## [2.0.0] - 2026-01-22

### Added
- 🎉 **50+ browser automation tools** for LLMs
- Direct `BrowserManager` integration (no subprocess overhead)
- Full navigation support (navigate, back, forward, reload)
- Complete interaction tools (click, fill, type, hover, select, etc.)
- Information gathering (snapshot, get text, screenshots, etc.)
- Multi-tab support (new, switch, close, list)
- Cookie management (get, set, clear)
- Storage management (localStorage, sessionStorage)
- Frame handling (switch to iframe, back to main)
- Dialog handling (accept, dismiss)
- Network request tracking
- Viewport and geolocation settings
- Console and error monitoring
- Session management for parallel browsers

### Changed
- Architecture: Direct import instead of CLI subprocess
- Headed mode by default (browser window visible)
- Improved error handling and reporting
- Better TypeScript types

### Documentation
- Complete README with 50+ tools documented
- SETUP.md with step-by-step installation
- Example config files for Cursor and Claude Desktop
- Troubleshooting guide

## [1.0.0] - Initial Release

### Added
- Basic MCP server implementation
- CLI subprocess approach
- Core browser commands
- README documentation
