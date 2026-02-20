import * as fs from 'fs';
import * as path from 'path';
import type { Page, Frame } from 'playwright-core';
import { mkdirSync } from 'node:fs';
import type { BrowserManager, ScreencastFrame } from './browser.js';
import { getAppDir } from './daemon.js';
import {
  getSessionsDir,
  readStateFile,
  isValidSessionName,
  isEncryptedPayload,
  listStateFiles,
  cleanupExpiredStates,
} from './state-utils.js';
import type {
  Command,
  Response,
  NavigateCommand,
  ClickCommand,
  TypeCommand,
  FillCommand,
  CheckCommand,
  UncheckCommand,
  UploadCommand,
  DoubleClickCommand,
  FocusCommand,
  DragCommand,
  FrameCommand,
  GetByRoleCommand,
  GetByTextCommand,
  GetByLabelCommand,
  GetByPlaceholderCommand,
  PressCommand,
  ScreenshotCommand,
  EvaluateCommand,
  WaitCommand,
  ScrollCommand,
  SelectCommand,
  HoverCommand,
  ContentCommand,
  TabNewCommand,
  TabSwitchCommand,
  TabCloseCommand,
  WindowNewCommand,
  CookiesSetCommand,
  StorageGetCommand,
  StorageSetCommand,
  StorageClearCommand,
  DialogCommand,
  PdfCommand,
  RouteCommand,
  RequestsCommand,
  DownloadCommand,
  GeolocationCommand,
  PermissionsCommand,
  ViewportCommand,
  DeviceCommand,
  GetAttributeCommand,
  GetTextCommand,
  IsVisibleCommand,
  IsEnabledCommand,
  IsCheckedCommand,
  CountCommand,
  BoundingBoxCommand,
  StylesCommand,
  TraceStartCommand,
  TraceStopCommand,
  ProfilerStartCommand,
  ProfilerStopCommand,
  HarStopCommand,
  StorageStateSaveCommand,
  StateListCommand,
  StateClearCommand,
  StateShowCommand,
  StateCleanCommand,
  StateRenameCommand,
  ConsoleCommand,
  ErrorsCommand,
  KeyboardCommand,
  WheelCommand,
  TapCommand,
  ClipboardCommand,
  HighlightCommand,
  ClearCommand,
  SelectAllCommand,
  InnerTextCommand,
  InnerHtmlCommand,
  InputValueCommand,
  SetValueCommand,
  DispatchEventCommand,
  AddScriptCommand,
  AddStyleCommand,
  EmulateMediaCommand,
  OfflineCommand,
  HeadersCommand,
  GetByAltTextCommand,
  GetByTitleCommand,
  GetByTestIdCommand,
  NthCommand,
  WaitForUrlCommand,
  WaitForLoadStateCommand,
  SetContentCommand,
  TimezoneCommand,
  LocaleCommand,
  HttpCredentialsCommand,
  MouseMoveCommand,
  MouseDownCommand,
  MouseUpCommand,
  WaitForFunctionCommand,
  ScrollIntoViewCommand,
  AddInitScriptCommand,
  KeyDownCommand,
  KeyUpCommand,
  InsertTextCommand,
  MultiSelectCommand,
  WaitForDownloadCommand,
  ResponseBodyCommand,
  ScreencastStartCommand,
  ScreencastStopCommand,
  InputMouseCommand,
  InputKeyboardCommand,
  InputTouchCommand,
  RecordingStartCommand,
  RecordingStopCommand,
  RecordingRestartCommand,
  DiffSnapshotCommand,
  DiffScreenshotCommand,
  DiffUrlCommand,
  Annotation,
  NavigateData,
  ScreenshotData,
  EvaluateData,
  DiffSnapshotData,
  DiffScreenshotData,
  DiffUrlData,
  ContentData,
  TabListData,
  TabNewData,
  TabSwitchData,
  TabCloseData,
  ScreencastStartData,
  ScreencastStopData,
  RecordingStartData,
  RecordingStopData,
  RecordingRestartData,
  InputEventData,
  StylesData,
} from './types.js';
import { successResponse, errorResponse } from './protocol.js';
import { diffSnapshots, diffScreenshots } from './diff.js';
import { getEnhancedSnapshot } from './snapshot.js';

// Callback for screencast frames - will be set by the daemon when streaming is active
let screencastFrameCallback: ((frame: ScreencastFrame) => void) | null = null;

/**
 * Set the callback for screencast frames
 * This is called by the daemon to set up frame streaming
 */
export function setScreencastFrameCallback(
  callback: ((frame: ScreencastFrame) => void) | null
): void {
  screencastFrameCallback = callback;
}

// Snapshot response type
interface SnapshotData {
  snapshot: string;
  refs?: Record<string, { role: string; name?: string }>;
}

/**
 * Convert Playwright errors to AI-friendly messages
 * @internal Exported for testing
 */
export function toAIFriendlyError(error: unknown, selector: string): Error {
  const message = error instanceof Error ? error.message : String(error);

  // Handle strict mode violation (multiple elements match)
  if (message.includes('strict mode violation')) {
    // Extract count if available
    const countMatch = message.match(/resolved to (\d+) elements/);
    const count = countMatch ? countMatch[1] : 'multiple';

    return new Error(
      `Selector "${selector}" matched ${count} elements. ` +
        `Run 'snapshot' to get updated refs, or use a more specific CSS selector.`
    );
  }

  // Handle element not interactable (must be checked BEFORE timeout case)
  // This includes cases where an overlay/modal blocks the element
  if (message.includes('intercepts pointer events')) {
    return new Error(
      `Element "${selector}" is blocked by another element (likely a modal or overlay). ` +
        `Try dismissing any modals/cookie banners first.`
    );
  }

  // Handle element not visible
  if (message.includes('not visible') && !message.includes('Timeout')) {
    return new Error(
      `Element "${selector}" is not visible. ` +
        `Try scrolling it into view or check if it's hidden.`
    );
  }

  // Handle general timeout (element exists but action couldn't complete)
  if (message.includes('Timeout') && message.includes('exceeded')) {
    return new Error(
      `Action on "${selector}" timed out. The element may be blocked, still loading, or not interactable. ` +
        `Run 'snapshot' to check the current page state.`
    );
  }

  // Handle element not found (timeout waiting for element)
  if (
    message.includes('waiting for') &&
    (message.includes('to be visible') || message.includes('Timeout'))
  ) {
    return new Error(
      `Element "${selector}" not found or not visible. ` +
        `Run 'snapshot' to see current page elements.`
    );
  }

  // Return original error for unknown cases
  return error instanceof Error ? error : new Error(message);
}

/**
 * Execute a command and return a response
 */
