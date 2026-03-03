import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import {
  getActionCategory,
  checkPolicy,
  loadPolicyFile,
  describeAction,
  KNOWN_CATEGORIES,
  type ActionPolicy,
} from './action-policy.js';

describe('action-policy', () => {
  describe('getActionCategory', () => {
    it('should return correct category for known actions', () => {
      expect(getActionCategory('navigate')).toBe('navigate');
      expect(getActionCategory('click')).toBe('click');
      expect(getActionCategory('fill')).toBe('fill');
      expect(getActionCategory('evaluate')).toBe('eval');
      expect(getActionCategory('download')).toBe('download');
      expect(getActionCategory('upload')).toBe('upload');
      expect(getActionCategory('snapshot')).toBe('snapshot');
      expect(getActionCategory('scroll')).toBe('scroll');
      expect(getActionCategory('wait')).toBe('wait');
      expect(getActionCategory('gettext')).toBe('get');
      expect(getActionCategory('route')).toBe('network');
      expect(getActionCategory('state_save')).toBe('state');
      expect(getActionCategory('hover')).toBe('interact');
    });

    it('should return _internal for internal actions', () => {
      expect(getActionCategory('launch')).toBe('_internal');
      expect(getActionCategory('close')).toBe('_internal');
      expect(getActionCategory('session')).toBe('_internal');
      expect(getActionCategory('auth_save')).toBe('_internal');
      expect(getActionCategory('confirm')).toBe('_internal');
    });

    it('should return eval for security-sensitive actions', () => {
      expect(getActionCategory('setcontent')).toBe('eval');
      expect(getActionCategory('expose')).toBe('eval');
      expect(getActionCategory('addstyle')).toBe('eval');
    });

    it('should return unknown for unrecognized actions', () => {
      expect(getActionCategory('nonexistent')).toBe('unknown');
      expect(getActionCategory('')).toBe('unknown');
    });

    it('should return get for semantic locator actions', () => {
      expect(getActionCategory('getbyrole')).toBe('get');
      expect(getActionCategory('getbytext')).toBe('get');
      expect(getActionCategory('getbylabel')).toBe('get');
    });
  });

  describe('checkPolicy', () => {
    it('should always allow internal actions regardless of policy', () => {
      const denyAll: ActionPolicy = { default: 'deny' };
      expect(checkPolicy('launch', denyAll, new Set())).toBe('allow');
      expect(checkPolicy('close', denyAll, new Set())).toBe('allow');
      expect(checkPolicy('session', denyAll, new Set())).toBe('allow');
    });

    it('should allow all when no policy and no confirm categories', () => {
      expect(checkPolicy('navigate', null, new Set())).toBe('allow');
      expect(checkPolicy('click', null, new Set())).toBe('allow');
      expect(checkPolicy('evaluate', null, new Set())).toBe('allow');
    });

    it('should deny actions in explicit deny list', () => {
      const policy: ActionPolicy = { default: 'allow', deny: ['eval', 'download'] };
      expect(checkPolicy('evaluate', policy, new Set())).toBe('deny');
      expect(checkPolicy('download', policy, new Set())).toBe('deny');
      expect(checkPolicy('click', policy, new Set())).toBe('allow');
    });

    it('should allow actions in explicit allow list with deny default', () => {
      const policy: ActionPolicy = { default: 'deny', allow: ['navigate', 'snapshot'] };
      expect(checkPolicy('navigate', policy, new Set())).toBe('allow');
      expect(checkPolicy('snapshot', policy, new Set())).toBe('allow');
      expect(checkPolicy('click', policy, new Set())).toBe('deny');
    });

    it('should return confirm for actions in confirm categories', () => {
      expect(checkPolicy('evaluate', null, new Set(['eval']))).toBe('confirm');
      expect(checkPolicy('download', null, new Set(['download']))).toBe('confirm');
    });

    it('should deny over confirm when action is in deny list', () => {
      const policy: ActionPolicy = { default: 'allow', deny: ['eval'] };
      expect(checkPolicy('evaluate', policy, new Set(['eval']))).toBe('deny');
    });

    it('should use default policy for unknown categories', () => {
      const denyPolicy: ActionPolicy = { default: 'deny' };
      const allowPolicy: ActionPolicy = { default: 'allow' };
      expect(checkPolicy('nonexistent', denyPolicy, new Set())).toBe('deny');
      expect(checkPolicy('nonexistent', allowPolicy, new Set())).toBe('allow');
    });
  });

  describe('loadPolicyFile', () => {
    let tempDir: string;

    beforeEach(() => {
      tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'action-policy-test-'));
    });

    afterEach(() => {
      fs.rmSync(tempDir, { recursive: true, force: true });
    });

    it('should load a valid allow-default policy', () => {
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, JSON.stringify({ default: 'allow', deny: ['eval'] }));
      const policy = loadPolicyFile(policyPath);
      expect(policy.default).toBe('allow');
      expect(policy.deny).toEqual(['eval']);
    });

    it('should load a valid deny-default policy', () => {
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(
        policyPath,
        JSON.stringify({ default: 'deny', allow: ['navigate', 'snapshot'] })
      );
      const policy = loadPolicyFile(policyPath);
      expect(policy.default).toBe('deny');
      expect(policy.allow).toEqual(['navigate', 'snapshot']);
    });

    it('should throw on invalid default value', () => {
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, JSON.stringify({ default: 'maybe' }));
      expect(() => loadPolicyFile(policyPath)).toThrow('must be "allow" or "deny"');
    });

    it('should throw on missing file', () => {
      expect(() => loadPolicyFile(path.join(tempDir, 'missing.json'))).toThrow();
    });

    it('should throw on invalid JSON', () => {
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, 'not json');
      expect(() => loadPolicyFile(policyPath)).toThrow();
    });

    it('should warn on unrecognized category names', () => {
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(
        policyPath,
        JSON.stringify({ default: 'allow', deny: ['eval', 'typo_category'] })
      );
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
      const policy = loadPolicyFile(policyPath);
      expect(policy.default).toBe('allow');
      expect(warnSpy).toHaveBeenCalledWith(
        expect.stringContaining('unrecognized action category "typo_category"')
      );
      warnSpy.mockRestore();
    });

    it('should not warn on valid category names', () => {
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(
        policyPath,
        JSON.stringify({ default: 'deny', allow: ['navigate', 'snapshot', 'get'] })
      );
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
      loadPolicyFile(policyPath);
      expect(warnSpy).not.toHaveBeenCalled();
      warnSpy.mockRestore();
    });
  });

  describe('describeAction', () => {
    it('should describe navigate actions', () => {
      expect(describeAction('navigate', { url: 'https://example.com' })).toBe(
        'Navigate to https://example.com'
      );
    });

    it('should describe eval actions with truncation', () => {
      const longScript = 'a'.repeat(200);
      const desc = describeAction('evaluate', { script: longScript });
      expect(desc).toContain('Evaluate JavaScript:');
      expect(desc.length).toBeLessThan(200);
    });

    it('should describe click actions', () => {
      expect(describeAction('click', { selector: '#btn' })).toBe('Click #btn');
    });

    it('should describe dblclick actions', () => {
      expect(describeAction('dblclick', { selector: '#btn' })).toBe('Double-click #btn');
    });

    it('should describe tap actions', () => {
      expect(describeAction('tap', { selector: '#btn' })).toBe('Tap #btn');
    });

    it('should describe fill actions', () => {
      expect(describeAction('fill', { selector: '#input' })).toBe('Fill #input');
    });

    it('should use fallback for unknown actions', () => {
      const desc = describeAction('scroll', {});
      expect(desc).toContain('scroll');
    });
  });
});
