// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from 'vitest';
import { MUTE_STORAGE_KEY, readMuted, readStoredMuted, writeMuted } from '../src/mute';

describe('mute persistence', () => {
  beforeEach(() => localStorage.clear());

  it('defaults to unmuted and round-trips through localStorage', () => {
    expect(readMuted()).toBe(false);
    expect(readStoredMuted()).toBeNull();
    expect(writeMuted(true)).toBe(true);
    expect(localStorage.getItem(MUTE_STORAGE_KEY)).toBe('1');
    expect(readMuted()).toBe(true);
    writeMuted(false);
    expect(readStoredMuted()).toBe(false);
  });

  it('supports a custom key', () => {
    writeMuted(true, 'k2');
    expect(readMuted('k2')).toBe(true);
    expect(readMuted()).toBe(false);
  });

  it('survives storage that throws (private mode, blocked site data)', () => {
    const broken = {
      getItem: () => {
        throw new Error('SecurityError');
      },
      setItem: () => {
        throw new Error('QuotaExceededError');
      },
    };
    expect(readMuted(MUTE_STORAGE_KEY, broken)).toBe(false);
    expect(readStoredMuted(MUTE_STORAGE_KEY, broken)).toBeNull();
    expect(writeMuted(true, MUTE_STORAGE_KEY, broken)).toBe(false);
  });

  it('works without any storage', () => {
    expect(readMuted(MUTE_STORAGE_KEY, null)).toBe(false);
    expect(writeMuted(true, MUTE_STORAGE_KEY, null)).toBe(false);
  });
});