export async function executeCommand(command: Command, browser: BrowserManager): Promise<Response> {
  try {
    switch (command.action) {
      case 'launch':
        return await handleLaunch(command, browser);
      case 'navigate':
        return await handleNavigate(command, browser);
      case 'click':
        return await handleClick(command, browser);
      case 'type':
        return await handleType(command, browser);
      case 'fill':
        return await handleFill(command, browser);
      case 'check':
        return await handleCheck(command, browser);
      case 'uncheck':
        return await handleUncheck(command, browser);
      case 'upload':
        return await handleUpload(command, browser);
      case 'dblclick':
        return await handleDoubleClick(command, browser);
      case 'focus':
        return await handleFocus(command, browser);
      case 'drag':
        return await handleDrag(command, browser);
      case 'frame':
        return await handleFrame(command, browser);
      case 'mainframe':
        return await handleMainFrame(command, browser);
      case 'getbyrole':
        return await handleGetByRole(command, browser);
      case 'getbytext':
        return await handleGetByText(command, browser);
      case 'getbylabel':
        return await handleGetByLabel(command, browser);
      case 'getbyplaceholder':
        return await handleGetByPlaceholder(command, browser);
      case 'press':
        return await handlePress(command, browser);
      case 'screenshot':
        return await handleScreenshot(command, browser);
      case 'snapshot':
        return await handleSnapshot(command, browser);
      case 'evaluate':
        return await handleEvaluate(command, browser);
      case 'wait':
        return await handleWait(command, browser);
      case 'scroll':
        return await handleScroll(command, browser);
      case 'select':
        return await handleSelect(command, browser);
      case 'hover':
        return await handleHover(command, browser);
      case 'content':
        return await handleContent(command, browser);
      case 'close':
        return await handleClose(command, browser);
      case 'tab_new':
        return await handleTabNew(command, browser);
      case 'tab_list':
        return await handleTabList(command, browser);
      case 'tab_switch':
        return await handleTabSwitch(command, browser);
      case 'tab_close':
        return await handleTabClose(command, browser);
      case 'window_new':
        return await handleWindowNew(command, browser);
      case 'cookies_get':
        return await handleCookiesGet(command, browser);
      case 'cookies_set':
        return await handleCookiesSet(command, browser);
      case 'cookies_clear':
        return await handleCookiesClear(command, browser);
      case 'storage_get':
        return await handleStorageGet(command, browser);
      case 'storage_set':
        return await handleStorageSet(command, browser);
      case 'storage_clear':
        return await handleStorageClear(command, browser);
      case 'dialog':
        return await handleDialog(command, browser);
      case 'pdf':
        return await handlePdf(command, browser);
      case 'route':
        return await handleRoute(command, browser);
      case 'unroute':
        return await handleUnroute(command, browser);
      case 'requests':
        return await handleRequests(command, browser);
      case 'download':
        return await handleDownload(command, browser);
      case 'geolocation':
        return await handleGeolocation(command, browser);
      case 'permissions':
        return await handlePermissions(command, browser);
      case 'viewport':
        return await handleViewport(command, browser);
      case 'useragent':
        return await handleUserAgent(command, browser);
      case 'device':
        return await handleDevice(command, browser);
      case 'back':
        return await handleBack(command, browser);
      case 'forward':
        return await handleForward(command, browser);
      case 'reload':
        return await handleReload(command, browser);
      case 'url':
        return await handleUrl(command, browser);
      case 'title':
        return await handleTitle(command, browser);
      case 'getattribute':
        return await handleGetAttribute(command, browser);
      case 'gettext':
        return await handleGetText(command, browser);
      case 'isvisible':
        return await handleIsVisible(command, browser);
      case 'isenabled':
        return await handleIsEnabled(command, browser);
      case 'ischecked':
        return await handleIsChecked(command, browser);
      case 'count':
        return await handleCount(command, browser);
      case 'boundingbox':
        return await handleBoundingBox(command, browser);
      case 'styles':
        return await handleStyles(command, browser);
      case 'video_start':
        return await handleVideoStart(command, browser);
      case 'video_stop':
        return await handleVideoStop(command, browser);
      case 'trace_start':
        return await handleTraceStart(command, browser);
      case 'trace_stop':
        return await handleTraceStop(command, browser);
      case 'profiler_start':
        return await handleProfilerStart(command, browser);
      case 'profiler_stop':
        return await handleProfilerStop(command, browser);
      case 'har_start':
        return await handleHarStart(command, browser);
      case 'har_stop':
        return await handleHarStop(command, browser);
      case 'state_save':
        return await handleStateSave(command, browser);
      case 'state_load':
        return await handleStateLoad(command, browser);
      case 'state_list':
        return await handleStateList(command);
      case 'state_clear':
        return await handleStateClear(command);
      case 'state_show':
        return await handleStateShow(command);
      case 'state_clean':
        return await handleStateClean(command);
      case 'state_rename':
        return await handleStateRename(command);
      case 'console':
        return await handleConsole(command, browser);
      case 'errors':
        return await handleErrors(command, browser);
      case 'keyboard':
        return await handleKeyboard(command, browser);
      case 'wheel':
        return await handleWheel(command, browser);
      case 'tap':
        return await handleTap(command, browser);
      case 'clipboard':
        return await handleClipboard(command, browser);
      case 'highlight':
        return await handleHighlight(command, browser);
      case 'clear':
        return await handleClear(command, browser);
      case 'selectall':
        return await handleSelectAll(command, browser);
      case 'innertext':
        return await handleInnerText(command, browser);
      case 'innerhtml':
        return await handleInnerHtml(command, browser);
      case 'inputvalue':
        return await handleInputValue(command, browser);
      case 'setvalue':
        return await handleSetValue(command, browser);
      case 'dispatch':
        return await handleDispatch(command, browser);
      case 'evalhandle':
        return await handleEvalHandle(command, browser);
      case 'expose':
        return await handleExpose(command, browser);
      case 'addscript':
        return await handleAddScript(command, browser);
      case 'addstyle':
        return await handleAddStyle(command, browser);
      case 'emulatemedia':
        return await handleEmulateMedia(command, browser);
      case 'offline':
        return await handleOffline(command, browser);
      case 'headers':
        return await handleHeaders(command, browser);
      case 'pause':
        return await handlePause(command, browser);
      case 'getbyalttext':
        return await handleGetByAltText(command, browser);
      case 'getbytitle':
        return await handleGetByTitle(command, browser);
      case 'getbytestid':
        return await handleGetByTestId(command, browser);
      case 'nth':
        return await handleNth(command, browser);
      case 'waitforurl':
        return await handleWaitForUrl(command, browser);
      case 'waitforloadstate':
        return await handleWaitForLoadState(command, browser);
      case 'setcontent':
        return await handleSetContent(command, browser);
      case 'timezone':
        return await handleTimezone(command, browser);
      case 'locale':
        return await handleLocale(command, browser);
      case 'credentials':
        return await handleCredentials(command, browser);
      case 'mousemove':
        return await handleMouseMove(command, browser);
      case 'mousedown':
        return await handleMouseDown(command, browser);
      case 'mouseup':
        return await handleMouseUp(command, browser);
      case 'bringtofront':
        return await handleBringToFront(command, browser);
      case 'waitforfunction':
        return await handleWaitForFunction(command, browser);
      case 'scrollintoview':
        return await handleScrollIntoView(command, browser);
      case 'addinitscript':
        return await handleAddInitScript(command, browser);
      case 'keydown':
        return await handleKeyDown(command, browser);
      case 'keyup':
        return await handleKeyUp(command, browser);
      case 'inserttext':
        return await handleInsertText(command, browser);
      case 'multiselect':
        return await handleMultiSelect(command, browser);
      case 'waitfordownload':
        return await handleWaitForDownload(command, browser);
      case 'responsebody':
        return await handleResponseBody(command, browser);
      case 'screencast_start':
        return await handleScreencastStart(command, browser);
      case 'screencast_stop':
        return await handleScreencastStop(command, browser);
      case 'input_mouse':
        return await handleInputMouse(command, browser);
      case 'input_keyboard':
        return await handleInputKeyboard(command, browser);
      case 'input_touch':
        return await handleInputTouch(command, browser);
      case 'recording_start':
        return await handleRecordingStart(command, browser);
      case 'recording_stop':
        return await handleRecordingStop(command, browser);
      case 'recording_restart':
        return await handleRecordingRestart(command, browser);
      case 'diff_snapshot':
        return await handleDiffSnapshot(command, browser);
      case 'diff_screenshot':
        return await handleDiffScreenshot(command, browser);
      case 'diff_url':
        return await handleDiffUrl(command, browser);
      default: {
        // TypeScript narrows to never here, but we handle it for safety
        const unknownCommand = command as { id: string; action: string };
        return errorResponse(unknownCommand.id, `Unknown action: ${unknownCommand.action}`);
      }
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return errorResponse(command.id, message);
  }
}

async function handleLaunch(
  command: Command & { action: 'launch' },
  browser: BrowserManager
): Promise<Response> {
  await browser.launch(command);
  return successResponse(command.id, { launched: true });
}

async function handleNavigate(
  command: NavigateCommand,
  browser: BrowserManager
): Promise<Response<NavigateData>> {
  const page = browser.getPage();

  // If headers are provided, set up scoped headers for this origin
  if (command.headers && Object.keys(command.headers).length > 0) {
    await browser.setScopedHeaders(command.url, command.headers);
  }

  await page.goto(command.url, {
    waitUntil: command.waitUntil ?? 'load',
  });

  return successResponse(command.id, {
    url: page.url(),
    title: await page.title(),
  });
}

async function handleClick(command: ClickCommand, browser: BrowserManager): Promise<Response> {
  // Support both refs (@e1) and regular selectors
  const locator = browser.getLocator(command.selector);

  try {
    // If --new-tab flag is set, get the href and open in a new tab
    if (command.newTab) {
      const fullUrl = await locator.evaluate((el) => {
        const href = el.getAttribute('href');
        // URL and document.baseURI are available in the browser context
        return href
          ? new (globalThis as any).URL(href, (globalThis as any).document.baseURI).toString()
          : '';
      });
      if (!fullUrl) {
        throw new Error(
          `Element '${command.selector}' does not have an href attribute. --new-tab only works on links.`
        );
      }

      await browser.newTab();
      const newPage = browser.getPage();
      await newPage.goto(fullUrl);

      return successResponse(command.id, {
        clicked: true,
        newTab: true,
        url: fullUrl,
      });
    }

    await locator.click({
      button: command.button,
      clickCount: command.clickCount,
      delay: command.delay,
    });
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }

  return successResponse(command.id, { clicked: true });
}

async function handleType(command: TypeCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);

  try {
    if (command.clear) {
      await locator.fill('');
    }

    await locator.pressSequentially(command.text, {
      delay: command.delay,
    });
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }

  return successResponse(command.id, { typed: true });
}

