import { describe, expect, it } from 'vitest';
import { parseSignalingMessage } from './parse';
import { KNOWN_SERVER_MESSAGE_TYPES } from './messages';

describe('parseSignalingMessage', () => {
  it('rejects non-object input', () => {
    expect(parseSignalingMessage(null)).toEqual({ kind: 'invalid' });
    expect(parseSignalingMessage(undefined)).toEqual({ kind: 'invalid' });
    expect(parseSignalingMessage(42)).toEqual({ kind: 'invalid' });
    expect(parseSignalingMessage('joined')).toEqual({ kind: 'invalid' });
  });

  it('rejects an object with a missing or non-string type', () => {
    expect(parseSignalingMessage({})).toEqual({ kind: 'invalid' });
    expect(parseSignalingMessage({ type: 42 })).toEqual({ kind: 'invalid' });
    expect(parseSignalingMessage({ noType: true })).toEqual({ kind: 'invalid' });
  });

  it('classifies an unrecognized type as unknown, preserving the raw payload', () => {
    const raw = { type: 'some_future_message', foo: 'bar' };
    expect(parseSignalingMessage(raw)).toEqual({
      kind: 'unknown',
      type: 'some_future_message',
      raw,
    });
  });

  it('classifies every known type tag as known and passes the payload through untouched', () => {
    const fixtures: Record<string, Record<string, unknown>> = {
      auth_success: { type: 'auth_success' },
      auth_failed: { type: 'auth_failed', reason: 'invalid_token' },
      joined: {
        type: 'joined',
        peerId: 'self-peer',
        roomId: 'room-1',
        participants: [],
        sharePermission: 'anyone',
      },
      sfu_cold_starting: { type: 'sfu_cold_starting', estimatedWaitSecs: 90 },
      join_rejected: { type: 'join_rejected', reason: 'room_full' },
      participant_joined: {
        type: 'participant_joined',
        participantId: 'peer-2',
        displayName: 'Bob',
      },
      participant_left: { type: 'participant_left', participantId: 'peer-2' },
      sub_room_state: {
        type: 'sub_room_state',
        rooms: [{ subRoomId: 'r1', roomNumber: 1, isDefault: true, participantIds: [] }],
      },
      sub_room_created: {
        type: 'sub_room_created',
        room: { subRoomId: 'r2', roomNumber: 2, isDefault: false, participantIds: [] },
      },
      sub_room_joined: {
        type: 'sub_room_joined',
        participantId: 'peer-2',
        subRoomId: 'r1',
        source: 'explicit',
      },
      sub_room_left: { type: 'sub_room_left', participantId: 'peer-2', subRoomId: 'r1' },
      sub_room_deleted: { type: 'sub_room_deleted', subRoomId: 'r1' },
      room_state: { type: 'room_state', participants: [] },
      participant_kicked: { type: 'participant_kicked', participantId: 'peer-2' },
      session_displaced: { type: 'session_displaced' },
      participant_muted: { type: 'participant_muted', participantId: 'peer-2' },
      participant_unmuted: { type: 'participant_unmuted', participantId: 'peer-2' },
      participant_self_muted: { type: 'participant_self_muted', participantId: 'peer-2' },
      participant_self_unmuted: { type: 'participant_self_unmuted', participantId: 'peer-2' },
      participant_deafened: { type: 'participant_deafened', participantId: 'peer-2' },
      participant_undeafened: { type: 'participant_undeafened', participantId: 'peer-2' },
      participant_color_updated: {
        type: 'participant_color_updated',
        participantId: 'peer-2',
        profileColor: '#ff0000',
      },
      participant_username_updated: {
        type: 'participant_username_updated',
        participantId: 'peer-2',
        username: 'newname',
      },
      share_started: { type: 'share_started', participantId: 'peer-2', shareType: 'screen' },
      share_stopped: { type: 'share_stopped', participantId: 'peer-2' },
      share_state: {
        type: 'share_state',
        participantIds: ['peer-2'],
        activeShares: [{ participantId: 'peer-2', shareType: 'screen' }],
      },
      share_permission_changed: { type: 'share_permission_changed', permission: 'host_only' },
      error: { type: 'error', message: 'boom' },
      peer_left: { type: 'peer_left', participantId: 'peer-2' },
      media_token: { type: 'media_token', token: 'tok', sfuUrl: 'wss://sfu' },
      viewer_token: {
        type: 'viewer_token',
        windowId: 'w1',
        token: 'tok',
        sfuUrl: 'wss://sfu',
        identity: 'peer-2',
      },
      chat_message: {
        type: 'chat_message',
        timestamp: '2026-01-01T00:00:00Z',
        participantId: 'peer-2',
        displayName: 'Bob',
        text: 'hi',
      },
      chat_history_response: { type: 'chat_history_response', messages: [] },
      viewer_joined: { type: 'viewer_joined' },
    };

    for (const type of KNOWN_SERVER_MESSAGE_TYPES) {
      const raw = fixtures[type];
      expect(raw, `missing fixture for type ${type}`).toBeDefined();
      expect(parseSignalingMessage(raw)).toEqual({ kind: 'known', msg: raw });
    }
    // fixtures map and KNOWN_SERVER_MESSAGE_TYPES stay in sync
    expect(Object.keys(fixtures).sort()).toEqual([...KNOWN_SERVER_MESSAGE_TYPES].sort());
  });
});
