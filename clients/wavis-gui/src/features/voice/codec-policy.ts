export interface CodecPolicy {
  primary: 'vp9' | 'vp8' | 'av1';
  backup: 'vp9' | 'vp8' | null;
  simulcast: boolean;
}

export interface CodecPolicyConfigOptions {
  defaultPrimaryCodec?: 'vp9' | 'vp8' | 'av1';
}

export const DEFAULT_CODEC_POLICY: CodecPolicy = {
  primary: 'vp9',
  backup: 'vp8',
  simulcast: false,
};

export class CodecPolicyConfig {
  readonly defaultPrimaryCodec: 'vp9' | 'vp8' | 'av1';

  constructor(config: CodecPolicyConfigOptions = {}) {
    this.defaultPrimaryCodec = config.defaultPrimaryCodec ?? DEFAULT_CODEC_POLICY.primary;
  }
}

export function getDefaultCodecPolicy(config: CodecPolicyConfigOptions = {}): CodecPolicy {
  const validated = new CodecPolicyConfig(config);
  const primary = validated.defaultPrimaryCodec;
  return {
    primary,
    backup: primary === 'vp9' ? 'vp8' : primary === 'av1' ? 'vp9' : null,
    simulcast: primary === 'vp8',
  };
}