async function handlePress(command: PressCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();

  if (command.selector) {
    await page.press(command.selector, command.key);
  } else {
    await page.keyboard.press(command.key);
  }

  return successResponse(command.id, { pressed: true });
}

const ANNOTATION_OVERLAY_ID = '__agent_browser_annotations__';

async function removeAnnotationOverlay(page: Page): Promise<void> {
  await page
    .evaluate(
      `(() => { const el = document.getElementById(${JSON.stringify(ANNOTATION_OVERLAY_ID)}); if (el) el.remove(); })()`
    )
    .catch(() => {});
}

async function handleScreenshot(
  command: ScreenshotCommand,
  browser: BrowserManager
): Promise<Response<ScreenshotData>> {
  const page = browser.getPage();

  const options: Parameters<Page['screenshot']>[0] = {
    fullPage: command.fullPage,
    type: command.format ?? 'png',
  };

  if (command.format === 'jpeg' && command.quality !== undefined) {
    options.quality = command.quality;
  }

  let target: Page | ReturnType<Page['locator']> = page;
  if (command.selector) {
    target = browser.getLocator(command.selector);
  }

  let overlayInjected = false;

  try {
    let savePath = command.path;
    if (!savePath) {
      const ext = command.format === 'jpeg' ? 'jpg' : 'png';
      const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
      const random = Math.random().toString(36).substring(2, 8);
      const filename = `screenshot-${timestamp}-${random}.${ext}`;
      const screenshotDir = path.join(getAppDir(), 'tmp', 'screenshots');
      mkdirSync(screenshotDir, { recursive: true });
      savePath = path.join(screenshotDir, filename);
    }

    let annotations: Annotation[] | undefined;

    if (command.annotate) {
      const { refs } = await browser.getSnapshot({ interactive: true });

      const entries = Object.entries(refs);
      const results = await Promise.all(
        entries.map(async ([ref, data]): Promise<Annotation | null> => {
          try {
            const locator = browser.getLocatorFromRef(ref);
            if (!locator) return null;
            const box = await locator.boundingBox();
            if (!box || box.width === 0 || box.height === 0) return null;
            const num = parseInt(ref.replace('e', ''), 10);
            return {
              ref,
              number: num,
              role: data.role,
              name: data.name || undefined,
              box: {
                x: Math.round(box.x),
                y: Math.round(box.y),
                width: Math.round(box.width),
                height: Math.round(box.height),
              },
            };
          } catch {
            return null;
          }
        })
      );

      // When a selector is provided the screenshot is cropped to that element,
      // so filter to annotations that overlap the target and shift coordinates.
      let targetBox: { x: number; y: number; width: number; height: number } | null = null;
      if (command.selector) {
        const raw = await browser.getLocator(command.selector).boundingBox();
        if (raw) {
          targetBox = {
            x: Math.round(raw.x),
            y: Math.round(raw.y),
            width: Math.round(raw.width),
            height: Math.round(raw.height),
          };
        }
      }

      const filtered = results.filter((a): a is Annotation => a !== null);

      // Filter by selector overlap if needed, but keep viewport-relative coords
      // for overlay positioning. Coordinate shifting happens later for metadata only.
      let overlayItems: Annotation[];
      if (targetBox) {
        const tb = targetBox;
        overlayItems = filtered
          .filter((a) => {
            const ax2 = a.box.x + a.box.width;
            const ay2 = a.box.y + a.box.height;
            const bx2 = tb.x + tb.width;
            const by2 = tb.y + tb.height;
            return a.box.x < bx2 && ax2 > tb.x && a.box.y < by2 && ay2 > tb.y;
          })
          .sort((a, b) => a.number - b.number);
      } else {
        overlayItems = filtered.sort((a, b) => a.number - b.number);
      }

      if (overlayItems.length > 0) {
        const overlayData = overlayItems.map((a) => ({
          number: a.number,
          x: a.box.x,
          y: a.box.y,
          width: a.box.width,
          height: a.box.height,
        }));

        // Uses position:absolute with document-relative coords so labels render
        // correctly for both viewport and fullPage screenshots, and when the
        // screenshot is scoped to a selector element.
        await page.evaluate(`(() => {
          var items = ${JSON.stringify(overlayData)};
          var id = ${JSON.stringify(ANNOTATION_OVERLAY_ID)};
          var sx = window.scrollX || 0;
          var sy = window.scrollY || 0;
          var c = document.createElement('div');
          c.id = id;
          c.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;pointer-events:none;z-index:2147483647;';
          for (var i = 0; i < items.length; i++) {
            var it = items[i];
            var dx = it.x + sx;
            var dy = it.y + sy;
            var b = document.createElement('div');
            b.style.cssText = 'position:absolute;left:' + dx + 'px;top:' + dy + 'px;width:' + it.width + 'px;height:' + it.height + 'px;border:2px solid rgba(255,0,0,0.8);box-sizing:border-box;pointer-events:none;';
            var l = document.createElement('div');
            l.textContent = String(it.number);
            var labelTop = dy < 14 ? '2px' : '-14px';
            l.style.cssText = 'position:absolute;top:' + labelTop + ';left:-2px;background:rgba(255,0,0,0.9);color:#fff;font:bold 11px/14px monospace;padding:0 4px;border-radius:2px;white-space:nowrap;';
            b.appendChild(l);
            c.appendChild(b);
          }
          document.documentElement.appendChild(c);
        })()`);
        overlayInjected = true;
      }

      // Build returned annotation metadata with image-relative coordinates.
      // Selector: shift to target-element-relative.
      // fullPage: convert to document-relative (matching fullPage image origin).
      // Default: viewport-relative (unchanged).
      if (targetBox) {
        const tb = targetBox;
        annotations = overlayItems.map((a) => ({
          ...a,
          box: {
            x: a.box.x - tb.x,
            y: a.box.y - tb.y,
            width: a.box.width,
            height: a.box.height,
          },
        }));
      } else if (command.fullPage) {
        const scroll = (await page.evaluate(
          `({x: window.scrollX || 0, y: window.scrollY || 0})`
        )) as { x: number; y: number };
        annotations = overlayItems.map((a) => ({
          ...a,
          box: {
            x: a.box.x + scroll.x,
            y: a.box.y + scroll.y,
            width: a.box.width,
            height: a.box.height,
          },
        }));
      } else {
        annotations = overlayItems;
      }
    }

    await target.screenshot({ ...options, path: savePath });

    if (overlayInjected) {
      await removeAnnotationOverlay(page);
    }

    return successResponse(command.id, {
      path: savePath,
      ...(annotations && annotations.length > 0 ? { annotations } : {}),
    });
  } catch (error) {
    if (overlayInjected) {
      await removeAnnotationOverlay(page);
    }
    if (command.selector) {
      throw toAIFriendlyError(error, command.selector);
    }
    throw error;
  }
}

