import { describe, it, expect } from 'vitest';
import { parseCommand } from '../src/protocol.js';
import { IOSManager } from '../src/ios-manager.js';

// Helper to create command JSON string
const cmd = (obj: object) => JSON.stringify(obj);

describe('iOS provider', () => {
  describe('IOSManager instantiation', () => {
    it('should be instantiable', () => {
      const manager = new IOSManager();
      expect(manager).toBeDefined();
      expect(manager).toBeInstanceOf(IOSManager);
    });
  });

  describe('iosDevice schema parsing', () => {
    it('should preserve iosDevice in navigate command', () => {
      const result = parseCommand(
        cmd({
          id: 'test-1',
          action: 'navigate',
          url: 'https://example.com',
          iosDevice: 'iPhone 16 Pro',
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect((result.command as any).iosDevice).toBe('iPhone 16 Pro');
      }
    });

    it('should accept navigate without iosDevice', () => {
      const result = parseCommand(
        cmd({
          id: 'test-2',
          action: 'navigate',
          url: 'https://example.com',
        })
      );
      expect(result.success).toBe(true);
      if (result.success) {
        expect((result.command as any).iosDevice).toBeUndefined();
      }
    });
  });
});
