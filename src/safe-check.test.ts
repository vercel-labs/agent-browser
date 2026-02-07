import { describe, it, expect, vi } from 'vitest';
import { safeCheckAction } from './actions.js';

/**
 * Tests for safeCheckAction - fix for issue #335
 * Ensures check/uncheck does not hang on hidden checkbox elements
 * (common in Element UI, Ant Design, Vuetify, etc.)
 */
describe('safeCheckAction', () => {
  it('should call check() normally when element is visible', async () => {
    const locator = {
      check: vi.fn().mockResolvedValue(undefined),
      uncheck: vi.fn().mockResolvedValue(undefined),
    } as any;

    await safeCheckAction(locator, 'check');
    expect(locator.check).toHaveBeenCalledWith({ timeout: 5000 });
  });

  it('should call uncheck() normally when element is visible', async () => {
    const locator = {
      check: vi.fn().mockResolvedValue(undefined),
      uncheck: vi.fn().mockResolvedValue(undefined),
    } as any;

    await safeCheckAction(locator, 'uncheck');
    expect(locator.uncheck).toHaveBeenCalledWith({ timeout: 5000 });
  });

  it('should retry with force:true when element is not visible (check)', async () => {
    const locator = {
      check: vi
        .fn()
        .mockRejectedValueOnce(new Error('Element is not visible'))
        .mockResolvedValueOnce(undefined),
      uncheck: vi.fn(),
    } as any;

    await safeCheckAction(locator, 'check');
    expect(locator.check).toHaveBeenCalledTimes(2);
    expect(locator.check).toHaveBeenLastCalledWith({ force: true });
  });

  it('should retry with force:true when element is not visible (uncheck)', async () => {
    const locator = {
      check: vi.fn(),
      uncheck: vi
        .fn()
        .mockRejectedValueOnce(new Error('Element is not visible'))
        .mockResolvedValueOnce(undefined),
    } as any;

    await safeCheckAction(locator, 'uncheck');
    expect(locator.uncheck).toHaveBeenCalledTimes(2);
    expect(locator.uncheck).toHaveBeenLastCalledWith({ force: true });
  });

  it('should retry with force:true on timeout (hidden checkbox)', async () => {
    const locator = {
      check: vi
        .fn()
        .mockRejectedValueOnce(new Error('Timeout 5000ms exceeded waiting for element'))
        .mockResolvedValueOnce(undefined),
      uncheck: vi.fn(),
    } as any;

    await safeCheckAction(locator, 'check');
    expect(locator.check).toHaveBeenCalledTimes(2);
    expect(locator.check).toHaveBeenLastCalledWith({ force: true });
  });

  it('should retry with force:true when element is hidden', async () => {
    const locator = {
      check: vi
        .fn()
        .mockRejectedValueOnce(new Error('element is hidden'))
        .mockResolvedValueOnce(undefined),
      uncheck: vi.fn(),
    } as any;

    await safeCheckAction(locator, 'check');
    expect(locator.check).toHaveBeenCalledTimes(2);
    expect(locator.check).toHaveBeenLastCalledWith({ force: true });
  });

  it('should retry with force:true on "waiting for" error', async () => {
    const locator = {
      check: vi
        .fn()
        .mockRejectedValueOnce(new Error('waiting for locator to be visible'))
        .mockResolvedValueOnce(undefined),
      uncheck: vi.fn(),
    } as any;

    await safeCheckAction(locator, 'check');
    expect(locator.check).toHaveBeenCalledTimes(2);
    expect(locator.check).toHaveBeenLastCalledWith({ force: true });
  });

  it('should rethrow non-visibility errors', async () => {
    const locator = {
      check: vi.fn().mockRejectedValue(new Error('strict mode violation')),
      uncheck: vi.fn(),
    } as any;

    await expect(safeCheckAction(locator, 'check')).rejects.toThrow('strict mode violation');
    expect(locator.check).toHaveBeenCalledTimes(1);
  });

  it('should rethrow unknown errors for uncheck', async () => {
    const locator = {
      check: vi.fn(),
      uncheck: vi.fn().mockRejectedValue(new Error('some other error')),
    } as any;

    await expect(safeCheckAction(locator, 'uncheck')).rejects.toThrow('some other error');
    expect(locator.uncheck).toHaveBeenCalledTimes(1);
  });
});