async function handleSnapshot(
  command: Command & {
    action: 'snapshot';
    interactive?: boolean;
    cursor?: boolean;
    maxDepth?: number;
    compact?: boolean;
    selector?: string;
  },
  browser: BrowserManager
): Promise<Response<SnapshotData>> {
  // Use enhanced snapshot with refs and optional filtering
  const { tree, refs } = await browser.getSnapshot({
    interactive: command.interactive,
    cursor: command.cursor,
    maxDepth: command.maxDepth,
    compact: command.compact,
    selector: command.selector,
  });

  // Simplify refs for output (just role and name)
  const simpleRefs: Record<string, { role: string; name?: string }> = {};
  for (const [ref, data] of Object.entries(refs)) {
    simpleRefs[ref] = { role: data.role, name: data.name };
  }

  return successResponse(command.id, {
    snapshot: tree || 'Empty page',
    refs: Object.keys(simpleRefs).length > 0 ? simpleRefs : undefined,
  });
}

async function handleEvaluate(
  command: EvaluateCommand,
  browser: BrowserManager
): Promise<Response<EvaluateData>> {
  const page = browser.getPage();

  // Evaluate the script directly as a string expression
  const result = await page.evaluate(command.script);

  return successResponse(command.id, { result });
}

async function handleWait(command: WaitCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();

  if (command.selector) {
    await page.waitForSelector(command.selector, {
      state: command.state ?? 'visible',
      timeout: command.timeout,
    });
  } else if (command.timeout) {
    await page.waitForTimeout(command.timeout);
  } else {
    // Default: wait for load state
    await page.waitForLoadState('load');
  }

  return successResponse(command.id, { waited: true });
}

async function handleScroll(command: ScrollCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();

  if (command.selector) {
    const element = browser.getLocator(command.selector);
    await element.scrollIntoViewIfNeeded();

    if (command.x !== undefined || command.y !== undefined) {
      await element.evaluate(
        (el, { x, y }) => {
          el.scrollBy(x ?? 0, y ?? 0);
        },
        { x: command.x, y: command.y }
      );
    }
  } else {
    // Scroll the page
    let deltaX = command.x ?? 0;
    let deltaY = command.y ?? 0;

    if (command.direction) {
      const amount = command.amount ?? 100;
      switch (command.direction) {
        case 'up':
          deltaY = -amount;
          break;
        case 'down':
          deltaY = amount;
          break;
        case 'left':
          deltaX = -amount;
          break;
        case 'right':
          deltaX = amount;
          break;
      }
    }

    await page.evaluate(`window.scrollBy(${deltaX}, ${deltaY})`);
  }

  return successResponse(command.id, { scrolled: true });
}

async function handleSelect(command: SelectCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const values = Array.isArray(command.values) ? command.values : [command.values];

  try {
    await locator.selectOption(values);
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }

  return successResponse(command.id, { selected: values });
}

async function handleHover(command: HoverCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  try {
    await locator.hover();
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }

  return successResponse(command.id, { hovered: true });
}

async function handleContent(
  command: ContentCommand,
  browser: BrowserManager
): Promise<Response<ContentData>> {
  const page = browser.getPage();

  let html: string;
  if (command.selector) {
    html = await page.locator(command.selector).innerHTML();
  } else {
    html = await page.content();
  }

  return successResponse(command.id, { html });
}

async function handleClose(
  command: Command & { action: 'close' },
  browser: BrowserManager
): Promise<Response> {
  await browser.close();
  return successResponse(command.id, { closed: true });
}

async function handleTabNew(
  command: TabNewCommand,
  browser: BrowserManager
): Promise<Response<TabNewData>> {
  const result = await browser.newTab();

  // Navigate to URL if provided (same pattern as handleNavigate)
  if (command.url) {
    const page = browser.getPage();
    await page.goto(command.url, { waitUntil: 'domcontentloaded' });
  }

  return successResponse(command.id, result);
}

async function handleTabList(
  command: Command & { action: 'tab_list' },
  browser: BrowserManager
): Promise<Response<TabListData>> {
  const tabs = await browser.listTabs();
  return successResponse(command.id, {
    tabs,
    active: browser.getActiveIndex(),
  });
}

async function handleTabSwitch(
  command: TabSwitchCommand,
  browser: BrowserManager
): Promise<Response<TabSwitchData>> {
  const result = await browser.switchTo(command.index);
  const page = browser.getPage();
  return successResponse(command.id, {
    ...result,
    title: await page.title(),
  });
}

async function handleTabClose(
  command: TabCloseCommand,
  browser: BrowserManager
): Promise<Response<TabCloseData>> {
  const result = await browser.closeTab(command.index);
  return successResponse(command.id, result);
}

async function handleWindowNew(
  command: WindowNewCommand,
  browser: BrowserManager
): Promise<Response<TabNewData>> {
  const result = await browser.newWindow(command.viewport);
  return successResponse(command.id, result);
}

// New handlers for enhanced Playwright parity

async function handleFill(command: FillCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  try {
    await locator.fill(command.value);
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }
  return successResponse(command.id, { filled: true });
}

async function handleCheck(command: CheckCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  try {
    await locator.check();
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }
  return successResponse(command.id, { checked: true });
}

async function handleUncheck(command: UncheckCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  try {
    await locator.uncheck();
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }
  return successResponse(command.id, { unchecked: true });
}

async function handleUpload(command: UploadCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const files = Array.isArray(command.files) ? command.files : [command.files];
  try {
    await locator.setInputFiles(files);
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }
  return successResponse(command.id, { uploaded: files });
}

async function handleDoubleClick(
  command: DoubleClickCommand,
  browser: BrowserManager
): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  try {
    await locator.dblclick();
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }
  return successResponse(command.id, { clicked: true });
}

async function handleFocus(command: FocusCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  try {
    await locator.focus();
  } catch (error) {
    throw toAIFriendlyError(error, command.selector);
  }
  return successResponse(command.id, { focused: true });
}

async function handleDrag(command: DragCommand, browser: BrowserManager): Promise<Response> {
  const frame = browser.getFrame();
  await frame.dragAndDrop(command.source, command.target);
  return successResponse(command.id, { dragged: true });
}

async function handleFrame(command: FrameCommand, browser: BrowserManager): Promise<Response> {
  await browser.switchToFrame({
    selector: command.selector,
    name: command.name,
    url: command.url,
  });
  return successResponse(command.id, { switched: true });
}

async function handleMainFrame(
  command: Command & { action: 'mainframe' },
  browser: BrowserManager
): Promise<Response> {
  browser.switchToMainFrame();
  return successResponse(command.id, { switched: true });
}

async function handleGetByRole(
  command: GetByRoleCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByRole(command.role as any, { name: command.name, exact: command.exact });

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'fill':
      await locator.fill(command.value ?? '');
      return successResponse(command.id, { filled: true });
    case 'check':
      await locator.check();
      return successResponse(command.id, { checked: true });
    case 'hover':
      await locator.hover();
      return successResponse(command.id, { hovered: true });
  }
}

async function handleGetByText(
  command: GetByTextCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByText(command.text, { exact: command.exact });

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'hover':
      await locator.hover();
      return successResponse(command.id, { hovered: true });
  }
}

async function handleGetByLabel(
  command: GetByLabelCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByLabel(command.label, { exact: command.exact });

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'fill':
      await locator.fill(command.value ?? '');
      return successResponse(command.id, { filled: true });
    case 'check':
      await locator.check();
      return successResponse(command.id, { checked: true });
  }
}

async function handleGetByPlaceholder(
  command: GetByPlaceholderCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByPlaceholder(command.placeholder, { exact: command.exact });

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'fill':
      await locator.fill(command.value ?? '');
      return successResponse(command.id, { filled: true });
  }
}

async function handleCookiesGet(
  command: Command & { action: 'cookies_get'; urls?: string[] },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const context = page.context();
  const cookies = await context.cookies(command.urls);
  return successResponse(command.id, { cookies });
}

async function handleCookiesSet(
  command: CookiesSetCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const context = page.context();
  // Auto-fill URL for cookies that don't have domain/path/url set
  const pageUrl = page.url();
  const cookies = command.cookies.map((cookie) => {
    if (!cookie.url && !cookie.domain && !cookie.path) {
      return { ...cookie, url: pageUrl };
    }
    return cookie;
  });
  await context.addCookies(cookies);
  return successResponse(command.id, { set: true });
}

