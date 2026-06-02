import type { ShareSelection } from '@features/screen-share/share-types';

export const SHARE_PICKER_LOADING_LABEL = 'waiting for screen picker...';
export const SHARE_STARTING_LOADING_LABEL = 'Starting screen share';

export function isVideoShareSelectionMode(mode: ShareSelection['mode']): boolean {
  return mode !== 'audio_only';
}
