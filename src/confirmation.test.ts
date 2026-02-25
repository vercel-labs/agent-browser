import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { requestConfirmation, getAndRemovePending } from './confirmation.js';

describe('confirmation', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe('requestConfirmation', () => {
    it('should return a confirmation ID', () => {
      const result = requestConfirmation('evaluate', 'eval', 'Evaluate JS', { script: 'test' });
      expect(result.confirmationId).toBeTruthy();
      expect(result.confirmationId).toMatch(/^c_[0-9a-f]{16}$/);
    });

    it('should generate unique IDs', () => {
      const r1 = requestConfirmation('evaluate', 'eval', 'desc', {});
      const r2 = requestConfirmation('click', 'click', 'desc', {});
      expect(r1.confirmationId).not.toBe(r2.confirmationId);
    });
  });

  describe('getAndRemovePending', () => {
    it('should retrieve and remove a pending confirmation', () => {
      const { confirmationId } = requestConfirmation('evaluate', 'eval', 'desc', {
        action: 'evaluate',
        script: 'test',
      });

      const entry = getAndRemovePending(confirmationId);
      expect(entry).not.toBeNull();
      expect(entry!.action).toBe('evaluate');
      expect(entry!.command).toEqual({ action: 'evaluate', script: 'test' });
    });

    it('should return null on second retrieval (already removed)', () => {
      const { confirmationId } = requestConfirmation('evaluate', 'eval', 'desc', {});
      getAndRemovePending(confirmationId);
      expect(getAndRemovePending(confirmationId)).toBeNull();
    });

    it('should return null for non-existent ID', () => {
      expect(getAndRemovePending('c_nonexistent')).toBeNull();
    });

    it('should auto-deny after 60 seconds', () => {
      const { confirmationId } = requestConfirmation('evaluate', 'eval', 'desc', {});

      vi.advanceTimersByTime(60_000);

      expect(getAndRemovePending(confirmationId)).toBeNull();
    });

    it('should still be retrievable before 60 second timeout', () => {
      const { confirmationId } = requestConfirmation('evaluate', 'eval', 'desc', {});

      vi.advanceTimersByTime(59_999);

      const entry = getAndRemovePending(confirmationId);
      expect(entry).not.toBeNull();
    });
  });
});