async function handleCookiesClear(
  command: Command & { action: 'cookies_clear' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const context = page.context();
  await context.clearCookies();
  return successResponse(command.id, { cleared: true });
}

async function handleStorageGet(
  command: StorageGetCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const storageType = command.type === 'local' ? 'localStorage' : 'sessionStorage';

  if (command.key) {
    const value = await page.evaluate(`${storageType}.getItem(${JSON.stringify(command.key)})`);
    return successResponse(command.id, { key: command.key, value });
  } else {
    const data = await page.evaluate(`
      (() => {
        const storage = ${storageType};
        const result = {};
        for (let i = 0; i < storage.length; i++) {
          const key = storage.key(i);
          if (key) result[key] = storage.getItem(key);
        }
        return result;
      })()
    `);
    return successResponse(command.id, { data });
  }
}

async function handleStorageSet(
  command: StorageSetCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const storageType = command.type === 'local' ? 'localStorage' : 'sessionStorage';

  await page.evaluate(
    `${storageType}.setItem(${JSON.stringify(command.key)}, ${JSON.stringify(command.value)})`
  );
  return successResponse(command.id, { set: true });
}

async function handleStorageClear(
  command: StorageClearCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const storageType = command.type === 'local' ? 'localStorage' : 'sessionStorage';

  await page.evaluate(`${storageType}.clear()`);
  return successResponse(command.id, { cleared: true });
}

async function handleDialog(command: DialogCommand, browser: BrowserManager): Promise<Response> {
  browser.setDialogHandler(command.response, command.promptText);
  return successResponse(command.id, { handler: 'set', response: command.response });
}

async function handlePdf(command: PdfCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  await page.pdf({
    path: command.path,
    format: command.format ?? 'Letter',
  });
  return successResponse(command.id, { path: command.path });
}

// Network & Request handlers

async function handleRoute(command: RouteCommand, browser: BrowserManager): Promise<Response> {
  await browser.addRoute(command.url, {
    response: command.response,
    abort: command.abort,
  });
  return successResponse(command.id, { routed: command.url });
}

async function handleUnroute(
  command: Command & { action: 'unroute'; url?: string },
  browser: BrowserManager
): Promise<Response> {
  await browser.removeRoute(command.url);
  return successResponse(command.id, { unrouted: command.url ?? 'all' });
}

async function handleRequests(
  command: RequestsCommand,
  browser: BrowserManager
): Promise<Response> {
  if (command.clear) {
    browser.clearRequests();
    return successResponse(command.id, { cleared: true });
  }

  // Start tracking if not already
  browser.startRequestTracking();

  const requests = browser.getRequests(command.filter);
  return successResponse(command.id, { requests });
}

async function handleDownload(
  command: DownloadCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = browser.getLocator(command.selector);

  const [download] = await Promise.all([page.waitForEvent('download'), locator.click()]);

  await download.saveAs(command.path);
  return successResponse(command.id, {
    path: command.path,
    suggestedFilename: download.suggestedFilename(),
  });
}

async function handleGeolocation(
  command: GeolocationCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.setGeolocation(command.latitude, command.longitude, command.accuracy);
  return successResponse(command.id, {
    latitude: command.latitude,
    longitude: command.longitude,
  });
}

async function handlePermissions(
  command: PermissionsCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.setPermissions(command.permissions, command.grant);
  return successResponse(command.id, {
    permissions: command.permissions,
    granted: command.grant,
  });
}

async function handleViewport(
  command: ViewportCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.setViewport(command.width, command.height);
  return successResponse(command.id, {
    width: command.width,
    height: command.height,
  });
}

async function handleUserAgent(
  command: Command & { action: 'useragent'; userAgent: string },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const context = page.context();
  // Note: Can't change user agent after context is created, but we can for new pages
  return successResponse(command.id, {
    note: 'User agent can only be set at launch time. Use device command instead.',
  });
}

async function handleDevice(command: DeviceCommand, browser: BrowserManager): Promise<Response> {
  const device = browser.getDevice(command.device);
  if (!device) {
    const available = browser.listDevices().slice(0, 10).join(', ');
    throw new Error(`Unknown device: ${command.device}. Available: ${available}...`);
  }

  // Apply device viewport
  await browser.setViewport(device.viewport.width, device.viewport.height);

  // Apply or clear device scale factor
  if (device.deviceScaleFactor && device.deviceScaleFactor !== 1) {
    // Apply device scale factor for HiDPI/retina displays
    await browser.setDeviceScaleFactor(
      device.deviceScaleFactor,
      device.viewport.width,
      device.viewport.height,
      device.isMobile ?? false
    );
  } else {
    // Clear device scale factor override to restore default (1x)
    try {
      await browser.clearDeviceMetricsOverride();
    } catch {
      // Ignore error if override was never set
    }
  }

  return successResponse(command.id, {
    device: command.device,
    viewport: device.viewport,
    userAgent: device.userAgent,
    deviceScaleFactor: device.deviceScaleFactor,
  });
}

async function handleBack(
  command: Command & { action: 'back' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.goBack();
  return successResponse(command.id, { url: page.url() });
}

async function handleForward(
  command: Command & { action: 'forward' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.goForward();
  return successResponse(command.id, { url: page.url() });
}

async function handleReload(
  command: Command & { action: 'reload' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.reload();
  return successResponse(command.id, { url: page.url() });
}

async function handleUrl(
  command: Command & { action: 'url' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  return successResponse(command.id, { url: page.url() });
}

async function handleTitle(
  command: Command & { action: 'title' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const title = await page.title();
  return successResponse(command.id, { title });
}

async function handleGetAttribute(
  command: GetAttributeCommand,
  browser: BrowserManager
): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const value = await locator.getAttribute(command.attribute);
  return successResponse(command.id, { attribute: command.attribute, value });
}

async function handleGetText(command: GetTextCommand, browser: BrowserManager): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const text = await locator.textContent();
  return successResponse(command.id, { text });
}

async function handleIsVisible(
  command: IsVisibleCommand,
  browser: BrowserManager
): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const visible = await locator.isVisible();
  return successResponse(command.id, { visible });
}

async function handleIsEnabled(
  command: IsEnabledCommand,
  browser: BrowserManager
): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const enabled = await locator.isEnabled();
  return successResponse(command.id, { enabled });
}

async function handleIsChecked(
  command: IsCheckedCommand,
  browser: BrowserManager
): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const checked = await locator.isChecked();
  return successResponse(command.id, { checked });
}

async function handleCount(command: CountCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  const count = await page.locator(command.selector).count();
  return successResponse(command.id, { count });
}

async function handleBoundingBox(
  command: BoundingBoxCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const box = await page.locator(command.selector).boundingBox();
  return successResponse(command.id, { box });
}

async function handleStyles(
  command: StylesCommand,
  browser: BrowserManager
): Promise<Response<StylesData>> {
  const page = browser.getPage();

  // Shared extraction logic as a string to be eval'd in browser context
  const extractStylesScript = `(function(el) {
    const s = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    return {
      tag: el.tagName.toLowerCase(),
      text: el.innerText?.trim().slice(0, 80) || null,
      box: {
        x: Math.round(r.x),
        y: Math.round(r.y),
        width: Math.round(r.width),
        height: Math.round(r.height),
      },
      styles: {
        fontSize: s.fontSize,
        fontWeight: s.fontWeight,
        fontFamily: s.fontFamily.split(',')[0].trim().replace(/"/g, ''),
        color: s.color,
        backgroundColor: s.backgroundColor,
        borderRadius: s.borderRadius,
        border: s.border !== 'none' && s.borderWidth !== '0px' ? s.border : null,
        boxShadow: s.boxShadow !== 'none' ? s.boxShadow : null,
        padding: s.padding,
      },
    };
  })`;

  // Check if it's a ref - single element
  if (browser.isRef(command.selector)) {
    const locator = browser.getLocator(command.selector);
    const element = (await locator.evaluate((el, script) => {
      const fn = eval(script);
      return fn(el);
    }, extractStylesScript)) as StylesData['elements'][0];
    return successResponse(command.id, { elements: [element] });
  }

  // CSS selector - can match multiple elements
  const elements = (await page.$$eval(
    command.selector,
    (els, script) => {
      const fn = eval(script);
      return els.map((el) => fn(el));
    },
    extractStylesScript
  )) as StylesData['elements'];

  return successResponse(command.id, { elements });
}

// Advanced handlers

async function handleVideoStart(
  command: Command & { action: 'video_start'; path: string },
  browser: BrowserManager
): Promise<Response> {
  // Video recording requires context-level setup at launch
  // For now, return a note about this limitation
  return successResponse(command.id, {
    note: 'Video recording must be enabled at browser launch. Use --video flag when starting.',
    path: command.path,
  });
}

async function handleVideoStop(
  command: Command & { action: 'video_stop' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const video = page.video();
  if (video) {
    const path = await video.path();
    return successResponse(command.id, { path });
  }
  return successResponse(command.id, { note: 'No video recording active' });
}

async function handleTraceStart(
  command: TraceStartCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.startTracing({
    screenshots: command.screenshots,
    snapshots: command.snapshots,
  });
  return successResponse(command.id, { started: true });
}

async function handleTraceStop(
  command: TraceStopCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.stopTracing(command.path);
  return successResponse(
    command.id,
    command.path ? { path: command.path } : { traceStopped: true }
  );
}

async function handleProfilerStart(
  command: ProfilerStartCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.startProfiling({ categories: command.categories });
  return successResponse(command.id, { started: true });
}

async function handleProfilerStop(
  command: ProfilerStopCommand,
  browser: BrowserManager
): Promise<Response> {
  let outputPath = command.path;
  if (!outputPath) {
    const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
    const random = Math.random().toString(36).substring(2, 8);
    const filename = `profile-${timestamp}-${random}.json`;
    const profileDir = path.join(getAppDir(), 'tmp', 'profiles');
    mkdirSync(profileDir, { recursive: true });
    outputPath = path.join(profileDir, filename);
  }
  const result = await browser.stopProfiling(outputPath);
  return successResponse(command.id, result);
}

async function handleHarStart(
  command: Command & { action: 'har_start' },
  browser: BrowserManager
): Promise<Response> {
  await browser.startHarRecording();
  browser.startRequestTracking();
  return successResponse(command.id, { started: true });
}

async function handleHarStop(command: HarStopCommand, browser: BrowserManager): Promise<Response> {
  // HAR recording is handled at context level
  // For now, we save tracked requests as a simplified HAR-like format
  const requests = browser.getRequests();
  return successResponse(command.id, {
    path: command.path,
    requestCount: requests.length,
  });
}

async function handleStateSave(
  command: StorageStateSaveCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.saveStorageState(command.path);
  return successResponse(command.id, { path: command.path });
}

async function handleStateLoad(
  command: Command & { action: 'state_load'; path: string },
  browser: BrowserManager
): Promise<Response> {
  if (browser.isLaunched()) {
    return errorResponse(
      command.id,
      'Cannot load state while browser is running. Close browser first, then relaunch with loaded state.'
    );
  }

  if (!fs.existsSync(command.path)) {
    return errorResponse(command.id, `State file not found: ${command.path}`);
  }

  await browser.launch({
    id: command.id,
    action: 'launch',
    headless: true,
    autoStateFilePath: command.path,
  });

  return successResponse(command.id, {
    loaded: true,
    path: command.path,
  });
}

async function handleStateList(command: StateListCommand): Promise<Response> {
  const sessionsDir = getSessionsDir();
  const files = listStateFiles();

  if (files.length === 0) {
    return successResponse(command.id, { files: [], directory: sessionsDir });
  }

  const stateFiles = files
    .map((filename) => {
      const filepath = path.join(sessionsDir, filename);
      const stats = fs.statSync(filepath);

      let encrypted = false;
      try {
        const content = fs.readFileSync(filepath, 'utf-8');
        const parsed = JSON.parse(content);
        encrypted = isEncryptedPayload(parsed);
      } catch {
        // Ignore parse errors
      }

      return {
        filename,
        path: filepath,
        size: stats.size,
        modified: stats.mtime.toISOString(),
        encrypted,
      };
    })
    .sort((a, b) => new Date(b.modified).getTime() - new Date(a.modified).getTime());

  return successResponse(command.id, { files: stateFiles, directory: sessionsDir });
}

async function handleStateClear(command: StateClearCommand): Promise<Response> {
  const sessionsDir = getSessionsDir();

  if (command.sessionName && !isValidSessionName(command.sessionName)) {
    return errorResponse(
      command.id,
      'Invalid session name. Use only letters, numbers, dashes, and underscores.'
    );
  }

  const files = listStateFiles();
  if (files.length === 0) {
    return successResponse(command.id, { cleared: 0, deleted: [] });
  }

  const deleted: string[] = [];

  if (command.all) {
    for (const file of files) {
      fs.unlinkSync(path.join(sessionsDir, file));
      deleted.push(file);
    }
  } else if (command.sessionName) {
    for (const file of files) {
      if (file.startsWith(`${command.sessionName}-`)) {
        fs.unlinkSync(path.join(sessionsDir, file));
        deleted.push(file);
      }
    }
  }

  return successResponse(command.id, { cleared: deleted.length, deleted });
}

async function handleStateShow(command: StateShowCommand): Promise<Response> {
  const sessionsDir = getSessionsDir();

  const baseName = command.filename.replace(/\.json$/, '');
  if (!command.filename.endsWith('.json') || !isValidSessionName(baseName)) {
    return errorResponse(
      command.id,
      'Invalid filename. Use only letters, numbers, dashes, and underscores (with .json extension).'
    );
  }

  const filepath = path.join(sessionsDir, command.filename);

  if (!fs.existsSync(filepath)) {
    return errorResponse(command.id, `State file not found: ${command.filename}`);
  }

  try {
    const { data: state, wasEncrypted } = readStateFile(filepath);
    const stats = fs.statSync(filepath);

    const stateObj = state as {
      cookies?: Array<{ domain: string }>;
      origins?: unknown[];
    };
    const cookies = stateObj.cookies?.length || 0;
    const origins = stateObj.origins?.length || 0;
    const domains = [...new Set((stateObj.cookies || []).map((c) => c.domain))];

    return successResponse(command.id, {
      filename: command.filename,
      path: filepath,
      size: stats.size,
      modified: stats.mtime.toISOString(),
      encrypted: wasEncrypted,
      summary: {
        cookies,
        origins,
        domains,
      },
      state,
    });
  } catch (e) {
    return errorResponse(command.id, `Failed to parse state file: ${(e as Error).message}`);
  }
}

async function handleStateClean(command: StateCleanCommand): Promise<Response> {
  const deleted = cleanupExpiredStates(command.days);
  const keptCount = listStateFiles().length;

  return successResponse(command.id, {
    cleaned: deleted.length,
    deleted,
    keptCount,
    days: command.days,
  });
}

async function handleStateRename(command: StateRenameCommand): Promise<Response> {
  const sessionsDir = getSessionsDir();

  if (!isValidSessionName(command.oldName) || !isValidSessionName(command.newName)) {
    return errorResponse(
      command.id,
      'Invalid name. Use only letters, numbers, dashes, and underscores.'
    );
  }

  const oldPath = path.join(sessionsDir, `${command.oldName}.json`);
  const newPath = path.join(sessionsDir, `${command.newName}.json`);

  if (!fs.existsSync(oldPath)) {
    return errorResponse(command.id, `State file not found: ${command.oldName}.json`);
  }

  if (fs.existsSync(newPath)) {
    return errorResponse(command.id, `Destination already exists: ${command.newName}.json`);
  }

  fs.renameSync(oldPath, newPath);

  return successResponse(command.id, {
    renamed: true,
    oldName: `${command.oldName}.json`,
    newName: `${command.newName}.json`,
    path: newPath,
  });
}

async function handleConsole(command: ConsoleCommand, browser: BrowserManager): Promise<Response> {
  if (command.clear) {
    browser.clearConsoleMessages();
    return successResponse(command.id, { cleared: true });
  }

  const messages = browser.getConsoleMessages();
  return successResponse(command.id, { messages });
}

async function handleErrors(command: ErrorsCommand, browser: BrowserManager): Promise<Response> {
  if (command.clear) {
    browser.clearPageErrors();
    return successResponse(command.id, { cleared: true });
  }

  const errors = browser.getPageErrors();
  return successResponse(command.id, { errors });
}

async function handleKeyboard(
  command: KeyboardCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.keyboard.press(command.keys);
  return successResponse(command.id, { pressed: command.keys });
}

async function handleWheel(command: WheelCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();

  if (command.selector) {
    const element = page.locator(command.selector);
    await element.hover();
  }

  await page.mouse.wheel(command.deltaX ?? 0, command.deltaY ?? 0);
  return successResponse(command.id, { scrolled: true });
}

async function handleTap(command: TapCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  await page.tap(command.selector);
  return successResponse(command.id, { tapped: true });
}

async function handleClipboard(
  command: ClipboardCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();

  switch (command.operation) {
    case 'copy':
      await page.keyboard.press('Control+c');
      return successResponse(command.id, { copied: true });
    case 'paste':
      await page.keyboard.press('Control+v');
      return successResponse(command.id, { pasted: true });
    case 'read':
      const text = await page.evaluate('navigator.clipboard.readText()');
      return successResponse(command.id, { text });
    default:
      return errorResponse(command.id, 'Unknown clipboard operation');
  }
}

async function handleHighlight(
  command: HighlightCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.locator(command.selector).highlight();
  return successResponse(command.id, { highlighted: true });
}

async function handleClear(command: ClearCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  await page.locator(command.selector).clear();
  return successResponse(command.id, { cleared: true });
}

async function handleSelectAll(
  command: SelectAllCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.locator(command.selector).selectText();
  return successResponse(command.id, { selected: true });
}

async function handleInnerText(
  command: InnerTextCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const text = await page.locator(command.selector).innerText();
  return successResponse(command.id, { text });
}

async function handleInnerHtml(
  command: InnerHtmlCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const html = await page.locator(command.selector).innerHTML();
  return successResponse(command.id, { html });
}

async function handleInputValue(
  command: InputValueCommand,
  browser: BrowserManager
): Promise<Response> {
  const locator = browser.getLocator(command.selector);
  const value = await locator.inputValue();
  return successResponse(command.id, { value });
}

async function handleSetValue(
  command: SetValueCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.locator(command.selector).fill(command.value);
  return successResponse(command.id, { set: true });
}

async function handleDispatch(
  command: DispatchEventCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.locator(command.selector).dispatchEvent(command.event, command.eventInit);
  return successResponse(command.id, { dispatched: command.event });
}

async function handleEvalHandle(
  command: Command & { action: 'evalhandle'; script: string },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const handle = await page.evaluateHandle(command.script);
  const result = await handle.jsonValue().catch(() => 'Handle (non-serializable)');
  return successResponse(command.id, { result });
}

async function handleExpose(
  command: Command & { action: 'expose'; name: string },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.exposeFunction(command.name, () => {
    // Exposed function - can be extended
    return `Function ${command.name} called`;
  });
  return successResponse(command.id, { exposed: command.name });
}

async function handleAddScript(
  command: AddScriptCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();

  if (command.content) {
    await page.addScriptTag({ content: command.content });
  } else if (command.url) {
    await page.addScriptTag({ url: command.url });
  }

  return successResponse(command.id, { added: true });
}

async function handleAddStyle(
  command: AddStyleCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();

  if (command.content) {
    await page.addStyleTag({ content: command.content });
  } else if (command.url) {
    await page.addStyleTag({ url: command.url });
  }

  return successResponse(command.id, { added: true });
}

async function handleEmulateMedia(
  command: EmulateMediaCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.emulateMedia({
    media: command.media,
    colorScheme: command.colorScheme,
    reducedMotion: command.reducedMotion,
    forcedColors: command.forcedColors,
  });
  return successResponse(command.id, { emulated: true });
}

async function handleOffline(command: OfflineCommand, browser: BrowserManager): Promise<Response> {
  await browser.setOffline(command.offline);
  return successResponse(command.id, { offline: command.offline });
}

async function handleHeaders(command: HeadersCommand, browser: BrowserManager): Promise<Response> {
  await browser.setExtraHeaders(command.headers);
  return successResponse(command.id, { set: true });
}

async function handlePause(
  command: Command & { action: 'pause' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.pause();
  return successResponse(command.id, { paused: true });
}

async function handleGetByAltText(
  command: GetByAltTextCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByAltText(command.text, { exact: command.exact });

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'hover':
      await locator.hover();
      return successResponse(command.id, { hovered: true });
  }
}

async function handleGetByTitle(
  command: GetByTitleCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByTitle(command.text, { exact: command.exact });

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'hover':
      await locator.hover();
      return successResponse(command.id, { hovered: true });
  }
}

async function handleGetByTestId(
  command: GetByTestIdCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const locator = page.getByTestId(command.testId);

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'fill':
      await locator.fill(command.value ?? '');
      return successResponse(command.id, { filled: true });
    case 'check':
      await locator.check();
      return successResponse(command.id, { checked: true });
    case 'hover':
      await locator.hover();
      return successResponse(command.id, { hovered: true });
  }
}

