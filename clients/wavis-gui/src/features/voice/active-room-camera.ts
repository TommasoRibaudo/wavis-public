export function cameraButtonLabel(cameraIntent: boolean): 'camera-off' | 'camera-on' {
  return cameraIntent ? 'camera-off' : 'camera-on';
}

export function shouldMountCameraButton(
  voiceRoomConnected: boolean,
  supportedCapturePlatform: boolean,
): boolean {
  return voiceRoomConnected && supportedCapturePlatform;
}

export function hasBrowserCameraMediaSupport(): boolean {
  return typeof window !== 'undefined'
    && typeof navigator !== 'undefined'
    && 'RTCPeerConnection' in window
    && 'mediaDevices' in navigator
    && navigator.mediaDevices !== undefined
    && typeof navigator.mediaDevices.getUserMedia === 'function';
}
