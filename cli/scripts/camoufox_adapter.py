#!/usr/bin/env python3
"""Camoufox sidecar for agent-browser.

The Rust daemon speaks line-delimited JSON to this process.  The sidecar keeps a
Camoufox/Playwright browser alive and translates high-level agent-browser
actions to Playwright calls.  It deliberately does not emulate CDP; actions that
depend on CDP-only domains should fail clearly in Python or be blocked by Rust.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import time
import traceback
from typing import Any
from urllib.parse import urlparse


class AdapterError(Exception):
    pass


def _compact(value: Any) -> Any:
    if isinstance(value, dict):
        return {k: _compact(v) for k, v in value.items() if v is not None}
    if isinstance(value, list):
        return [_compact(v) for v in value]
    return value


def _truthy(value: str | None) -> bool:
    return value is not None and value.lower() in {"1", "true", "yes", "on"}


class CamoufoxSidecar:
    def __init__(self) -> None:
        self.browser_cm = None
        self.browser = None
        self.context = None
        self.pages: list[Any] = []
        self.active = 0
        self.refs: dict[str, str] = {}
        self.init_scripts: dict[str, str] = {}
        self.default_timeout = int(os.environ.get("AGENT_BROWSER_DEFAULT_TIMEOUT", "30000"))

    @property
    def page(self) -> Any:
        if not self.pages:
            raise AdapterError("No active page")
        if self.active >= len(self.pages):
            self.active = max(0, len(self.pages) - 1)
        return self.pages[self.active]

    def launch(self, cmd: dict[str, Any]) -> dict[str, Any]:
        if self.browser is not None:
            return {"launched": True, "reused": True}

        try:
            from camoufox.sync_api import Camoufox
        except Exception as exc:  # pragma: no cover - exercised from Rust integration
            raise AdapterError(
                "Python package 'camoufox' is not importable. Install it with "
                "`python3 -m pip install 'camoufox[geoip]'` or set "
                "AGENT_BROWSER_CAMOUFOX_PYTHON to a Python environment that has it."
            ) from exc

        options: dict[str, Any] = {"headless": cmd.get("headless", True)}

        if cmd.get("executablePath"):
            options["executable_path"] = cmd["executablePath"]
        if cmd.get("args"):
            options["args"] = cmd["args"]

        proxy = cmd.get("proxy")
        if isinstance(proxy, str):
            options["proxy"] = {"server": proxy}
        elif isinstance(proxy, dict) and proxy.get("server"):
            options["proxy"] = _compact(
                {
                    "server": proxy.get("server"),
                    "username": proxy.get("username"),
                    "password": proxy.get("password"),
                }
            )

        if _truthy(os.environ.get("AGENT_BROWSER_CAMOUFOX_GEOIP")):
            options["geoip"] = True
        if _truthy(os.environ.get("AGENT_BROWSER_CAMOUFOX_HUMANIZE")):
            options["humanize"] = True
        if os.environ.get("AGENT_BROWSER_CAMOUFOX_LOCALE"):
            options["locale"] = os.environ["AGENT_BROWSER_CAMOUFOX_LOCALE"]
        if os.environ.get("AGENT_BROWSER_CAMOUFOX_OS"):
            options["os"] = os.environ["AGENT_BROWSER_CAMOUFOX_OS"]

        self.browser_cm = Camoufox(**options)
        self.browser = self.browser_cm.__enter__()
        context_options: dict[str, Any] = {}
        if cmd.get("userAgent"):
            context_options["user_agent"] = cmd["userAgent"]
        if cmd.get("ignoreHTTPSErrors"):
            context_options["ignore_https_errors"] = True
        if cmd.get("storageState"):
            context_options["storage_state"] = cmd["storageState"]
        if os.environ.get("AGENT_BROWSER_CAMOUFOX_LOCALE"):
            context_options["locale"] = os.environ["AGENT_BROWSER_CAMOUFOX_LOCALE"]
        self.context = self.browser.new_context(**context_options)
        self.pages = [self.context.new_page()]
        self.active = 0

        timeout = cmd.get("timeout") or self.default_timeout
        self.page.set_default_timeout(timeout)
        return {"launched": True, "engine": "camoufox"}

    def close(self) -> dict[str, Any]:
        self.refs.clear()
        try:
            if self.context is not None:
                self.context.close()
            if self.browser is not None:
                self.browser.close()
        finally:
            if self.browser_cm is not None:
                try:
                    self.browser_cm.__exit__(None, None, None)
                except Exception:
                    pass
            self.browser_cm = None
            self.browser = None
            self.context = None
            self.pages = []
            self.active = 0
        return {"closed": True}

    def ensure_launched(self) -> None:
        if self.browser is None:
            raise AdapterError("Camoufox browser not launched")
        if self.context is None:
            raise AdapterError("Camoufox browser context not launched")

    def resolve_selector(self, selector: Any) -> str:
        if selector is None:
            raise AdapterError("Missing selector")
        selector = str(selector)
        if selector.startswith("@"):
            ref = selector[1:]
            xpath = self.refs.get(ref)
            if not xpath:
                raise AdapterError(f"Unknown ref {selector}; take a fresh snapshot")
            return f"xpath={xpath}"
        return selector

    def locator(self, selector: Any) -> Any:
        return self.page.locator(self.resolve_selector(selector)).first

    def navigate(self, cmd: dict[str, Any]) -> dict[str, Any]:
        url = cmd.get("url")
        if not url:
            raise AdapterError("Missing 'url' parameter")
        wait_until = cmd.get("waitUntil") or "load"
        self.page.goto(url, wait_until=wait_until, timeout=cmd.get("timeout") or self.default_timeout)
        self.refs.clear()
        return {"url": self.page.url, "title": self.page.title()}

    def url(self, _cmd: dict[str, Any]) -> dict[str, Any]:
        return {"url": self.page.url}

    def title(self, _cmd: dict[str, Any]) -> dict[str, Any]:
        return {"title": self.page.title()}

    def content(self, _cmd: dict[str, Any]) -> dict[str, Any]:
        return {"html": self.page.content(), "origin": self.page.url}

    def evaluate(self, cmd: dict[str, Any]) -> dict[str, Any]:
        script = cmd.get("script")
        if script is None:
            raise AdapterError("Missing 'script' parameter")
        result = self.page.evaluate(script)
        return {"result": result, "origin": self.page.url}

    def snapshot(self, cmd: dict[str, Any]) -> dict[str, Any]:
        selector = cmd.get("selector")
        root_expr = "document.body || document.documentElement"
        if selector:
            if str(selector).startswith("@"):
                root_expr = f"document.evaluate({json.dumps(self.refs.get(str(selector)[1:], ''))}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue"
            else:
                root_expr = f"document.querySelector({json.dumps(str(selector))})"

        max_depth = cmd.get("maxDepth")
        if max_depth is None:
            max_depth = 8

        nodes = self.page.evaluate(
            """
            ({ rootExpr, maxDepth, interactiveOnly }) => {
              const root = eval(rootExpr) || document.body || document.documentElement;
              const out = [];
              const seenText = new Set();

              function visible(el) {
                if (!el || el.nodeType !== Node.ELEMENT_NODE) return false;
                const style = getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
                const rect = el.getBoundingClientRect();
                return rect.width > 0 && rect.height > 0;
              }

              function xpath(el) {
                if (el.id) return `//*[@id="${el.id.replace(/"/g, '\\\\\"')}"]`;
                const parts = [];
                for (; el && el.nodeType === Node.ELEMENT_NODE; el = el.parentElement) {
                  let idx = 1;
                  for (let sib = el.previousElementSibling; sib; sib = sib.previousElementSibling) {
                    if (sib.tagName === el.tagName) idx++;
                  }
                  parts.unshift(`${el.tagName.toLowerCase()}[${idx}]`);
                }
                return '/' + parts.join('/');
              }

              function textOf(el) {
                const aria = el.getAttribute('aria-label');
                if (aria) return aria.trim();
                const labelledBy = el.getAttribute('aria-labelledby');
                if (labelledBy) {
                  const text = labelledBy.split(/\\s+/).map(id => document.getElementById(id)?.innerText || '').join(' ').trim();
                  if (text) return text;
                }
                if (el.alt) return el.alt.trim();
                if (el.placeholder) return el.placeholder.trim();
                if (el.value && /^(button|submit|reset)$/i.test(el.type || '')) return el.value.trim();
                return (el.innerText || el.textContent || '').replace(/\\s+/g, ' ').trim();
              }

              function roleOf(el) {
                const explicit = el.getAttribute('role');
                if (explicit) return explicit;
                const tag = el.tagName.toLowerCase();
                if (/^h[1-6]$/.test(tag)) return 'heading';
                if (tag === 'a' && el.href) return 'link';
                if (tag === 'button') return 'button';
                if (tag === 'select') return 'combobox';
                if (tag === 'textarea') return 'textbox';
                if (tag === 'input') {
                  const type = (el.type || 'text').toLowerCase();
                  if (['button', 'submit', 'reset'].includes(type)) return 'button';
                  if (['checkbox', 'radio'].includes(type)) return type;
                  if (type === 'range') return 'slider';
                  return 'textbox';
                }
                if (tag === 'img' && el.alt) return 'img';
                if (tag === 'summary') return 'button';
                if (tag === 'label') return 'label';
                if (tag === 'p') return 'paragraph';
                if (tag === 'li') return 'listitem';
                return null;
              }

              function interesting(el, role, name) {
                if (!role) return false;
                if (['link','button','textbox','checkbox','radio','combobox','slider','img','heading'].includes(role)) return true;
                if (!interactiveOnly && ['paragraph','listitem','label'].includes(role) && name) return true;
                return false;
              }

              function attrs(el, role) {
                const a = {};
                if (role === 'heading') {
                  const m = /^h([1-6])$/i.exec(el.tagName);
                  if (m) a.level = Number(m[1]);
                }
                if (['checkbox','radio'].includes(role)) a.checked = !!el.checked;
                if (el.disabled) a.disabled = true;
                return a;
              }

              function walk(el, depth) {
                if (!el || out.length >= 250 || depth > maxDepth) return;
                if (el.nodeType !== Node.ELEMENT_NODE || !visible(el)) return;
                const role = roleOf(el);
                let name = textOf(el);
                if (name.length > 180) name = name.slice(0, 177) + '...';
                if (interesting(el, role, name)) {
                  const key = `${depth}:${role}:${name}`;
                  if (!seenText.has(key)) {
                    seenText.add(key);
                    out.push({ role, name, depth, xpath: xpath(el), attrs: attrs(el, role) });
                  }
                }
                for (const child of el.children) walk(child, depth + 1);
              }

              walk(root, 0);
              return out;
            }
            """,
            {
                "rootExpr": root_expr,
                "maxDepth": max_depth,
                "interactiveOnly": bool(cmd.get("interactive")),
            },
        )

        self.refs.clear()
        refs: dict[str, dict[str, str]] = {}
        lines: list[str] = []
        ref_index = 1
        min_depth = min((int(node.get("depth") or 0) for node in nodes), default=0)
        for node in nodes:
            role = node.get("role") or "generic"
            name = node.get("name") or ""
            attrs = node.get("attrs") or {}
            is_ref = role in {"link", "button", "textbox", "checkbox", "radio", "combobox", "slider", "img", "heading"}
            ref = None
            if is_ref:
                ref = f"e{ref_index}"
                ref_index += 1
                self.refs[ref] = node["xpath"]
                refs[ref] = {"role": role, "name": name}
            suffix: list[str] = []
            for key, value in attrs.items():
                if value is True:
                    suffix.append(str(key))
                elif value not in (False, None, ""):
                    suffix.append(f"{key}={value}")
            if ref:
                suffix.append(f"ref={ref}")
            suffix_text = f" [{', '.join(suffix)}]" if suffix else ""
            quoted = f' "{name}"' if name else ""
            depth = min(max(0, int(node.get("depth") or 0) - min_depth), 12)
            lines.append(f"{'  ' * depth}- {role}{quoted}{suffix_text}")

        snapshot = "\n".join(lines) if lines else "- document"
        return {"origin": self.page.url, "refs": refs, "snapshot": snapshot}

    def screenshot(self, cmd: dict[str, Any]) -> dict[str, Any]:
        path = cmd.get("path")
        if not path:
            suffix = ".jpg" if cmd.get("format") == "jpeg" else ".png"
            fd, path = tempfile.mkstemp(prefix="screenshot-", suffix=suffix)
            os.close(fd)
        kwargs: dict[str, Any] = {
            "path": path,
            "full_page": bool(cmd.get("fullPage")),
        }
        if cmd.get("format"):
            kwargs["type"] = cmd["format"]
        if cmd.get("quality") is not None:
            kwargs["quality"] = int(cmd["quality"])
        selector = cmd.get("selector")
        if selector:
            self.locator(selector).screenshot(**kwargs)
        else:
            self.page.screenshot(**kwargs)
        return {"path": path}

    def pdf(self, cmd: dict[str, Any]) -> dict[str, Any]:
        path = cmd.get("path")
        if not path:
            raise AdapterError("Missing 'path' parameter")
        self.page.pdf(path=path)
        return {"path": path}

    def click(self, cmd: dict[str, Any], *, click_count: int = 1) -> dict[str, Any]:
        self.locator(cmd.get("selector")).click(click_count=click_count)
        return {"clicked": True}

    def fill(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).fill(str(cmd.get("value", "")))
        return {"filled": True}

    def type_text(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).type(str(cmd.get("text", "")))
        return {"typed": True}

    def press(self, cmd: dict[str, Any]) -> dict[str, Any]:
        key = cmd.get("key")
        if not key:
            raise AdapterError("Missing 'key' parameter")
        selector = cmd.get("selector")
        if selector:
            self.locator(selector).press(str(key))
        else:
            self.page.keyboard.press(str(key))
        return {"pressed": True}

    def hover(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).hover()
        return {"hovered": True}

    def focus(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).focus()
        return {"focused": True}

    def clear(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).fill("")
        return {"cleared": True}

    def selectall(self, cmd: dict[str, Any]) -> dict[str, Any]:
        loc = self.locator(cmd.get("selector"))
        loc.focus()
        self.page.keyboard.press("Meta+A" if sys.platform == "darwin" else "Control+A")
        return {"selected": True}

    def scrollintoview(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).scroll_into_view_if_needed()
        return {"scrolled": True}

    def select(self, cmd: dict[str, Any]) -> dict[str, Any]:
        values = cmd.get("values")
        result = self.locator(cmd.get("selector")).select_option(values)
        return {"values": result}

    def check(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).check()
        return {"checked": True}

    def uncheck(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.locator(cmd.get("selector")).uncheck()
        return {"checked": False}

    def wait(self, cmd: dict[str, Any]) -> dict[str, Any]:
        timeout = cmd.get("timeout") or self.default_timeout
        if cmd.get("selector"):
            self.locator(cmd.get("selector")).wait_for(timeout=timeout)
            return {"waited": True}
        if cmd.get("text"):
            self.page.get_by_text(str(cmd["text"])).first.wait_for(timeout=timeout)
            return {"waited": True}
        if cmd.get("ms") or cmd.get("timeoutMs"):
            self.page.wait_for_timeout(int(cmd.get("ms") or cmd.get("timeoutMs")))
            return {"waited": True}
        self.page.wait_for_load_state("load", timeout=timeout)
        return {"waited": True}

    def waitforurl(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.page.wait_for_url(str(cmd.get("url")), timeout=cmd.get("timeout") or self.default_timeout)
        return {"url": self.page.url}

    def waitforloadstate(self, cmd: dict[str, Any]) -> dict[str, Any]:
        self.page.wait_for_load_state(str(cmd.get("state") or "load"), timeout=cmd.get("timeout") or self.default_timeout)
        return {"state": cmd.get("state") or "load"}

    def waitforfunction(self, cmd: dict[str, Any]) -> dict[str, Any]:
        script = cmd.get("script") or cmd.get("function")
        if not script:
            raise AdapterError("Missing function/script")
        self.page.wait_for_function(str(script), timeout=cmd.get("timeout") or self.default_timeout)
        return {"waited": True}

    def getter(self, action: str, cmd: dict[str, Any]) -> dict[str, Any]:
        selector = cmd.get("selector")
        loc = self.locator(selector)
        if action == "gettext" or action == "innertext":
            return {"text": loc.inner_text()}
        if action == "innerhtml":
            return {"html": loc.inner_html()}
        if action == "inputvalue":
            return {"value": loc.input_value()}
        if action == "getattribute":
            return {"value": loc.get_attribute(str(cmd.get("attribute")))}
        if action == "count":
            return {"count": self.page.locator(self.resolve_selector(selector)).count()}
        if action == "boundingbox":
            return {"box": loc.bounding_box()}
        if action == "styles":
            return {"styles": loc.evaluate("(el) => Object.fromEntries(Array.from(getComputedStyle(el)).map(k => [k, getComputedStyle(el).getPropertyValue(k)]))")}
        raise AdapterError(f"Unsupported getter: {action}")

    def state_check(self, action: str, cmd: dict[str, Any]) -> dict[str, Any]:
        loc = self.locator(cmd.get("selector"))
        key = action.removeprefix("is")
        if action == "isvisible":
            return {"visible": loc.is_visible()}
        if action == "isenabled":
            return {"enabled": loc.is_enabled()}
        if action == "ischecked":
            return {"checked": loc.is_checked()}
        return {key: False}

    def history(self, action: str) -> dict[str, Any]:
        if action == "back":
            self.page.go_back()
        elif action == "forward":
            self.page.go_forward()
        elif action == "reload":
            self.page.reload()
        return {"url": self.page.url, "title": self.page.title()}

    def cookies_get(self, cmd: dict[str, Any]) -> dict[str, Any]:
        urls = cmd.get("urls") or cmd.get("url")
        if isinstance(urls, str):
            cookies = self.context.cookies([urls])
        else:
            cookies = self.context.cookies(urls)
        return {"cookies": cookies}

    def cookies_set(self, cmd: dict[str, Any]) -> dict[str, Any]:
        cookies = cmd.get("cookies")
        if not isinstance(cookies, list):
            raise AdapterError("Missing 'cookies' array")
        self.context.add_cookies(cookies)
        return {"set": len(cookies)}

    def cookies_clear(self, _cmd: dict[str, Any]) -> dict[str, Any]:
        self.context.clear_cookies()
        return {"cleared": True}

    def storage_get(self, cmd: dict[str, Any]) -> dict[str, Any]:
        origin = cmd.get("origin") or self._origin()
        local_storage = self.page.evaluate("() => Object.assign({}, localStorage)")
        session_storage = self.page.evaluate("() => Object.assign({}, sessionStorage)")
        return {"origin": origin, "localStorage": local_storage, "sessionStorage": session_storage}

    def storage_set(self, cmd: dict[str, Any]) -> dict[str, Any]:
        key = cmd.get("key")
        value = cmd.get("value")
        if key is None:
            raise AdapterError("Missing 'key'")
        storage_type = cmd.get("type") or "local"
        target = "sessionStorage" if storage_type == "session" else "localStorage"
        self.page.evaluate("(args) => window[args.target].setItem(args.key, args.value)", {"target": target, "key": str(key), "value": str(value)})
        return {"set": True}

    def storage_clear(self, cmd: dict[str, Any]) -> dict[str, Any]:
        storage_type = cmd.get("type")
        if storage_type == "session":
            self.page.evaluate("() => sessionStorage.clear()")
        elif storage_type == "local":
            self.page.evaluate("() => localStorage.clear()")
        else:
            self.page.evaluate("() => { localStorage.clear(); sessionStorage.clear(); }")
        return {"cleared": True}

    def state_save(self, cmd: dict[str, Any]) -> dict[str, Any]:
        path = cmd.get("path")
        if not path:
            fd, path = tempfile.mkstemp(prefix="camoufox-storage-state-", suffix=".json")
            os.close(fd)
        self.context.storage_state(path=str(path))
        return {"saved": True, "path": str(path)}

    def state_load(self, cmd: dict[str, Any]) -> dict[str, Any]:
        path = cmd.get("path")
        if not path:
            raise AdapterError("Missing 'path' parameter")
        with open(path, "r", encoding="utf-8") as handle:
            state = json.load(handle)
        cookies = state.get("cookies") or []
        if cookies:
            self.context.add_cookies(cookies)
        current_url = self.page.url
        for origin in state.get("origins") or []:
            origin_url = origin.get("origin")
            if not origin_url:
                continue
            self.page.goto(origin_url)
            for item in origin.get("localStorage") or []:
                self.page.evaluate(
                    "(item) => localStorage.setItem(item.name, item.value)",
                    item,
                )
        if current_url and current_url != "about:blank":
            self.page.goto(current_url)
        return {"loaded": True, "path": str(path)}

    def setcontent(self, cmd: dict[str, Any]) -> dict[str, Any]:
        html = cmd.get("html") or cmd.get("content")
        if html is None:
            raise AdapterError("Missing html/content")
        self.page.set_content(str(html))
        return {"set": True}

    def keyboard(self, cmd: dict[str, Any]) -> dict[str, Any]:
        sub = cmd.get("subaction")
        text = str(cmd.get("text", ""))
        if sub in {"type", "press"}:
            if sub == "press":
                self.page.keyboard.press(text)
            else:
                self.page.keyboard.type(text)
        elif sub in {"insertText", "inserttext"}:
            self.page.keyboard.insert_text(text)
        else:
            raise AdapterError(f"Unsupported keyboard subaction: {sub}")
        return {"ok": True}

    def mouse(self, cmd: dict[str, Any]) -> dict[str, Any]:
        sub = cmd.get("subaction") or cmd.get("type") or "move"
        x = float(cmd.get("x", 0))
        y = float(cmd.get("y", 0))
        if sub in {"move", "mousemove"}:
            self.page.mouse.move(x, y)
        elif sub in {"down", "mousedown"}:
            self.page.mouse.down()
        elif sub in {"up", "mouseup"}:
            self.page.mouse.up()
        elif sub == "click":
            self.page.mouse.click(x, y)
        elif sub == "wheel":
            self.page.mouse.wheel(float(cmd.get("deltaX", 0)), float(cmd.get("deltaY", 0)))
        else:
            raise AdapterError(f"Unsupported mouse subaction: {sub}")
        return {"ok": True}

    def viewport(self, cmd: dict[str, Any]) -> dict[str, Any]:
        width = int(cmd.get("width") or 1280)
        height = int(cmd.get("height") or 720)
        self.page.set_viewport_size({"width": width, "height": height})
        return {"viewport": {"width": width, "height": height}}

    def useragent(self, _cmd: dict[str, Any]) -> dict[str, Any]:
        ua = self.page.evaluate("() => navigator.userAgent")
        return {"userAgent": ua}

    def upload(self, cmd: dict[str, Any]) -> dict[str, Any]:
        files = cmd.get("files")
        if isinstance(files, str):
            files = [files]
        self.locator(cmd.get("selector")).set_input_files(files or [])
        return {"uploaded": len(files or [])}

    def addscript(self, cmd: dict[str, Any]) -> dict[str, Any]:
        script = cmd.get("script")
        if not script:
            raise AdapterError("Missing script")
        self.page.add_script_tag(content=str(script))
        return {"added": True}

    def addstyle(self, cmd: dict[str, Any]) -> dict[str, Any]:
        style = cmd.get("style") or cmd.get("css")
        if not style:
            raise AdapterError("Missing style/css")
        self.page.add_style_tag(content=str(style))
        return {"added": True}

    def addinitscript(self, cmd: dict[str, Any]) -> dict[str, Any]:
        script = cmd.get("script")
        if not script:
            raise AdapterError("Missing script")
        script_id = f"init-{int(time.time() * 1000)}"
        self.init_scripts[script_id] = str(script)
        self.context.add_init_script(str(script))
        return {"identifier": script_id}

    def tab_list(self, _cmd: dict[str, Any]) -> dict[str, Any]:
        tabs = []
        for i, page in enumerate(self.pages):
            tabs.append(
                {
                    "id": f"t{i + 1}",
                    "tabId": i + 1,
                    "url": page.url,
                    "title": page.title(),
                    "active": i == self.active,
                    "targetType": "page",
                }
            )
        return {"tabs": tabs, "active": f"t{self.active + 1}" if self.pages else None}

    def tab_new(self, cmd: dict[str, Any]) -> dict[str, Any]:
        page = self.context.new_page()
        self.pages.append(page)
        self.active = len(self.pages) - 1
        if cmd.get("url"):
            page.goto(str(cmd["url"]))
        return {"tab": f"t{self.active + 1}", "url": page.url, "title": page.title()}

    def tab_switch(self, cmd: dict[str, Any]) -> dict[str, Any]:
        ref = str(cmd.get("tab") or cmd.get("tabId") or cmd.get("id") or "")
        index = self._tab_index(ref)
        self.active = index
        self.page.bring_to_front()
        return {"tab": f"t{self.active + 1}", "url": self.page.url, "title": self.page.title()}

    def tab_close(self, cmd: dict[str, Any]) -> dict[str, Any]:
        ref = cmd.get("tab") or cmd.get("tabId") or cmd.get("id")
        index = self._tab_index(str(ref)) if ref else self.active
        page = self.pages.pop(index)
        page.close()
        if not self.pages:
            self.pages.append(self.context.new_page())
        self.active = min(self.active, len(self.pages) - 1)
        return {"closed": True, "active": f"t{self.active + 1}"}

    def find(self, action: str, cmd: dict[str, Any]) -> dict[str, Any]:
        subaction = cmd.get("subaction") or cmd.get("locatorAction") or "click"
        exact = bool(cmd.get("exact"))
        if action == "getbyrole":
            loc = self.page.get_by_role(str(cmd.get("role")), name=cmd.get("name"), exact=exact)
        elif action == "getbytext":
            loc = self.page.get_by_text(str(cmd.get("text")), exact=exact)
        elif action == "getbylabel":
            loc = self.page.get_by_label(str(cmd.get("text") or cmd.get("label")), exact=exact)
        elif action == "getbyplaceholder":
            loc = self.page.get_by_placeholder(str(cmd.get("text")), exact=exact)
        elif action == "getbyalttext":
            loc = self.page.get_by_alt_text(str(cmd.get("text")), exact=exact)
        elif action == "getbytitle":
            loc = self.page.get_by_title(str(cmd.get("text")), exact=exact)
        elif action == "getbytestid":
            loc = self.page.get_by_test_id(str(cmd.get("text") or cmd.get("testid")))
        elif action == "nth":
            loc = self.page.locator(self.resolve_selector(cmd.get("selector"))).nth(int(cmd.get("index", 0)))
        else:
            raise AdapterError(f"Unsupported locator action: {action}")
        loc = loc.first
        return self._locator_subaction(loc, subaction, cmd)

    def _locator_subaction(self, loc: Any, subaction: str, cmd: dict[str, Any]) -> dict[str, Any]:
        if subaction in {"click", "press"}:
            if subaction == "press":
                loc.press(str(cmd.get("value") or cmd.get("key") or "Enter"))
            else:
                loc.click()
            return {"ok": True}
        if subaction == "fill":
            loc.fill(str(cmd.get("value", "")))
            return {"ok": True}
        if subaction in {"text", "gettext", "innertext"}:
            return {"text": loc.inner_text()}
        if subaction in {"count"}:
            return {"count": loc.count()}
        if subaction in {"visible", "isvisible"}:
            return {"visible": loc.is_visible()}
        return {"text": loc.inner_text()}

    def _tab_index(self, ref: str) -> int:
        if ref.startswith("t"):
            ref = ref[1:]
        try:
            index = int(ref) - 1
        except ValueError as exc:
            raise AdapterError(f"Invalid tab ref: {ref}") from exc
        if index < 0 or index >= len(self.pages):
            raise AdapterError(f"Unknown tab: t{index + 1}")
        return index

    def _origin(self) -> str:
        parsed = urlparse(self.page.url)
        if not parsed.scheme or not parsed.netloc:
            return self.page.url
        return f"{parsed.scheme}://{parsed.netloc}"

    def handle(self, action: str, cmd: dict[str, Any]) -> dict[str, Any]:
        if action == "launch":
            return self.launch(cmd)
        if action == "close":
            return self.close()

        self.ensure_launched()

        if action == "navigate":
            return self.navigate(cmd)
        if action == "url":
            return self.url(cmd)
        if action == "title":
            return self.title(cmd)
        if action == "content":
            return self.content(cmd)
        if action == "evaluate":
            return self.evaluate(cmd)
        if action == "snapshot":
            return self.snapshot(cmd)
        if action == "screenshot":
            return self.screenshot(cmd)
        if action == "pdf":
            return self.pdf(cmd)
        if action == "click":
            return self.click(cmd)
        if action == "dblclick":
            return self.click(cmd, click_count=2)
        if action == "fill":
            return self.fill(cmd)
        if action == "type":
            return self.type_text(cmd)
        if action in {"press", "keydown", "keyup", "inserttext"}:
            if action == "inserttext":
                self.page.keyboard.insert_text(str(cmd.get("text", "")))
                return {"inserted": True}
            if action == "keydown":
                self.page.keyboard.down(str(cmd.get("key")))
                return {"ok": True}
            if action == "keyup":
                self.page.keyboard.up(str(cmd.get("key")))
                return {"ok": True}
            return self.press(cmd)
        if action == "hover":
            return self.hover(cmd)
        if action == "focus":
            return self.focus(cmd)
        if action == "clear":
            return self.clear(cmd)
        if action == "selectall":
            return self.selectall(cmd)
        if action == "scrollintoview":
            return self.scrollintoview(cmd)
        if action == "select":
            return self.select(cmd)
        if action == "check":
            return self.check(cmd)
        if action == "uncheck":
            return self.uncheck(cmd)
        if action in {"wait", "waitforurl", "waitforloadstate", "waitforfunction"}:
            return getattr(self, action)(cmd)
        if action in {"gettext", "innertext", "innerhtml", "inputvalue", "getattribute", "count", "boundingbox", "styles"}:
            return self.getter(action, cmd)
        if action in {"isvisible", "isenabled", "ischecked"}:
            return self.state_check(action, cmd)
        if action in {"back", "forward", "reload"}:
            return self.history(action)
        if action == "cookies_get":
            return self.cookies_get(cmd)
        if action == "cookies_set":
            return self.cookies_set(cmd)
        if action == "cookies_clear":
            return self.cookies_clear(cmd)
        if action == "storage_get":
            return self.storage_get(cmd)
        if action == "storage_set":
            return self.storage_set(cmd)
        if action == "storage_clear":
            return self.storage_clear(cmd)
        if action == "state_save":
            return self.state_save(cmd)
        if action == "state_load":
            return self.state_load(cmd)
        if action == "setcontent":
            return self.setcontent(cmd)
        if action == "keyboard":
            return self.keyboard(cmd)
        if action in {"mouse", "mousemove", "mousedown", "mouseup", "wheel"}:
            if action != "mouse":
                cmd = {**cmd, "subaction": action}
            return self.mouse(cmd)
        if action == "viewport":
            return self.viewport(cmd)
        if action in {"useragent", "user_agent"}:
            return self.useragent(cmd)
        if action == "upload":
            return self.upload(cmd)
        if action == "addscript":
            return self.addscript(cmd)
        if action == "addstyle":
            return self.addstyle(cmd)
        if action == "addinitscript":
            return self.addinitscript(cmd)
        if action == "tab_list":
            return self.tab_list(cmd)
        if action == "tab_new":
            return self.tab_new(cmd)
        if action == "tab_switch":
            return self.tab_switch(cmd)
        if action == "tab_close":
            return self.tab_close(cmd)
        if action in {"getbyrole", "getbytext", "getbylabel", "getbyplaceholder", "getbyalttext", "getbytitle", "getbytestid", "nth"}:
            return self.find(action, cmd)

        raise AdapterError(
            f"Action '{action}' is not supported by the Camoufox adapter yet. "
            "This action depends on Chrome/CDP-specific plumbing or has not been mapped to Playwright."
        )


def respond(message_id: Any, success: bool, data: Any = None, error: str | None = None) -> None:
    sys.stdout.write(json.dumps({"id": message_id, "success": success, "data": data, "error": error}, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def main() -> int:
    sidecar = CamoufoxSidecar()
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            message = json.loads(line)
            message_id = message.get("id")
            action = message.get("action")
            cmd = message.get("cmd") or {}
            if not action:
                raise AdapterError("Missing action")
            data = sidecar.handle(str(action), cmd)
            respond(message_id, True, data=data)
        except Exception as exc:
            if os.environ.get("AGENT_BROWSER_CAMOUFOX_DEBUG"):
                traceback.print_exc(file=sys.stderr)
            respond(locals().get("message_id", None), False, error=str(exc))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