async function handleNth(command: NthCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  const base = page.locator(command.selector);
  const locator = command.index === -1 ? base.last() : base.nth(command.index);

  switch (command.subaction) {
    case 'click':
      await locator.click();
      return successResponse(command.id, { clicked: true });
    case 'fill':
      await locator.fill(command.value ?? '');
      return successResponse(command.id, { filled: true });
    case 'check':
      await locator.check();
      return successResponse(command.id, { checked: true });
    case 'hover':
      await locator.hover();
      return successResponse(command.id, { hovered: true });
    case 'text':
      const text = await locator.textContent();
      return successResponse(command.id, { text });
  }
}

async function handleWaitForUrl(
  command: WaitForUrlCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.waitForURL(command.url, { timeout: command.timeout });
  return successResponse(command.id, { url: page.url() });
}

async function handleWaitForLoadState(
  command: WaitForLoadStateCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.waitForLoadState(command.state, { timeout: command.timeout });
  return successResponse(command.id, { state: command.state });
}

async function handleSetContent(
  command: SetContentCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.setContent(command.html);
  return successResponse(command.id, { set: true });
}

async function handleTimezone(
  command: TimezoneCommand,
  browser: BrowserManager
): Promise<Response> {
  // Timezone must be set at context level before navigation
  // This is a limitation - it sets for the current context
  const page = browser.getPage();
  await page.context().setGeolocation({ latitude: 0, longitude: 0 }); // Trigger context awareness
  return successResponse(command.id, {
    note: 'Timezone must be set at browser launch. Use --timezone flag.',
    timezone: command.timezone,
  });
}

