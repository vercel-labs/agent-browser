/**
 * iOS command execution - mirrors actions.ts but for iOS Safari via Appium.
 * Provides 1:1 command parity where possible.
 */

import type { IOSManager } from './ios-manager.js';
import type { Command, Response } from './types.js';

function successResponse<T>(id: string, data: T): Response<T> {
  return { id, success: true, data };
}

function errorResponse(id: string, error: string): Response {
  return { id, success: false, error };
}

/**
 * Execute a command on the iOS manager
 */
export async function executeIOSCommand(command: Command, manager: IOSManager): Promise<Response> {
  const { id, action } = command;

  try {
    switch (action) {
      case 'launch': {
        const cmd = command as any;
        await manager.launch({
          device: cmd.device,
          udid: cmd.udid,
        });
        const info = manager.getDeviceInfo();
        return successResponse(id, {
          launched: true,
          device: info?.name ?? 'iOS Simulator',
          udid: info?.udid,
        });
      }

      case 'navigate': {
        const cmd = command as any;
        const result = await manager.navigate(cmd.url);
        return successResponse(id, result);
      }

      case 'click': {
        const cmd = command as any;
        await manager.click(cmd.selector);
        return successResponse(id, { clicked: true });
      }

      case 'tap': {
        const cmd = command as any;
        await manager.tap(cmd.selector);
        return successResponse(id, { tapped: true });
      }

      case 'type': {
        const cmd = command as any;
        await manager.type(cmd.selector, cmd.text, {
          delay: cmd.delay,
          clear: cmd.clear,
        });
        return successResponse(id, { typed: true });
      }

      case 'fill': {
        const cmd = command as any;
        await manager.fill(cmd.selector, cmd.value);
        return successResponse(id, { filled: true });
      }

      case 'screenshot': {
        const cmd = command as any;
        const result = await manager.screenshot({
          path: cmd.path,
          fullPage: cmd.fullPage,
        });
        return successResponse(id, result);
      }

      case 'snapshot': {
        const cmd = command as any;
        const result = await manager.getSnapshot({
          interactive: cmd.interactive,
        });
        return successResponse(id, { snapshot: result.tree, refs: result.refs });
      }

      case 'scroll': {
        const cmd = command as any;
        await manager.scroll({
          selector: cmd.selector,
          x: cmd.x,
          y: cmd.y,
          direction: cmd.direction,
          amount: cmd.amount,
        });
        return successResponse(id, { scrolled: true });
      }

      case 'swipe': {
        const cmd = command as any;
        await manager.swipe(cmd.direction, { distance: cmd.distance });
        return successResponse(id, { swiped: true });
      }

      case 'evaluate': {
        const cmd = command as any;
        const result = await manager.evaluate(cmd.script, ...(cmd.args ?? []));
        return successResponse(id, { result });
      }

      case 'wait': {
        const cmd = command as any;
        await manager.wait({
          selector: cmd.selector,
          timeout: cmd.timeout,
          state: cmd.state,
        });
        return successResponse(id, { waited: true });
      }

      case 'press': {
        const cmd = command as any;
        await manager.press(cmd.key);
        return successResponse(id, { pressed: true });
      }

      case 'hover': {
        const cmd = command as any;
        await manager.hover(cmd.selector);
        return successResponse(id, { hovered: true });
      }

      case 'content': {
        const cmd = command as any;
        const html = await manager.getContent(cmd.selector);
        return successResponse(id, { html });
      }

      case 'gettext': {
        const cmd = command as any;
        const text = await manager.getText(cmd.selector);
        return successResponse(id, { text });
      }

      case 'getattribute': {
        const cmd = command as any;
        const value = await manager.getAttribute(cmd.selector, cmd.attribute);
        return successResponse(id, { value });
      }

      case 'isvisible': {
        const cmd = command as any;
        const visible = await manager.isVisible(cmd.selector);
        return successResponse(id, { visible });
      }

      case 'isenabled': {
        const cmd = command as any;
        const enabled = await manager.isEnabled(cmd.selector);
        return successResponse(id, { enabled });
      }

      case 'url': {
        const url = await manager.getUrl();
        return successResponse(id, { url });
      }

      case 'title': {
        const title = await manager.getTitle();
        return successResponse(id, { title });
      }

      case 'back': {
        await manager.goBack();
        return successResponse(id, { navigated: 'back' });
      }

      case 'forward': {
        await manager.goForward();
        return successResponse(id, { navigated: 'forward' });
      }

      case 'reload': {
        await manager.reload();
        return successResponse(id, { reloaded: true });
      }

      case 'select': {
        const cmd = command as any;
        await manager.select(cmd.selector, cmd.values);
        return successResponse(id, { selected: true });
      }

      case 'check': {
        const cmd = command as any;
        await manager.check(cmd.selector);
        return successResponse(id, { checked: true });
      }

      case 'uncheck': {
        const cmd = command as any;
        await manager.uncheck(cmd.selector);
        return successResponse(id, { unchecked: true });
      }

      case 'focus': {
        const cmd = command as any;
        await manager.focus(cmd.selector);
        return successResponse(id, { focused: true });
      }

      case 'clear': {
        const cmd = command as any;
        await manager.clear(cmd.selector);
        return successResponse(id, { cleared: true });
      }

      case 'count': {
        const cmd = command as any;
        const count = await manager.count(cmd.selector);
        return successResponse(id, { count });
      }

      case 'boundingbox': {
        const cmd = command as any;
        const box = await manager.getBoundingBox(cmd.selector);
        return successResponse(id, { box });
      }

      case 'close': {
        await manager.close();
        return successResponse(id, { closed: true });
      }

      // iOS-specific: device list
      case 'device_list': {
        const devices = await manager.listDevices();
        return successResponse(id, { devices });
      }

      // Commands that don't apply to iOS Safari
      case 'tab_new':
      case 'tab_list':
      case 'tab_switch':
      case 'tab_close':
      case 'window_new':
        return errorResponse(
          id,
          `Command '${action}' is not supported on iOS Safari. Mobile Safari does not support programmatic tab management.`
        );

      case 'pdf':
        return errorResponse(id, 'PDF generation is not supported on iOS Safari.');

      case 'screencast_start':
      case 'screencast_stop':
        return errorResponse(id, 'Screencast is not supported on iOS (requires CDP).');

      case 'recording_start':
      case 'recording_stop':
      case 'recording_restart':
        return errorResponse(id, 'Video recording is not yet supported on iOS.');

      default:
        return errorResponse(id, `Unknown or unsupported iOS command: ${action}`);
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return errorResponse(id, message);
  }
}
