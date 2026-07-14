/**
 * Shared constants for the Watch All grid window's tile components
 * (WatchAllPage.tsx, ShareTile.tsx, AudioOnlyTile.tsx). Split out to avoid a
 * circular import between the page and its extracted tile components.
 */

export const DEBUG_SHARE_VIEW = import.meta.env.VITE_DEBUG_SCREEN_SHARE_VIEW === 'true';
export const LOG = '[wavis:watch-all]';

export const STREAM_MUTED_ICON = '○';
export const STREAM_UNMUTED_ICON = '●';
export const MIXER_ICON = '⊞';