async function handleLocale(command: LocaleCommand, browser: BrowserManager): Promise<Response> {
  // Locale must be set at context creation
  return successResponse(command.id, {
    note: 'Locale must be set at browser launch. Use --locale flag.',
    locale: command.locale,
  });
}

async function handleCredentials(
  command: HttpCredentialsCommand,
  browser: BrowserManager
): Promise<Response> {
  const context = browser.getPage().context();
  await context.setHTTPCredentials({
    username: command.username,
    password: command.password,
  });
  return successResponse(command.id, { set: true });
}

async function handleMouseMove(
  command: MouseMoveCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.mouse.move(command.x, command.y);
  return successResponse(command.id, { moved: true, x: command.x, y: command.y });
}

async function handleMouseDown(
  command: MouseDownCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.mouse.down({ button: command.button ?? 'left' });
  return successResponse(command.id, { down: true });
}

async function handleMouseUp(command: MouseUpCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  await page.mouse.up({ button: command.button ?? 'left' });
  return successResponse(command.id, { up: true });
}

async function handleBringToFront(
  command: Command & { action: 'bringtofront' },
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.bringToFront();
  return successResponse(command.id, { focused: true });
}

async function handleWaitForFunction(
  command: WaitForFunctionCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.waitForFunction(command.expression, { timeout: command.timeout });
  return successResponse(command.id, { waited: true });
}

async function handleScrollIntoView(
  command: ScrollIntoViewCommand,
  browser: BrowserManager
): Promise<Response> {
  await browser.getLocator(command.selector).scrollIntoViewIfNeeded();
  return successResponse(command.id, { scrolled: true });
}

