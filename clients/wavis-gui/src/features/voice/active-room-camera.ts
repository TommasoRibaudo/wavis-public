export function cameraButtonLabel(cameraIntent: boolean): 'camera-off' | 'camera-on' {
  return cameraIntent ? 'camera-off' : 'camera-on';
}

export function shouldMountCameraButton(
  voiceRoomConnected: boolean,
  supportedCapturePlatform: boolean,
): boolean {
  return voiceRoomConnected && supportedCapturePlatform;
}
