import { describe, it, expect } from 'vitest';
import { injectSessionId, stripSessionId } from './inspect-server.js';

describe('injectSessionId', () => {
  it('should inject sessionId into a command', () => {
    const input = '{"id":1,"method":"DOM.getDocument"}';
    const result = JSON.parse(injectSessionId(input, 'abc123'));
    expect(result.sessionId).toBe('abc123');
    expect(result.method).toBe('DOM.getDocument');
    expect(result.id).toBe(1);
  });

  it('should inject sessionId into an empty object', () => {
    const result = JSON.parse(injectSessionId('{}', 'abc'));
    expect(result.sessionId).toBe('abc');
  });
});

describe('stripSessionId', () => {
  it('should remove sessionId from a message', () => {
    const input = '{"id":1,"result":{},"sessionId":"abc123"}';
    const result = JSON.parse(stripSessionId(input));
    expect(result.sessionId).toBeUndefined();
    expect(result.id).toBe(1);
  });
});

describe('inject then strip roundtrip', () => {
  it('should return the original message after inject + strip', () => {
    const input = '{"id":42,"method":"Runtime.evaluate"}';
    const injected = injectSessionId(input, 'sess1');
    const stripped = stripSessionId(injected);
    expect(JSON.parse(stripped)).toEqual(JSON.parse(input));
  });
});