async function handleAddInitScript(
  command: AddInitScriptCommand,
  browser: BrowserManager
): Promise<Response> {
  const context = browser.getPage().context();
  await context.addInitScript(command.script);
  return successResponse(command.id, { added: true });
}

async function handleKeyDown(command: KeyDownCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  await page.keyboard.down(command.key);
  return successResponse(command.id, { down: true, key: command.key });
}

async function handleKeyUp(command: KeyUpCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();
  await page.keyboard.up(command.key);
  return successResponse(command.id, { up: true, key: command.key });
}

async function handleInsertText(
  command: InsertTextCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  await page.keyboard.insertText(command.text);
  return successResponse(command.id, { inserted: true });
}

async function handleMultiSelect(
  command: MultiSelectCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const selected = await page.locator(command.selector).selectOption(command.values);
  return successResponse(command.id, { selected });
}

async function handleWaitForDownload(
  command: WaitForDownloadCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const download = await page.waitForEvent('download', { timeout: command.timeout });

  let filePath: string;
  if (command.path) {
    filePath = command.path;
    await download.saveAs(filePath);
  } else {
    filePath = (await download.path()) || download.suggestedFilename();
  }

  return successResponse(command.id, {
    path: filePath,
    filename: download.suggestedFilename(),
    url: download.url(),
  });
}

async function handleResponseBody(
  command: ResponseBodyCommand,
  browser: BrowserManager
): Promise<Response> {
  const page = browser.getPage();
  const response = await page.waitForResponse((resp) => resp.url().includes(command.url), {
    timeout: command.timeout,
  });

  const body = await response.text();
  let parsed: unknown = body;

  try {
    parsed = JSON.parse(body);
  } catch {
    // Keep as string if not JSON
  }

  return successResponse(command.id, {
    url: response.url(),
    status: response.status(),
    body: parsed,
  });
}

// Screencast and input injection handlers

async function handleScreencastStart(
  command: ScreencastStartCommand,
  browser: BrowserManager
): Promise<Response<ScreencastStartData>> {
  if (!screencastFrameCallback) {
    throw new Error('Screencast frame callback not set. Start the streaming server first.');
  }

  await browser.startScreencast(screencastFrameCallback, {
    format: command.format,
    quality: command.quality,
    maxWidth: command.maxWidth,
    maxHeight: command.maxHeight,
    everyNthFrame: command.everyNthFrame,
  });

  return successResponse(command.id, {
    started: true,
    format: command.format ?? 'jpeg',
    quality: command.quality ?? 80,
  });
}

async function handleScreencastStop(
  command: ScreencastStopCommand,
  browser: BrowserManager
): Promise<Response<ScreencastStopData>> {
  await browser.stopScreencast();
  return successResponse(command.id, { stopped: true });
}

async function handleInputMouse(
  command: InputMouseCommand,
  browser: BrowserManager
): Promise<Response<InputEventData>> {
  await browser.injectMouseEvent({
    type: command.type,
    x: command.x,
    y: command.y,
    button: command.button,
    clickCount: command.clickCount,
    deltaX: command.deltaX,
    deltaY: command.deltaY,
    modifiers: command.modifiers,
  });
  return successResponse(command.id, { injected: true });
}

async function handleInputKeyboard(
  command: InputKeyboardCommand,
  browser: BrowserManager
): Promise<Response<InputEventData>> {
  await browser.injectKeyboardEvent({
    type: command.type,
    key: command.key,
    code: command.code,
    text: command.text,
    modifiers: command.modifiers,
  });
  return successResponse(command.id, { injected: true });
}

async function handleInputTouch(
  command: InputTouchCommand,
  browser: BrowserManager
): Promise<Response<InputEventData>> {
  await browser.injectTouchEvent({
    type: command.type,
    touchPoints: command.touchPoints,
    modifiers: command.modifiers,
  });
  return successResponse(command.id, { injected: true });
}

// Recording handlers (Playwright native video recording)

async function handleRecordingStart(
  command: RecordingStartCommand,
  browser: BrowserManager
): Promise<Response<RecordingStartData>> {
  await browser.startRecording(command.path, command.url);
  return successResponse(command.id, {
    started: true,
    path: command.path,
  });
}

async function handleRecordingStop(
  command: RecordingStopCommand,
  browser: BrowserManager
): Promise<Response<RecordingStopData>> {
  const result = await browser.stopRecording();
  return successResponse(command.id, result);
}

async function handleRecordingRestart(
  command: RecordingRestartCommand,
  browser: BrowserManager
): Promise<Response<RecordingRestartData>> {
  const result = await browser.restartRecording(command.path, command.url);
  return successResponse(command.id, {
    started: true,
    path: command.path,
    previousPath: result.previousPath,
    stopped: result.stopped,
  });
}

// Diff handlers

async function handleDiffSnapshot(
  command: DiffSnapshotCommand,
  browser: BrowserManager
): Promise<Response> {
  let before: string;

  if (command.baseline) {
    try {
      before = fs.readFileSync(command.baseline, 'utf-8');
    } catch {
      return errorResponse(command.id, `Cannot read baseline file: ${command.baseline}`);
    }
  } else {
    before = browser.getLastSnapshot();
    if (!before) {
      return errorResponse(
        command.id,
        'No previous snapshot in this session. Take a snapshot first, or use --baseline <file>.'
      );
    }
  }

  const page = browser.getPage();
  const { tree } = await getEnhancedSnapshot(page, {
    selector: command.selector,
    compact: command.compact,
    maxDepth: command.maxDepth,
  });

  const after = tree || 'Empty page';
  const result = diffSnapshots(before, after);
  browser.setLastSnapshot(after);
  return successResponse(command.id, result);
}

async function handleDiffScreenshot(
  command: DiffScreenshotCommand,
  browser: BrowserManager
): Promise<Response> {
  if (!fs.existsSync(command.baseline)) {
    return errorResponse(command.id, `Baseline file not found: ${command.baseline}`);
  }

  const page = browser.getPage();
  let screenshotBuffer: Buffer;
  if (command.selector) {
    const locator = browser.getLocatorFromRef(command.selector) || page.locator(command.selector);
    screenshotBuffer = await locator.screenshot({ type: 'png' });
  } else {
    screenshotBuffer = await page.screenshot({ fullPage: command.fullPage, type: 'png' });
  }

  const baselineBuffer = fs.readFileSync(command.baseline);
  const ext = path.extname(command.baseline).toLowerCase();
  const baselineMime = ext === '.jpg' || ext === '.jpeg' ? 'image/jpeg' : 'image/png';

  const result = await diffScreenshots(page.context(), baselineBuffer, screenshotBuffer, {
    threshold: command.threshold,
    outputPath: command.output,
    baselineMime,
  });

  return successResponse(command.id, result);
}

async function handleDiffUrl(command: DiffUrlCommand, browser: BrowserManager): Promise<Response> {
  const page = browser.getPage();

  const waitUntil = command.waitUntil ?? 'load';
  const snapshotOpts = {
    selector: command.selector,
    compact: command.compact,
    maxDepth: command.maxDepth,
  };

  // Capture state of url1
  await page.goto(command.url1, { waitUntil });
  const { tree: tree1 } = await getEnhancedSnapshot(page, snapshotOpts);
  const snapshot1 = tree1 || 'Empty page';
  let screenshot1: Buffer | undefined;
  if (command.screenshot) {
    screenshot1 = await page.screenshot({ fullPage: command.fullPage, type: 'png' });
  }

  // Capture state of url2
  await page.goto(command.url2, { waitUntil });
  const { tree: tree2 } = await getEnhancedSnapshot(page, snapshotOpts);
  const snapshot2 = tree2 || 'Empty page';

  const snapshotDiff = diffSnapshots(snapshot1, snapshot2);

  const result: DiffUrlData = { snapshot: snapshotDiff };

  if (command.screenshot && screenshot1) {
    const screenshot2 = await page.screenshot({ fullPage: command.fullPage, type: 'png' });
    result.screenshot = await diffScreenshots(page.context(), screenshot1, screenshot2, {});
  }

  return successResponse(command.id, result);
}
