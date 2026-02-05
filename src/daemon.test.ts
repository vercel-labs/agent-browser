import { describe, it, expect } from 'vitest';
import { parseViewportEnv } from './daemon.js';

describe('parseViewportEnv', () => {
  it('should parse "WIDTHxHEIGHT" format', () => {
    expect(parseViewportEnv('1920x1080')).toEqual({ width: 1920, height: 1080 });
  });

  it('should parse "WIDTH,HEIGHT" format', () => {
    expect(parseViewportEnv('1920,1080')).toEqual({ width: 1920, height: 1080 });
  });

  it('should parse default viewport size', () => {
    expect(parseViewportEnv('1280x720')).toEqual({ width: 1280, height: 720 });
  });

  it('should return undefined for invalid format', () => {
    expect(parseViewportEnv('invalid')).toBeUndefined();
  });

  it('should return undefined for partial format', () => {
    expect(parseViewportEnv('1920x')).toBeUndefined();
    expect(parseViewportEnv('x1080')).toBeUndefined();
  });

  it('should return undefined for empty string', () => {
    expect(parseViewportEnv('')).toBeUndefined();
  });

  it('should return undefined for undefined', () => {
    expect(parseViewportEnv(undefined)).toBeUndefined();
  });

  it('should reject non-numeric values', () => {
    expect(parseViewportEnv('abcxdef')).toBeUndefined();
  });

  it('should reject negative numbers', () => {
    expect(parseViewportEnv('-1920x1080')).toBeUndefined();
  });

  it('should reject formats with extra text', () => {
    expect(parseViewportEnv('1920x1080px')).toBeUndefined();
    expect(parseViewportEnv('px1920x1080')).toBeUndefined();
  });

  it('should reject zero width', () => {
    expect(parseViewportEnv('0x1080')).toBeUndefined();
  });

  it('should reject zero height', () => {
    expect(parseViewportEnv('1920x0')).toBeUndefined();
  });

  it('should reject both zero', () => {
    expect(parseViewportEnv('0x0')).toBeUndefined();
  });
});
