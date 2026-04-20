# camoufox-sidecar

Python sidecar that agent-browser spawns when `--engine camoufox` is selected.
It drives [Camoufox](https://camoufox.com/) via Playwright and speaks a
JSON-line protocol over stdio to the Rust daemon.

This package is not meant to be used directly by humans. See
`docs/engines/camoufox.md` in the agent-browser repo for the user-facing docs.

## Install

```
pip install -U "camoufox[geoip]"
python -m camoufox fetch
pip install -e packages/camoufox-sidecar
```

## Run

```
python -m camoufox_sidecar
```

Emits `{"event": "ready"}` on startup, then reads JSON-line commands from
stdin.

## Test

```
pip install -e 'packages/camoufox-sidecar[test]'
pytest packages/camoufox-sidecar/tests/
```
