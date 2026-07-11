import { describe, expect, it } from 'vitest';

import { deriveTrayState } from '../useTrayIntegration';

describe('deriveTrayState', () => {
  it('reports the active self participant state', () => {
    expect(
      deriveTrayState({
        machineState: 'active',
        participants: [
          { id: 'other', isMuted: false },
          { id: 'self', isMuted: true },
        ],
        selfParticipantId: 'self',
        isDeafened: true,
      }),
    ).toEqual({
      inVoiceSession: true,
      isMuted: true,
      isDeafened: true,
    });
  });

  it('uses safe defaults when the self participant is unavailable', () => {
    expect(
      deriveTrayState({
        machineState: 'reconnecting',
        participants: [],
        selfParticipantId: null,
        isDeafened: false,
      }),
    ).toEqual({
      inVoiceSession: false,
      isMuted: false,
      isDeafened: false,
    });
  });
});
