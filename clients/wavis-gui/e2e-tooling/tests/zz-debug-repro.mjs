import { spawnPeer, joinVoiceAsPeer } from './live-backend-helpers.mjs';

const token1 = process.argv[2];
const token2 = process.argv[3];
const channelId = process.argv[4];

const peer1 = await spawnPeer({ accessToken: token1 });
const peer2 = await spawnPeer({ accessToken: token2 });

try {
  await joinVoiceAsPeer(peer1, { channelId, displayName: 'Peer1' });
  console.log('peer1 joined');
  await joinVoiceAsPeer(peer2, { channelId, displayName: 'Peer2' });
  console.log('peer2 joined');

  peer1.send({ type: 'chat_send', text: 'hello-from-peer1' });
  const received = await peer2.waitForOutput(/"type":"chat_message"/, 10_000);
  console.log('peer2 received:', received);
} catch (err) {
  console.error('REPRO FAILED:', err.message);
} finally {
  await peer1.close();
  await peer2.close();
}
