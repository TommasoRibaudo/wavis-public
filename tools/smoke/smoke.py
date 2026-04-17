#!/usr/bin/env python3
"""
Wavis dev backend smoke test suite.
Runs against the live CloudFront-fronted backend after each dev deploy.

Exit 0 = all blocking tests passed.
Exit 1 = at least one blocking test failed.

Environment variables:
  BACKEND_URL         HTTPS base URL  (e.g. https://dt2nm86rf5ksq.cloudfront.net)
  WS_URL              WebSocket URL   (e.g. wss://dt2nm86rf5ksq.cloudfront.net/ws)
  LIVEKIT_HEALTH_URL  LiveKit health  (e.g. http://<ip>:7880/) - optional, skipped if unset
  SMOKE_TIMEOUT_SECS  Per-operation timeout in seconds (default: 10)

Test sequence (each depends on prior steps):
  1  HTTPS /health                          blocking
  2  POST /auth/register_device             blocking
  3  WebSocket connect                      blocking
  4  Ping (no error response)               blocking
  5  Auth -> auth_success                   blocking
  6  create_room -> room_created            NON-BLOCKING (legacy path)
  7  media_token arrives after room_created NON-BLOCKING (legacy path)
  8  media_token JWT structure              NON-BLOCKING (legacy path)
  9  LiveKit HTTP health                    NON-BLOCKING (warning only)
  10 leave + disconnect                     blocking
  11 POST /channels                         blocking
  12 POST /channels/{id}/invites            blocking
  13 POST /auth/register_device (user 2)    blocking
  14 POST /channels/join                    blocking
  15 User 1 WSS auth + join_voice           blocking
  16 User 2 WSS auth + join_voice           blocking
  17 participant_joined fanout              blocking
  18 GET /channels/{id}/voice active        blocking
  19 Channel media_token JWT structure      blocking
  20 LiveKit connect/disconnect             NON-BLOCKING (warning only)
  21 Both users leave                       blocking
  22 GET /channels/{id}/voice inactive      blocking
  23 Ban-eject scenario setup               blocking
  24 Ban-eject join_voice                   blocking
  25 REST ban -> participant_kicked         blocking
  26 Banned user rejoin rejected            blocking
"""

import asyncio
import base64
import json
import os
import sys
import time
import traceback
import uuid

import httpx
import websockets

BACKEND_URL = os.environ["BACKEND_URL"].rstrip("/")
WS_URL = os.environ["WS_URL"]
LIVEKIT_URL = os.environ.get("LIVEKIT_HEALTH_URL", "").strip()
TIMEOUT = int(os.environ.get("SMOKE_TIMEOUT_SECS", "10"))

PASSED = []
FAILED = []
WARNINGS = []


def ok(name: str):
    print(f"  [PASS] {name}")
    PASSED.append(name)


def fail(name: str, reason: str):
    print(f"  [FAIL] {name}: {reason}", file=sys.stderr)
    FAILED.append((name, reason))


def warn(name: str, reason: str):
    print(f"  [WARN] {name}: {reason}")
    WARNINGS.append((name, reason))


