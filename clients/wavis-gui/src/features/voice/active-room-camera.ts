export function cameraButtonLabel(cameraIntent: boolean): 'Start Video' | 'Stop Video' {
  return cameraIntent ? 'Stop Video' : 'Start Video';
}

export function shouldMountCameraButton(
  voiceRoomConnected: boolean,
  supportedCapturePlatform: boolean,
): boolean {
  return voiceRoomConnected && supportedCapturePlatform;
}
