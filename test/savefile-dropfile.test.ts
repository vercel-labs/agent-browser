import { describe, it, expect } from 'vitest';
import { parseCommand } from '../src/protocol.js';

describe('savefile command validation', () => {
  it('accepts savefile with outputPath only', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'savefile', outputPath: '/tmp/out.png' })
    );
    expect(result.success).toBe(true);
  });

  it('accepts savefile with outputPath and selector', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'savefile', outputPath: '/tmp/out.png', selector: 'img.hero' })
    );
    expect(result.success).toBe(true);
  });

  it('rejects savefile without outputPath', () => {
    const result = parseCommand(JSON.stringify({ id: '1', action: 'savefile' }));
    expect(result.success).toBe(false);
  });

  it('rejects savefile with empty outputPath', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'savefile', outputPath: '' })
    );
    expect(result.success).toBe(false);
  });
});

describe('dropfile command validation', () => {
  it('accepts dropfile with required fields', () => {
    const result = parseCommand(
      JSON.stringify({
        id: '1',
        action: 'dropfile',
        selector: '.drop-zone',
        filePath: '/tmp/file.pdf',
      })
    );
    expect(result.success).toBe(true);
  });

  it('accepts dropfile with optional fileName and mimeType', () => {
    const result = parseCommand(
      JSON.stringify({
        id: '1',
        action: 'dropfile',
        selector: '.drop-zone',
        filePath: '/tmp/file.pdf',
        fileName: 'custom.pdf',
        mimeType: 'application/pdf',
      })
    );
    expect(result.success).toBe(true);
  });

  it('rejects dropfile without selector', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'dropfile', filePath: '/tmp/file.pdf' })
    );
    expect(result.success).toBe(false);
  });

  it('rejects dropfile without filePath', () => {
    const result = parseCommand(
      JSON.stringify({ id: '1', action: 'dropfile', selector: '.drop-zone' })
    );
    expect(result.success).toBe(false);
  });

  it('rejects dropfile with empty selector', () => {
    const result = parseCommand(
      JSON.stringify({
        id: '1',
        action: 'dropfile',
        selector: '',
        filePath: '/tmp/file.pdf',
      })
    );
    expect(result.success).toBe(false);
  });

  it('rejects dropfile with empty filePath', () => {
    const result = parseCommand(
      JSON.stringify({
        id: '1',
        action: 'dropfile',
        selector: '.drop-zone',
        filePath: '',
      })
    );
    expect(result.success).toBe(false);
  });
});
