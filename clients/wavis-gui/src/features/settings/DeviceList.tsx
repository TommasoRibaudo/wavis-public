import { useState, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import { type DeviceInfo, listDevices, revokeDevice, logoutAll } from '@features/auth/auth';
import { ConfirmTextGate } from '@shared/ConfirmTextGate';
import { ErrorPanel } from '@shared/ErrorPanel';
import { LoadingBlock } from '@shared/LoadingBlock';
import { usePolling } from '@shared/hooks/usePolling';

/* ─── Constants ─────────────────────────────────────────────────── */
const POLL_MS = 15_000;
const DIVIDER = '─'.repeat(48);

/* ─── Helpers ───────────────────────────────────────────────────── */
function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString('en-US', {
      dateStyle: 'medium',
      timeStyle: 'short',
      hour12: false,
    });
  } catch {
    return iso;
  }
}

function truncateId(id: string): string {
  return id.length > 12 ? id.slice(0, 12) + '…' : id;
}

/* ─── Sub-components ────────────────────────────────────────────── */

interface DeviceCardProps {
  device: DeviceInfo;
  isCurrent: boolean;
  onRevoke: (deviceId: string) => void;
}

function DeviceCard({ device, isCurrent, onRevoke }: DeviceCardProps) {
  const [showConfirm, setShowConfirm] = useState(false);
  const [revoking, setRevoking] = useState(false);

  const isRevoked = device.revoked_at !== null;

  const handleRevoke = async () => {
    setRevoking(true);
    try {
      await revokeDevice(device.device_id);
      onRevoke(device.device_id);
    } finally {
      setRevoking(false);
      setShowConfirm(false);
    }
  };

  return (
    <div className="p-3 bg-wavis-panel border border-wavis-text-secondary space-y-1 text-sm">
      <div className="flex items-center gap-2">
        <span>{device.device_name}</span>
        {isCurrent && <span className="text-wavis-accent text-xs">(this device)</span>}
      </div>
      <div className="text-xs text-wavis-text-secondary">
        id: {truncateId(device.device_id)}
      </div>
      <div className="text-xs text-wavis-text-secondary">
        created: {formatDate(device.created_at)}
      </div>
      <div className="text-xs">
        {isRevoked ? (
          <span className="text-wavis-danger">
            revoked {device.revoked_at ? formatDate(device.revoked_at) : ''}
          </span>
        ) : (
          <span className="text-wavis-accent">active</span>
        )}
      </div>

      {!isRevoked && !showConfirm && (
        <button
          onClick={() => setShowConfirm(true)}
          className="mt-2 border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs"
        >
          /revoke
        </button>
      )}

      {showConfirm && (
        <ConfirmTextGate
          requiredText="REVOKE"
          busy={revoking}
          busyLabel="revoking..."
          onConfirm={handleRevoke}
          onCancel={() => setShowConfirm(false)}
        />
      )}
    </div>
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */
export default function DeviceList() {
  const navigate = useNavigate();

  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [currentDeviceId, setCurrentDeviceId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [showLogoutConfirm, setShowLogoutConfirm] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);

  const loadDevices = useCallback(async (silent = false) => {
    if (!silent) setLoading(true);
    setError(null);
    try {
      const data = await listDevices();
      setDevices(data.devices);
      setCurrentDeviceId(data.current_device_id);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load devices');
    } finally {
      setLoading(false);
    }
  }, []);

  // Initial load
  useEffect(() => { loadDevices(); }, [loadDevices]);

  // Auto-refresh
  usePolling(() => loadDevices(true), POLL_MS);

  const handleRevoke = () => {
    loadDevices(true);
  };

  const handleLogoutAll = async () => {
    setLoggingOut(true);
    try {
      await logoutAll();
      navigate('/setup', { replace: true });
    } catch {
      setError('Failed to log out all devices');
      setLoggingOut(false);
      setShowLogoutConfirm(false);
    }
  };

  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-3 sm:px-6 py-6">
          <button
            onClick={() => navigate('/settings')}
            className="mb-4 text-xs text-wavis-text-secondary border border-wavis-text-secondary py-0.5 px-1 text-center transition-colors hover:bg-wavis-text-secondary hover:text-wavis-text-contrast"
          >
            ← /settings
          </button>
          <h2>devices</h2>
          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Loading */}
          {loading && <LoadingBlock message="loading devices..." className="text-sm" />}

          {/* Error */}
          {error && (
            <ErrorPanel error={error} onRetry={() => loadDevices()} className="mb-4" />
          )}

          {/* Device list */}
          {!loading && !error && devices.length === 0 && (
            <p className="text-sm text-wavis-text-secondary">no devices found</p>
          )}

          {!loading && devices.length > 0 && (
            <div className="space-y-3 mb-6">
              {devices.map((device) => (
                <DeviceCard
                  key={device.device_id}
                  device={device}
                  isCurrent={device.device_id === currentDeviceId}
                  onRevoke={handleRevoke}
                />
              ))}
            </div>
          )}

          <div className="text-wavis-text-secondary my-4 overflow-hidden">{DIVIDER}</div>

          {/* Danger zone — Logout all */}
          <div>
            <p className="text-sm text-wavis-danger mb-2">DANGER ZONE</p>
            <div className="p-3 bg-wavis-panel border border-wavis-danger space-y-3">
              <div>
                <p className="text-sm">Log out all devices</p>
                <p className="text-xs text-wavis-text-secondary mt-1">
                  Revokes all sessions across every device. You will be returned to setup.
                </p>
              </div>

              {!showLogoutConfirm ? (
                <button
                  onClick={() => setShowLogoutConfirm(true)}
                  className="border border-wavis-danger text-wavis-danger hover:bg-wavis-danger hover:text-wavis-bg transition-colors px-1 py-0.5 text-xs"
                >
                  /logout-all
                </button>
              ) : (
                <ConfirmTextGate
                  requiredText="LOGOUT"
                  busy={loggingOut}
                  busyLabel="logging out..."
                  onConfirm={handleLogoutAll}
                  onCancel={() => setShowLogoutConfirm(false)}
                />
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