def auth_headers(access_token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {access_token}"}


def normalize_livekit_url(url: str) -> str:
    if url.startswith("https://"):
        return "wss://" + url[len("https://"):]
    if url.startswith("http://"):
        return "ws://" + url[len("http://"):]
    return url


async def ws_auth(ws, access_token: str) -> dict:
    await ws.send(json.dumps({"type": "auth", "accessToken": access_token}))
    msg = await recv_type(ws, "auth_success")
    assert "userId" in msg, f"missing userId in auth_success: {msg}"
    return msg


async def ws_join_voice(ws, channel_id: str, display_name: str) -> tuple[dict, dict]:
    await ws.send(json.dumps({
        "type": "join_voice",
        "channelId": channel_id,
        "displayName": display_name,
    }))
    joined = await recv_type(ws, "joined")
    media_token = await recv_type(ws, "media_token")
    assert media_token.get("token"), f"media_token.token is empty: {media_token}"
    assert media_token.get("sfuUrl"), f"media_token.sfuUrl is empty: {media_token}"
    return joined, media_token


async def recv_type(ws, expected_type: str, timeout: int = TIMEOUT):
    """
    Receive messages until one matching expected_type arrives or timeout fires.
    Logs unexpected messages received along the way.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        try:
            raw = await asyncio.wait_for(ws.recv(), timeout=remaining)
        except asyncio.TimeoutError:
            raise TimeoutError(f"Timed out after {timeout}s waiting for '{expected_type}'")
        msg = json.loads(raw)
        if msg.get("type") == expected_type:
            return msg
        if msg.get("type") == "error":
            raise RuntimeError(
                f"Server error while waiting for '{expected_type}': {msg.get('message', msg)}"
            )
        print(
            f"    [debug] recv_type waiting for '{expected_type}', "
            f"got '{msg.get('type')}' - continuing"
        )
    raise TimeoutError(f"Timed out after {timeout}s waiting for '{expected_type}'")


def decode_jwt_payload(token: str) -> tuple[dict | None, str]:
    parts = token.split(".")
    if len(parts) != 3:
        return None, f"expected 3 dot-separated parts, got {len(parts)}"
    try:
        padding = 4 - len(parts[1]) % 4
        payload_bytes = base64.urlsafe_b64decode(parts[1] + "=" * padding)
        return json.loads(payload_bytes), "ok"
    except Exception as e:
        return None, f"could not base64-decode payload: {e}"


def validate_jwt_structure(token: str, expected_room_id: str) -> tuple[bool, str]:
    """
    Returns (ok, reason). Does not verify the signature.

    Supports both token layouts emitted by the backend:
    - custom mode: room_id / participant_id
    - LiveKit mode: video.room / sub
    """
    payload, err = decode_jwt_payload(token)
    if payload is None:
        return False, err

    now = int(time.time())
    exp = payload.get("exp", 0)
    if exp <= now:
        return False, f"token is expired (exp={exp}, now={now})"

    if "room_id" in payload:
        room_claim = payload.get("room_id", "")
        identity_claim = payload.get("participant_id", "")
        mode = "custom"
    elif "video" in payload:
        video = payload.get("video", {})
        room_claim = video.get("room", "") if isinstance(video, dict) else ""
        identity_claim = payload.get("sub", "")
        mode = "livekit"
    else:
        return False, (
            f"unrecognized token format - payload keys: {list(payload.keys())}. "
            "Expected room_id/participant_id or video/sub."
        )

    if not room_claim:
        return False, f"[{mode}] room claim is empty"
    if not identity_claim:
        return False, f"[{mode}] participant identity claim is empty"
    if room_claim != expected_room_id:
        return False, (
            f"[{mode}] room claim mismatch: token says '{room_claim}', "
            f"expected '{expected_room_id}'"
        )

    return True, f"ok (mode={mode}, room={room_claim}, identity={identity_claim})"


async def livekit_connect_probe(sfu_url: str, token: str, expected_room_id: str):
    from livekit.rtc import Room

    room = Room()
    try:
        await room.connect(normalize_livekit_url(sfu_url), token)
        assert room.isconnected(), "room did not report connected state"
        assert room.name == expected_room_id, (
            f"room name mismatch: got '{room.name}', expected '{expected_room_id}'"
        )
    finally:
        try:
            await room.disconnect()
        except Exception:
            pass


async def run_smoke():
    try:
        r = httpx.get(f"{BACKEND_URL}/health", timeout=TIMEOUT)
        assert r.status_code == 200, f"HTTP {r.status_code}"
        ok("1_https_health")
    except Exception as e:
        fail("1_https_health", str(e))
        return

    access_token = None
    try:
        r = httpx.post(f"{BACKEND_URL}/auth/register_device", timeout=TIMEOUT)
        assert r.status_code == 201, f"HTTP {r.status_code} (expected 201 CREATED)"
        body = r.json()
        assert "access_token" in body, f"missing access_token in response: {list(body.keys())}"
        access_token = body["access_token"]
        ok("2_device_register")
    except Exception as e:
        fail("2_device_register", str(e))

    try:
        async with websockets.connect(WS_URL, open_timeout=TIMEOUT) as ws:
            ok("3_ws_connect")

            try:
                await ws.send(json.dumps({"type": "ping"}))
                try:
                    msg = await asyncio.wait_for(ws.recv(), timeout=2.0)
                    parsed = json.loads(msg)
                    if parsed.get("type") == "error":
                        fail("4_ping", f"unexpected error response: {parsed}")
                    else:
                        print(f"    [debug] ping: unexpected message type '{parsed.get('type')}'")
                        ok("4_ping")
                except asyncio.TimeoutError:
                    ok("4_ping")
            except Exception as e:
                fail("4_ping", str(e))

            if access_token:
                try:
                    await ws.send(json.dumps({"type": "auth", "accessToken": access_token}))
                    msg = await recv_type(ws, "auth_success")
                    assert "userId" in msg, f"missing userId in auth_success: {msg}"
                    ok("5_ws_auth")
                except Exception as e:
                    fail("5_ws_auth", str(e))
            else:
                fail("5_ws_auth", "skipped - no access_token from step 2")

            # Legacy direct-room path: keep coverage, but do not gate deploys on it.
            # The current GUI path is channel-based JoinVoice; direct create/join remains
            # as backward compatibility and for non-primary clients/tools.
            room_id = f"smoke-{uuid.uuid4().hex[:8]}"
            got_room_created = False
            try:
                await ws.send(json.dumps({
                    "type": "create_room",
                    "roomId": room_id,
                    "roomType": "sfu",
                    "displayName": "smoke-bot",
                }))
                msg = await recv_type(ws, "room_created")
                assert msg.get("roomId") == room_id, (
                    f"roomId mismatch: got '{msg.get('roomId')}', expected '{room_id}'"
                )
                got_room_created = True
                ok("6_create_room")
            except Exception as e:
                warn("6_create_room", str(e))

            media_token_value = None
            if got_room_created:
                try:
                    msg = await recv_type(ws, "media_token")
                    media_token_value = msg.get("token", "")
                    assert media_token_value, "media_token.token is empty"
                    ok("7_media_token_received")
                except Exception as e:
                    warn("7_media_token_received", str(e))

            if media_token_value:
                try:
                    valid, reason = validate_jwt_structure(media_token_value, room_id)
                    assert valid, reason
                    ok("8_token_structure")
                except Exception as e:
                    warn("8_token_structure", str(e))
            else:
                warn("8_token_structure", "skipped - no media_token from step 7")

            if LIVEKIT_URL:
                try:
                    r = httpx.get(LIVEKIT_URL, timeout=TIMEOUT)
                    assert r.status_code == 200, f"HTTP {r.status_code}"
                    ok("9_livekit_health")
                except Exception as e:
                    warn(
                        "9_livekit_health",
                        f"{e} - LiveKit may be down or security group tightened; investigate",
                    )
            else:
                print("  [SKIP] 9_livekit_health: LIVEKIT_HEALTH_URL not set")

            try:
                await ws.send(json.dumps({"type": "leave"}))
                ok("10_leave")
            except Exception as e:
                fail("10_leave", str(e))

    except websockets.exceptions.WebSocketException as e:
        fail("3_ws_connect", str(e))
    except Exception:
        fail("3_ws_connect", traceback.format_exc().strip())

    channel_id = None
    invite_code = None
    second_access_token = None
    second_user_id = None

    if access_token:
        channel_name = f"smoke-{uuid.uuid4().hex[:8]}"
        try:
            r = httpx.post(
                f"{BACKEND_URL}/channels",
                headers=auth_headers(access_token),
                json={"name": channel_name},
                timeout=TIMEOUT,
            )
            assert r.status_code == 201, f"HTTP {r.status_code} (expected 201 CREATED)"
            body = r.json()
            channel_id = body.get("channel_id")
            assert channel_id, f"missing channel_id in response: {body}"
            assert body.get("name") == channel_name, (
                f"name mismatch: got '{body.get('name')}', expected '{channel_name}'"
            )
            for field in ("owner_user_id", "created_at"):
                assert body.get(field), f"missing {field} in response: {body}"
            ok("11_create_channel")
        except Exception as e:
            fail("11_create_channel", str(e))
    else:
        fail("11_create_channel", "skipped - no access_token from step 2")

    if access_token and channel_id:
        try:
            r = httpx.post(
                f"{BACKEND_URL}/channels/{channel_id}/invites",
                headers=auth_headers(access_token),
                json={"expires_in_secs": 3600, "max_uses": 5},
                timeout=TIMEOUT,
            )
            assert r.status_code == 201, f"HTTP {r.status_code} (expected 201 CREATED)"
            body = r.json()
            invite_code = body.get("code")
            assert invite_code, f"missing code in response: {body}"
            assert body.get("channel_id") == channel_id, (
                f"channel_id mismatch: got '{body.get('channel_id')}', expected '{channel_id}'"
            )
            ok("12_create_invite")
        except Exception as e:
            fail("12_create_invite", str(e))
    else:
        fail("12_create_invite", "skipped - no authenticated channel owner from steps 2/11")

    try:
        r = httpx.post(f"{BACKEND_URL}/auth/register_device", timeout=TIMEOUT)
        assert r.status_code == 201, f"HTTP {r.status_code} (expected 201 CREATED)"
        body = r.json()
        second_access_token = body.get("access_token")
        second_user_id = body.get("user_id")
        assert second_access_token, f"missing access_token in response: {body}"
        assert second_user_id, f"missing user_id in response: {body}"
        ok("13_second_device_register")
    except Exception as e:
        fail("13_second_device_register", str(e))

    if invite_code and second_access_token and channel_id:
        try:
            r = httpx.post(
                f"{BACKEND_URL}/channels/join",
                headers=auth_headers(second_access_token),
                json={"code": invite_code},
                timeout=TIMEOUT,
            )
            assert r.status_code == 200, f"HTTP {r.status_code}"
            body = r.json()
            assert body.get("channel_id") == channel_id, (
                f"channel_id mismatch: got '{body.get('channel_id')}', expected '{channel_id}'"
            )
            assert body.get("role") == "member", (
                f"role mismatch: got '{body.get('role')}', expected 'member'"
            )
            assert body.get("name"), f"missing name in response: {body}"
            ok("14_join_channel")
        except Exception as e:
            fail("14_join_channel", str(e))
    else:
        fail("14_join_channel", "skipped - missing invite code or second user token")

    owner_joined = None
    member_joined = None
    owner_media_token = None
    member_media_token = None
    cleanup_leave_sent = False

    if access_token and second_access_token and channel_id:
        try:
            async with (
                websockets.connect(WS_URL, open_timeout=TIMEOUT) as ws1,
                websockets.connect(WS_URL, open_timeout=TIMEOUT) as ws2,
            ):
                try:
                    await ws_auth(ws1, access_token)
                    owner_joined, owner_media_token = await ws_join_voice(
                        ws1, channel_id, "Smoke Owner"
                    )
                    assert owner_joined.get("roomId", "").startswith("channel-"), (
                        f"expected roomId to start with 'channel-', got {owner_joined.get('roomId')}"
                    )
                    assert owner_joined.get("peerId"), (
                        f"missing peerId in joined payload: {owner_joined}"
                    )
                    assert owner_joined.get("peerCount") == 1, (
                        f"expected peerCount 1, got {owner_joined.get('peerCount')}"
                    )
                    participants = owner_joined.get("participants", [])
                    assert isinstance(participants, list), (
                        f"joined.participants must be a list, got {type(participants).__name__}"
                    )
                    assert any(
                        p.get("displayName") == "Smoke Owner" for p in participants
                    ), f"owner not present in joined participants: {participants}"
                    ok("15_owner_join_voice")
                except Exception as e:
                    fail("15_owner_join_voice", str(e))

                if owner_joined is not None:
                    try:
                        await ws_auth(ws2, second_access_token)
                        member_joined, member_media_token = await ws_join_voice(
                            ws2, channel_id, "Smoke Member"
                        )
                        assert member_joined.get("roomId") == owner_joined.get("roomId"), (
                            "member joined different room: "
                            f"{member_joined.get('roomId')} != {owner_joined.get('roomId')}"
                        )
                        assert member_joined.get("peerCount") == 2, (
                            f"expected peerCount 2, got {member_joined.get('peerCount')}"
                        )
                        participants = member_joined.get("participants", [])
                        assert isinstance(participants, list), (
                            f"joined.participants must be a list, got {type(participants).__name__}"
                        )
                        participant_names = {p.get("displayName") for p in participants}
                        assert "Smoke Owner" in participant_names, (
                            f"owner missing from member joined payload: {participants}"
                        )
                        assert "Smoke Member" in participant_names, (
                            f"member missing from member joined payload: {participants}"
                        )
                        ok("16_member_join_voice")
                    except Exception as e:
                        fail("16_member_join_voice", str(e))
                else:
                    fail("16_member_join_voice", "skipped - owner join_voice failed")

                if owner_joined is not None and member_joined is not None:
                    try:
                        msg = await recv_type(ws1, "participant_joined")
                        assert msg.get("displayName") == "Smoke Member", (
                            f"displayName mismatch: got '{msg.get('displayName')}'"
                        )
                        assert msg.get("participantId") == member_joined.get("peerId"), (
                            "participantId mismatch: "
                            f"{msg.get('participantId')} != {member_joined.get('peerId')}"
                        )
                        ok("17_participant_joined")
                    except Exception as e:
                        fail("17_participant_joined", str(e))
                else:
                    fail("17_participant_joined", "skipped - join_voice preconditions failed")

                if owner_joined is not None and member_joined is not None:
                    try:
                        r = httpx.get(
                            f"{BACKEND_URL}/channels/{channel_id}/voice",
                            headers=auth_headers(access_token),
                            timeout=TIMEOUT,
                        )
                        assert r.status_code == 200, f"HTTP {r.status_code}"
                        body = r.json()
                        assert body.get("active") is True, f"expected active=true, got {body}"
                        assert body.get("participant_count") == 2, (
                            f"expected participant_count 2, got {body.get('participant_count')}"
                        )
                        participants = body.get("participants")
                        assert isinstance(participants, list), (
                            f"participants must be a list when active, got {participants}"
                        )
                        participant_names = {p.get("display_name") for p in participants}
                        assert "Smoke Owner" in participant_names, (
                            f"owner missing from voice status: {participants}"
                        )
                        assert "Smoke Member" in participant_names, (
                            f"member missing from voice status: {participants}"
                        )
                        ok("18_voice_status_active")
                    except Exception as e:
                        fail("18_voice_status_active", str(e))
                else:
                    fail("18_voice_status_active", "skipped - active voice session was not established")

                if owner_media_token and member_media_token and owner_joined and member_joined:
                    try:
                        for token_msg, joined_msg, label in (
                            (owner_media_token, owner_joined, "owner"),
                            (member_media_token, member_joined, "member"),
                        ):
                            token = token_msg["token"]
                            room_id_for_token = joined_msg.get("roomId", "")
                            valid, reason = validate_jwt_structure(token, room_id_for_token)
                            assert valid, f"{label} token invalid: {reason}"
                        ok("19_channel_token_structure")
                    except Exception as e:
                        fail("19_channel_token_structure", str(e))
                else:
                    fail("19_channel_token_structure", "skipped - no channel media tokens from steps 15/16")

                if owner_media_token and owner_joined:
                    try:
                        await livekit_connect_probe(
                            owner_media_token["sfuUrl"],
                            owner_media_token["token"],
                            owner_joined["roomId"],
                        )
                        ok("20_livekit_connect")
                    except Exception as e:
                        warn(
                            "20_livekit_connect",
                            f"{e} - LiveKit token or public SFU URL may be misconfigured; investigate",
                        )
                else:
                    warn("20_livekit_connect", "skipped - no channel media token available from step 15")

                if owner_joined is not None and member_joined is not None:
                    try:
                        await ws2.send(json.dumps({"type": "leave"}))
                        await ws1.send(json.dumps({"type": "leave"}))
                        cleanup_leave_sent = True
                        ok("21_leave_disconnect")
                    except Exception as e:
                        fail("21_leave_disconnect", str(e))
                else:
                    fail("21_leave_disconnect", "skipped - active voice session was not established")
        except websockets.exceptions.WebSocketException as e:
            fail("15_owner_join_voice", f"websocket setup failed: {e}")
            fail("16_member_join_voice", f"websocket setup failed: {e}")
            fail("17_participant_joined", f"websocket setup failed: {e}")
            fail("18_voice_status_active", f"websocket setup failed: {e}")
            fail("19_channel_token_structure", f"websocket setup failed: {e}")
            fail("21_leave_disconnect", f"websocket setup failed: {e}")
            warn("20_livekit_connect", f"skipped - websocket setup failed before LiveKit probe: {e}")
        except Exception as e:
            reason = traceback.format_exc().strip() if not str(e) else str(e)
            fail("15_owner_join_voice", f"websocket setup failed: {reason}")
            fail("16_member_join_voice", f"websocket setup failed: {reason}")
            fail("17_participant_joined", f"websocket setup failed: {reason}")
            fail("18_voice_status_active", f"websocket setup failed: {reason}")
            fail("19_channel_token_structure", f"websocket setup failed: {reason}")
            fail("21_leave_disconnect", f"websocket setup failed: {reason}")
            warn("20_livekit_connect", f"skipped - websocket setup failed before LiveKit probe: {reason}")
    else:
        fail("15_owner_join_voice", "skipped - missing channel voice prerequisites")
        fail("16_member_join_voice", "skipped - missing channel voice prerequisites")
        fail("17_participant_joined", "skipped - missing channel voice prerequisites")
        fail("18_voice_status_active", "skipped - missing channel voice prerequisites")
        fail("19_channel_token_structure", "skipped - missing channel voice prerequisites")
        fail("21_leave_disconnect", "skipped - missing channel voice prerequisites")
        warn("20_livekit_connect", "skipped - missing channel voice prerequisites")

    if access_token and channel_id and cleanup_leave_sent:
        try:
            await asyncio.sleep(0.2)
            r = httpx.get(
                f"{BACKEND_URL}/channels/{channel_id}/voice",
                headers=auth_headers(access_token),
                timeout=TIMEOUT,
            )
            assert r.status_code == 200, f"HTTP {r.status_code}"
            body = r.json()
            assert body.get("active") is False, f"expected active=false, got {body}"
            participant_count = body.get("participant_count")
            assert participant_count in (None, 0), (
                f"expected participant_count None/0 after cleanup, got {participant_count}"
            )
            participants = body.get("participants")
            assert participants in (None, []), (
                f"expected participants None/[] after cleanup, got {participants}"
            )
            ok("22_voice_status_inactive")
        except Exception as e:
            fail("22_voice_status_inactive", str(e))
    else:
        fail("22_voice_status_inactive", "skipped - leave/disconnect cleanup preconditions failed")

    # Tests 23-26: isolated ban-eject scenario using fresh WS connections.
    ban_channel_id = None
    ban_invite_code = None

    if access_token:
        ban_channel_name = f"smoke-ban-{uuid.uuid4().hex[:8]}"
        try:
            r = httpx.post(
                f"{BACKEND_URL}/channels",
                headers=auth_headers(access_token),
                json={"name": ban_channel_name},
                timeout=TIMEOUT,
            )
            assert r.status_code == 201, f"HTTP {r.status_code} (expected 201 CREATED)"
            body = r.json()
            ban_channel_id = body.get("channel_id")
            assert ban_channel_id, f"missing channel_id in response: {body}"

            r = httpx.post(
                f"{BACKEND_URL}/channels/{ban_channel_id}/invites",
                headers=auth_headers(access_token),
                json={"expires_in_secs": 3600, "max_uses": 5},
                timeout=TIMEOUT,
            )
            assert r.status_code == 201, f"HTTP {r.status_code} (expected 201 CREATED)"
            body = r.json()
            ban_invite_code = body.get("code")
            assert ban_invite_code, f"missing code in response: {body}"

            if second_access_token:
                r = httpx.post(
                    f"{BACKEND_URL}/channels/join",
                    headers=auth_headers(second_access_token),
                    json={"code": ban_invite_code},
                    timeout=TIMEOUT,
                )
                assert r.status_code == 200, f"HTTP {r.status_code}"
            else:
                raise AssertionError("missing second_access_token for ban scenario")

            ok("23_ban_eject_setup")
        except Exception as e:
            fail("23_ban_eject_setup", str(e))
    else:
        fail("23_ban_eject_setup", "skipped - missing owner access token")

    if access_token and second_access_token and second_user_id and ban_channel_id:
        ban_owner_joined = None
        ban_member_joined = None
        try:
            async with (
                websockets.connect(WS_URL, open_timeout=TIMEOUT) as ws1,
                websockets.connect(WS_URL, open_timeout=TIMEOUT) as ws2,
            ):
                try:
                    await ws_auth(ws1, access_token)
                    ban_owner_joined, _ = await ws_join_voice(ws1, ban_channel_id, "Ban Owner")

                    await ws_auth(ws2, second_access_token)
                    ban_member_joined, _ = await ws_join_voice(ws2, ban_channel_id, "Ban Member")

                    assert ban_owner_joined.get("roomId") == ban_member_joined.get("roomId"), (
                        "ban scenario users joined different rooms: "
                        f"{ban_owner_joined.get('roomId')} != {ban_member_joined.get('roomId')}"
                    )
                    _ = await recv_type(ws1, "participant_joined")
                    ok("24_ban_eject_join_voice")
                except Exception as e:
                    fail("24_ban_eject_join_voice", str(e))

                if ban_owner_joined is not None and ban_member_joined is not None:
                    try:
                        r = httpx.post(
                            f"{BACKEND_URL}/channels/{ban_channel_id}/bans/{second_user_id}",
                            headers=auth_headers(access_token),
                            timeout=TIMEOUT,
                        )
                        assert r.status_code == 200, f"HTTP {r.status_code}"

                        owner_kicked = await recv_type(ws1, "participant_kicked")
                        assert owner_kicked.get("participantId") == ban_member_joined.get("peerId"), (
                            "owner participant_kicked mismatch: "
                            f"{owner_kicked.get('participantId')} != {ban_member_joined.get('peerId')}"
                        )

                        member_kicked = await recv_type(ws2, "participant_kicked", timeout=3)
                        assert member_kicked.get("participantId") == ban_member_joined.get("peerId"), (
                            "member participant_kicked mismatch: "
                            f"{member_kicked.get('participantId')} != {ban_member_joined.get('peerId')}"
                        )
                        ok("25_ban_eject")
                    except Exception as e:
                        fail("25_ban_eject", str(e))

                    # Test 26: banned member reconnects on a fresh WS and tries to rejoin.
                    # Must use a new connection — the kicked ws2 still carries a live
                    # SignalingSession, so join_voice on it hits "already joined" before
                    # the ban check runs. A real client would reconnect after being kicked.
                    try:
                        async with websockets.connect(WS_URL, open_timeout=TIMEOUT) as ws3:
                            await ws_auth(ws3, second_access_token)
                            await ws3.send(json.dumps({
                                "type": "join_voice",
                                "channelId": ban_channel_id,
                                "displayName": "Ban Member Return",
                            }))
                            rejected = await recv_type(ws3, "join_rejected")
                            assert rejected.get("reason") == "not_authorized", (
                                f"expected not_authorized, got {rejected}"
                            )
                        ok("26_banned_rejoin_rejected")
                    except Exception as e:
                        fail("26_banned_rejoin_rejected", str(e))
                else:
                    fail("25_ban_eject", "skipped - join_voice preconditions failed")
                    fail("26_banned_rejoin_rejected", "skipped - join_voice preconditions failed")
        except websockets.exceptions.WebSocketException as e:
            fail("24_ban_eject_join_voice", f"websocket setup failed: {e}")
            fail("25_ban_eject", f"websocket setup failed: {e}")
            fail("26_banned_rejoin_rejected", f"websocket setup failed: {e}")
        except Exception as e:
            reason = traceback.format_exc().strip() if not str(e) else str(e)
            fail("24_ban_eject_join_voice", f"websocket setup failed: {reason}")
            fail("25_ban_eject", f"websocket setup failed: {reason}")
            fail("26_banned_rejoin_rejected", f"websocket setup failed: {reason}")
    else:
        fail("24_ban_eject_join_voice", "skipped - missing ban scenario prerequisites")
        fail("25_ban_eject", "skipped - missing ban scenario prerequisites")
        fail("26_banned_rejoin_rejected", "skipped - missing ban scenario prerequisites")


async def main():
    print("\n=== Wavis Smoke Tests ===")
    print("  backend : configured")
    print("  ws      : configured")
    print(f"  livekit : {'configured' if LIVEKIT_URL else '(skipped)'}")
    print()

    await run_smoke()

    print(
        f"\n--- Results: {len(PASSED)} passed, {len(FAILED)} failed, "
        f"{len(WARNINGS)} warnings ---"
    )

    for name, reason in WARNINGS:
        print(f"  WARN: {name} - {reason}")
    for name, reason in FAILED:
        print(f"  FAIL: {name} - {reason}", file=sys.stderr)

    if FAILED:
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())
