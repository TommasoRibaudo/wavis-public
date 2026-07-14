import { describe, expect, it } from 'vitest';

import { CodecPolicyConfig, DEFAULT_CODEC_POLICY, getDefaultCodecPolicy } from '../codec-policy';

describe('codec-policy', () => {
  it('defaults to vp8 simulcast (W4 decision: vp8sim)', () => {
    expect(new CodecPolicyConfig().defaultPrimaryCodec).toBe('vp8');
    expect(getDefaultCodecPolicy()).toEqual(DEFAULT_CODEC_POLICY);
    expect(DEFAULT_CODEC_POLICY).toEqual({ primary: 'vp8', backup: null, simulcast: true });
  });

  it('allows vp9 as primary: backup=vp8, simulcast=false', () => {
    expect(getDefaultCodecPolicy({ defaultPrimaryCodec: 'vp9' })).toEqual({
      primary: 'vp9',
      backup: 'vp8',
      simulcast: false,
    });
  });

  it('allows vp8 as primary: backup=null, simulcast=true', () => {
    expect(getDefaultCodecPolicy({ defaultPrimaryCodec: 'vp8' })).toEqual({
      primary: 'vp8',
      backup: null,
      simulcast: true,
    });
  });

  it('allows av1 as primary: backup=vp9, simulcast=false', () => {
    expect(() => new CodecPolicyConfig({ defaultPrimaryCodec: 'av1' })).not.toThrow();
    expect(getDefaultCodecPolicy({ defaultPrimaryCodec: 'av1' })).toEqual({
      primary: 'av1',
      backup: 'vp9',
      simulcast: false,
    });
  });
});
