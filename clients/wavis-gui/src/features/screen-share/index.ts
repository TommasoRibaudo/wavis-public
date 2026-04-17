// Public API for features/screen-share
// Other features should import from here, not from internal paths directly.

export type { ShareMode, ShareSourceType, ShareSource, EnumerationResult, ShareSelection } from './share-types';
export { startReceiving, stopReceiving, startSending, stopSending, stopSendingForWindow, stopAllSending, resendStream, StreamReceiver, isVideoTrackAlive } from './screen-share-viewer';
