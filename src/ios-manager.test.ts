import { describe, it, expect, vi, beforeEach } from 'vitest';
import { IOSManager } from './ios-manager.js';

// Mock node-simctl
vi.mock('node-simctl', () => {
  return {
    Simctl: class MockSimctl {
      async getDevices() {
        return {
          'iOS 18.0': [
            {
              name: 'iPhone 16 Pro',
              udid: 'TEST-UDID-1234',
              state: 'Shutdown',
              isAvailable: true,
            },
            {
              name: 'iPhone 16',
              udid: 'TEST-UDID-5678',
              state: 'Booted',
              isAvailable: true,
            },
            {
              name: 'iPad Pro',
              udid: 'TEST-UDID-IPAD',
              state: 'Shutdown',
              isAvailable: true,
            },
          ],
        };
      }
    },
  };
});

describe('IOSManager', () => {
  let manager: IOSManager;

  beforeEach(() => {
    manager = new IOSManager();
  });

  describe('listDevices', () => {
    it('should list available iOS simulators', async () => {
      const devices = await manager.listDevices();

      expect(devices).toHaveLength(3);
      expect(devices[0]).toEqual({
        name: 'iPhone 16 Pro',
        udid: 'TEST-UDID-1234',
        state: 'Shutdown',
        runtime: 'iOS 18.0',
        isAvailable: true,
        isRealDevice: false,
      });
    });

    it('should include runtime version for each device', async () => {
      const devices = await manager.listDevices();

      devices.forEach((device) => {
        expect(device.runtime).toBe('iOS 18.0');
      });
    });
  });

  describe('isLaunched', () => {
    it('should return false when browser is not launched', () => {
      expect(manager.isLaunched()).toBe(false);
    });
  });

  describe('getRefData', () => {
    it('should return null for unknown refs', () => {
      // Access private method via bracket notation for testing
      const result = (manager as any).getRefData('@e99');
      expect(result).toBeNull();
    });

    it('should handle @-prefixed refs', () => {
      // Set up a ref in the refMap
      (manager as any).refMap = {
        e1: { selector: 'button', role: 'button', name: 'Submit' },
      };

      const result = (manager as any).getRefData('@e1');
      expect(result).toEqual({ selector: 'button', role: 'button', name: 'Submit' });
    });

    it('should handle ref= prefixed refs', () => {
      (manager as any).refMap = {
        e2: { selector: 'a', role: 'link', name: 'Learn more' },
      };

      const result = (manager as any).getRefData('ref=e2');
      expect(result).toEqual({ selector: 'a', role: 'link', name: 'Learn more' });
    });

    it('should handle bare ref names', () => {
      (manager as any).refMap = {
        e3: { selector: 'input', role: 'textbox', name: 'Email' },
      };

      const result = (manager as any).getRefData('e3');
      expect(result).toEqual({ selector: 'input', role: 'textbox', name: 'Email' });
    });
  });
});

describe('IOSManager integration', () => {
  // These tests require Appium and iOS Simulator to be available
  // They are skipped by default and can be run manually
  describe.skip('with real simulator', () => {
    let manager: IOSManager;

    beforeEach(() => {
      // Use real implementation for integration tests
      vi.resetModules();
      manager = new IOSManager();
    });

    it('should launch Safari and navigate', async () => {
      await manager.launch({ device: 'iPhone 16 Pro' });
      expect(manager.isLaunched()).toBe(true);

      const result = await manager.navigate('https://example.com');
      expect(result.url).toContain('example.com');
      expect(result.title).toBe('Example Domain');

      await manager.close();
    }, 120000);

    it('should take screenshots', async () => {
      await manager.launch({ device: 'iPhone 16 Pro' });
      await manager.navigate('https://example.com');

      const result = await manager.screenshot();
      expect(result.base64).toBeDefined();
      expect(result.base64?.length).toBeGreaterThan(1000);

      await manager.close();
    }, 120000);

    it('should generate snapshots with refs', async () => {
      await manager.launch({ device: 'iPhone 16 Pro' });
      await manager.navigate('https://example.com');

      const snapshot = await manager.getSnapshot();
      expect(snapshot.tree).toContain('link');
      expect(snapshot.tree).toContain('[ref=e1]');
      expect(snapshot.refs.e1).toBeDefined();
      expect(snapshot.refs.e1.role).toBe('link');

      await manager.close();
    }, 120000);
  });
});
