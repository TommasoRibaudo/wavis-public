import { describe, expect, it } from 'vitest';

import { chatLinkTarget } from '../chat-links';

describe('chatLinkTarget', () => {
  it('opens github.com links in a new tab', () => {
    expect(chatLinkTarget('https://github.com/example/wavis/issues/79')).toBe('_blank');
  });

  it('opens GitHub subdomain links in a new tab', () => {
    expect(chatLinkTarget('https://gist.github.com/example/abc123')).toBe('_blank');
  });

  it('does not force non-GitHub links into a new tab', () => {
    expect(chatLinkTarget('https://example.com/github.com')).toBeUndefined();
  });

  it('ignores invalid URLs', () => {
    expect(chatLinkTarget('github.com/example/wavis')).toBeUndefined();
  });
});
