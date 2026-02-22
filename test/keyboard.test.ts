import { describe, it, expect } from 'vitest';
import { parseCommand } from '../src/protocol.js';

describe('keyboard command validation', () => {
  it('accepts keyboard type with text', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'type', text: 'hello' })
    );
    expect(result.success).toBe(true);
  });

  it('accepts keyboard insertText with text', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'insertText', text: 'hello' })
    );
    expect(result.success).toBe(true);
  });

  it('accepts keyboard press with keys', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'press', keys: 'Enter' })
    );
    expect(result.success).toBe(true);
  });

  it('accepts legacy keyboard (no subaction) with keys', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', keys: 'Enter' })
    );
    expect(result.success).toBe(true);
  });

  it('rejects keyboard type without text', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'type' })
    );
    expect(result.success).toBe(false);
    if (!result.success) expect(result.error).toContain('requires text');
  });

  it('rejects keyboard insertText without text', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'insertText' })
    );
    expect(result.success).toBe(false);
    if (!result.success) expect(result.error).toContain('requires text');
  });

  it('rejects keyboard press without keys', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'press' })
    );
    expect(result.success).toBe(false);
    if (!result.success) expect(result.error).toContain('requires keys');
  });

  it('rejects legacy keyboard (no subaction) without keys', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard' })
    );
    expect(result.success).toBe(false);
    if (!result.success) expect(result.error).toContain('requires keys');
  });

  it('accepts keyboard type with delay option', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'keyboard', subaction: 'type', text: 'hello', delay: 50 })
    );
    expect(result.success).toBe(true);
  });
});
