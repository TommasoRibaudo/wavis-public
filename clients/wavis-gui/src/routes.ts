import { createBrowserRouter } from 'react-router';
import DeviceSetup from '@features/auth/DeviceSetup';
import AuthGate from '@features/auth/AuthGate';
import ChannelsList from '@features/channels/ChannelsList';

/** Helper to add HydrateFallback to lazy-loaded routes. In React Router v7 with CSR,
 * lazy routes briefly enter a "hydrating" state while the dynamic import resolves.
 * We use () => null to suppress the warning without adding unnecessary UI. */
const withFallback = (mod: Record<string, unknown>) => ({
  ...mod,
  HydrateFallback: () => null,
});

export const router = createBrowserRouter([
  {
    path: '/setup',
    Component: DeviceSetup,
  },
  {
    path: '/recover',
    lazy: async () => {
      const { default: Component } = await import('@features/auth/RecoverAccount');
      return withFallback({ Component });
    },
  },
  {
    path: '/login',
    lazy: async () => {
      const { default: Component } = await import('@features/auth/Login');
      return withFallback({ Component });
    },
  },
  {
    path: '/pair',
    lazy: async () => {
      const { default: Component } = await import('@features/auth/PairDevice');
      return withFallback({ Component });
    },
  },
  {
    path: '/screen-share',
    lazy: async () => {
      const { default: Component } = await import('@features/screen-share/ScreenSharePage');
      return withFallback({ Component });
    },
  },
  {
    path: '/watch-all',
    lazy: async () => {
      const { default: Component } = await import('@features/screen-share/WatchAllPage');
      return withFallback({ Component });
    },
  },
  {
    path: '/share-picker',
    lazy: async () => {
      const { default: Component } = await import('@features/screen-share/SharePicker');
      return withFallback({ Component });
    },
  },
  {
    path: '/share-indicator',
    lazy: async () => {
      const { default: Component } = await import('@features/screen-share/ShareIndicator');
      return withFallback({ Component });
    },
  },
  ...(import.meta.env.VITE_DIAGNOSTICS === 'true'
    ? [
        {
          path: '/diagnostics',
          lazy: async () => {
            const { default: Component } = await import('@features/diagnostics/DiagnosticsPage');
            return withFallback({ Component });
          },
        },
      ]
    : []),
  {
    path: '/',
    Component: AuthGate,
    children: [
      { index: true, Component: ChannelsList },
      {
        path: 'channel/:channelId',
        lazy: async () => {
          const { default: Component } = await import('@features/channels/ChannelDetail');
          return withFallback({ Component });
        },
      },
      {
        path: 'settings',
        lazy: async () => {
          const { default: Component } = await import('@features/settings/Settings');
          return withFallback({ Component });
        },
      },
      {
        path: 'devices',
        lazy: async () => {
          const { default: Component } = await import('@features/settings/DeviceList');
          return withFallback({ Component });
        },
      },
      {
        path: 'phrase',
        lazy: async () => {
          const { default: Component } = await import('@features/auth/ChangePhrase');
          return withFallback({ Component });
        },
      },
      {
        path: 'room',
        lazy: async () => {
          const { default: Component } = await import('@features/voice/ActiveRoom');
          return withFallback({ Component });
        },
      },
      {
        path: 'legacy',
        lazy: async () => {
          const { default: Component } = await import('@features/voice/LegacyRoom');
          return withFallback({ Component });
        },
      },
    ],
  },
]);
