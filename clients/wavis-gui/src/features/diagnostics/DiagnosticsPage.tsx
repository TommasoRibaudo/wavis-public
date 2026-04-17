/**
 * Wavis Diagnostics Window
 *
 * Env-gated floating window for real-time resource and network monitoring.
 * Only active when WAVIS_DIAGNOSTICS_WINDOW=1.
 *
 * Data sources:
 *   - Process RSS + CPU %    — Rust IPC, ~1s cadence
 *   - JS heap + DOM nodes    — frontend, ~1s cadence
 *   - Network stats + MOS    — voice-room.ts state pull, ~1s cadence
 *   - Audio levels           — voice-room.ts participant state, ~1s cadence
 *   - Screen share stats     — voice-room.ts state pull, ~5s cadence
 *   - Rolling history        — RingBuffer<DiagnosticsSnapshot>, 300 samples (5 min)
 *
 * Charts are per-metric and toggleable — click ▾ next to any value to expand.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
} from 'recharts';
import {
  initDiagnostics,
  destroyDiagnostics,
  setBaseline,
  clearBaseline,
  getBaseline,
  exportSnapshot,
  mosLabel,
  type DiagnosticsConfig,
  type DiagnosticsSnapshot,
  type DiagnosticsBaseline,
  type WarningEntry,
} from './diagnostics';
import { useCopyToClipboardFeedback } from '@shared/hooks/useCopyToClipboardFeedback';

/* ─── Helpers ───────────────────────────────────────────────────── */

function fmt1(n: number): string {
  return n.toFixed(1);
}

function fmtMb(n: number): string {
  return `${fmt1(n)} MB`;
}

function delta(current: number, base: number | undefined): string {
  if (base === undefined) return '';
  const d = current - base;
  const sign = d >= 0 ? '+' : '';
  return ` (${sign}${fmt1(d)})`;
}

function deltaInt(current: number, base: number | undefined): string {
  if (base === undefined) return '';
  const d = current - base;
  const sign = d >= 0 ? '+' : '';
  return ` (${sign}${d})`;
}

function fmtTimestamp(ts: number): string {
  return new Date(ts).toTimeString().slice(0, 8);
}

function mosColor(mos: number): string {
  if (mos >= 4.0) return 'text-wavis-accent';
  if (mos >= 3.1) return 'text-wavis-warn';
  return 'text-wavis-danger';
}

function candidateLabel(ct: string): string {
  if (ct === 'host') return 'Direct (LAN)';
  if (ct === 'srflx') return 'STUN (NAT traversal)';
  if (ct === 'relay') return 'TURN relay';
  return 'Unknown';
}

/* ─── Sub-components ─────────────────────────────────────────────── */

function SectionHeader({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[0.625rem] text-wavis-text-secondary uppercase tracking-widest mb-1 border-b border-wavis-text-secondary/20 pb-0.5">
      {children}
    </div>
  );
}

function Section({ children }: { children: React.ReactNode }) {
  return <div className="bg-wavis-panel border border-wavis-text-secondary/20 rounded px-3 py-2 flex flex-col gap-1">{children}</div>;
}

function ActionButton({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className="text-xs border border-wavis-text-secondary/40 text-wavis-text hover:border-wavis-accent hover:text-wavis-accent px-2 py-0.5 transition-colors"
    >
      {children}
    </button>
  );
}

/* ─── Sparkline chart ─────────────────────────────────────────────── */

type ChartDomain = [number | 'auto', number | 'auto'];

interface SparklineProps {
  data: { t: number; v: number | null }[];
  color?: string;
  unit?: string;
  domain?: ChartDomain;
}

function Sparkline({ data, color = '#6b7280', unit = '', domain }: SparklineProps) {
  return (
    <div className="h-[56px] w-full mt-0.5">
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={data} margin={{ top: 2, right: 2, bottom: 2, left: 0 }}>
          <XAxis
            dataKey="t"
            tickFormatter={fmtTimestamp}
            tick={{ fontSize: 8, fill: '#6b7280' }}
            tickLine={false}
            axisLine={false}
            interval={Math.max(0, Math.floor(data.length / 4) - 1)}
            minTickGap={40}
          />
          <YAxis
            tick={{ fontSize: 8, fill: '#6b7280' }}
            tickLine={false}
            axisLine={false}
            width={28}
            domain={domain ?? ['auto', 'auto']}
          />
          <Tooltip
            contentStyle={{ background: '#1a1a1a', border: '1px solid #374151', fontSize: 10, padding: '2px 6px' }}
            labelFormatter={(v) => fmtTimestamp(v as number)}
            formatter={(v: number) => [`${v.toFixed(1)}${unit}`, '']}
          />
          <Line
            type="monotone"
            dataKey="v"
            stroke={color}
            dot={false}
            strokeWidth={1}
            connectNulls={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}

/* ─── MetricRow — plain label/value row (no chart support) ────────── */

interface MetricRowProps {
  label: string;
  value: string;
  colorClass?: string;
  dim?: boolean;
}

function MetricRow({ label, value, colorClass, dim }: MetricRowProps) {
  return (
    <div className="flex justify-between text-xs gap-2">
      <span className="text-wavis-text-secondary shrink-0">{label}</span>
      <span className={dim ? 'text-wavis-text-secondary' : (colorClass ?? 'text-wavis-text font-mono tabular-nums')}>
        {value}
      </span>
    </div>
  );
}

/* ─── ChartRow — metric row with toggleable inline sparkline ─────── */

interface ChartRowProps {
  label: string;
  value: string;
  colorClass?: string;
  chartKey: string;
  chartData: { t: number; v: number | null }[];
  chartColor?: string;
  chartUnit?: string;
  chartDomain?: ChartDomain;
  openCharts: Set<string>;
  onToggle: (key: string) => void;
}

function ChartRow({
  label, value, colorClass, chartKey, chartData, chartColor, chartUnit, chartDomain,
  openCharts, onToggle,
}: ChartRowProps) {
  const hasData = chartData.some(d => d.v !== null);
  const isOpen = hasData && openCharts.has(chartKey);
  return (
    <>
      <div className="flex justify-between text-xs gap-2">
        <span className="text-wavis-text-secondary shrink-0">{label}</span>
        <div className="flex items-center gap-0.5">
          <span className={colorClass ?? 'text-wavis-text font-mono tabular-nums'}>
            {value}
          </span>
          {hasData && (
            <button
              onClick={() => onToggle(chartKey)}
              className="text-[0.7rem] text-wavis-text-secondary/70 hover:text-wavis-accent ml-1 px-0.5 shrink-0 leading-none"
              title={isOpen ? 'Hide chart' : 'Show chart'}
            >
              {isOpen ? '▴' : '▾'}
            </button>
          )}
        </div>
      </div>
      {isOpen && (
        <Sparkline
          data={chartData}
          color={chartColor}
          unit={chartUnit}
          domain={chartDomain}
        />
      )}
    </>
  );
}

/* ═══ Component ═════════════════════════════════════════════════════ */

export default function DiagnosticsPage() {
  const [config, setConfig] = useState<DiagnosticsConfig | null>(null);
  const [snap, setSnap] = useState<DiagnosticsSnapshot | null>(null);
  const [bl, setBl] = useState<DiagnosticsBaseline | null>(null);
  const [activeWarnings, setActiveWarnings] = useState<WarningEntry[]>([]);
  const [history, setHistory] = useState<DiagnosticsSnapshot[]>([]);
  const [openCharts, setOpenCharts] = useState<Set<string>>(new Set());
  const [copy, copied] = useCopyToClipboardFeedback({
    feedbackMs: 1500,
    writeText: async (text) => {
      await writeText(text).catch(() => {
        console.warn('[wavis:diagnostics] clipboard write failed');
        throw new Error('clipboard write failed');
      });
    },
  });
  const unlistenCloseRef = useRef<UnlistenFn | null>(null);
  const unlistenMainRef = useRef<UnlistenFn | null>(null);
  const closingRef = useRef(false);

  const toggleChart = useCallback((key: string) => {
    setOpenCharts(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key); else next.add(key);
      return next;
    });
  }, []);

  const closeWindow = async () => {
    if (closingRef.current) return;
    closingRef.current = true;
    destroyDiagnostics();
    unlistenCloseRef.current?.();
    unlistenCloseRef.current = null;
    unlistenMainRef.current?.();
    unlistenMainRef.current = null;
    await getCurrentWindow().destroy();
  };

  useEffect(() => {
    let mounted = true;

    const win = getCurrentWindow();

    // Close the window cleanly when the OS X button is pressed.
    // The listener must explicitly call close() — Tauri does not auto-close
    // when a JS listener is registered for tauri://close-requested.
    win
      .onCloseRequested(() => {
        destroyDiagnostics();
      })
      .then((unlisten: UnlistenFn) => {
        if (!mounted) { unlisten(); return; }
        unlistenCloseRef.current = unlisten;
      });

    // Close when the main window is destroyed (user closed the app).
    WebviewWindow.getByLabel('main').then((mainWin) => {
      if (!mainWin || !mounted) return;
      mainWin
        .listen('tauri://destroyed', () => {
          void closeWindow();
        })
        .then((unlisten: UnlistenFn) => {
          if (!mounted) { unlisten(); return; }
          unlistenMainRef.current = unlisten;
        });
    });

    initDiagnostics((s, w, h) => {
      if (!mounted) return;
      setSnap(s);
      setActiveWarnings(w);
      setBl(getBaseline());
      // History is only pushed every 5th poll (~5s) to reduce GC pressure.
      if (h !== null) setHistory(h);
    })
      .then((cfg) => { if (mounted) setConfig(cfg); })
      .catch((err) => {
        console.error('[wavis:diagnostics] init failed:', err);
      });

    return () => {
      mounted = false;
      destroyDiagnostics();
      unlistenCloseRef.current?.();
      unlistenMainRef.current?.();
    };
  }, []);

  /* ── Chart data — downsampled to every 5th sample for render perf ── */

  const chartData = useMemo(() => {
    // Display at most every 5th sample (≈5s resolution, 60 points for 5 min).
    // Full-res 300-sample buffer is retained in diagnostics.ts for export accuracy.
    const sampled = history.filter((_, i) => i % 5 === 0);
    return {
      rtt:         sampled.map(s => ({ t: s.timestamp, v: s.network ? s.network.rttMs : null })),
      loss:        sampled.map(s => ({ t: s.timestamp, v: s.network ? s.network.packetLossPercent : null })),
      jitter:      sampled.map(s => ({ t: s.timestamp, v: s.network ? s.network.jitterMs : null })),
      mos:         sampled.map(s => ({ t: s.timestamp, v: s.network ? s.network.mos : null })),
      jitterBuf:   sampled.map(s => ({ t: s.timestamp, v: s.network?.jitterBufferDelayMs ?? null })),
      concealment: sampled.map(s => ({ t: s.timestamp, v: s.network ? s.network.concealmentEventsPerInterval : null })),
      bandwidth:   sampled.map(s => ({ t: s.timestamp, v: s.network?.availableBandwidthKbps ? s.network.availableBandwidthKbps / 1000 : null })),
      rss:         sampled.map(s => ({ t: s.timestamp, v: s.rss ? s.rss.mb : null })),
      cpu:         sampled.map(s => ({ t: s.timestamp, v: s.cpuPercent })),
      jsHeap:      sampled.map(s => ({ t: s.timestamp, v: s.jsHeap ? s.jsHeap.usedMb : null })),
      domNodes:    sampled.map(s => ({ t: s.timestamp, v: s.domNodes })),
      shareBitrate:   sampled.map(s => ({ t: s.timestamp, v: s.share ? s.share.bitrateKbps / 1000 : null })),
      shareFps:       sampled.map(s => ({ t: s.timestamp, v: s.share ? s.share.fps : null })),
      sharePli:       sampled.map(s => ({ t: s.timestamp, v: s.share ? s.share.pliCount : null })),
      shareNack:      sampled.map(s => ({ t: s.timestamp, v: s.share ? s.share.nackCount : null })),
      recvFps:        sampled.map(s => ({ t: s.timestamp, v: s.videoReceive ? s.videoReceive.fps : null })),
      recvLoss:       sampled.map(s => ({ t: s.timestamp, v: s.videoReceive ? s.videoReceive.packetLossPercent : null })),
      recvJitterBuf:  sampled.map(s => ({ t: s.timestamp, v: s.videoReceive?.jitterBufferDelayMs ?? null })),
      recvFreeze:     sampled.map(s => ({ t: s.timestamp, v: s.videoReceive ? s.videoReceive.freezeCount : null })),
      recvPli:        sampled.map(s => ({ t: s.timestamp, v: s.videoReceive ? s.videoReceive.pliCount : null })),
      recvNack:       sampled.map(s => ({ t: s.timestamp, v: s.videoReceive ? s.videoReceive.nackCount : null })),
      recvDecode:     sampled.map(s => ({ t: s.timestamp, v: s.videoReceive?.avgDecodeTimeMs ?? null })),
    };
  }, [history]);

  /* ── Loading ─────────────────────────────────────────────────── */

  if (!snap) {
    return (
      <div className="h-full flex items-center justify-center bg-wavis-bg font-mono text-wavis-text-secondary text-xs">
        Initialising diagnostics...
      </div>
    );
  }

  /* ── Data ────────────────────────────────────────────────────── */

  const baseSnap = bl?.snapshot;
  const pollLabel = config ? `${config.pollMs}ms` : '';

  const handleSetBaseline = () => {
    setBaseline(snap);
    setBl(getBaseline());
  };

  const handleClearBaseline = () => {
    clearBaseline();
    setBl(null);
  };

  const handleCopySnapshot = () => {
    void copy(exportSnapshot(snap));
  };

  /* ── Render ──────────────────────────────────────────────────── */

  return (
    <div className="h-full overflow-y-auto bg-wavis-bg font-mono text-wavis-text flex flex-col gap-3 p-3 text-xs select-none">

      {/* Header */}
      <div className="flex items-center justify-between">
        <span className="text-[0.65rem] text-wavis-text-secondary tracking-widest uppercase">
          Wavis Diagnostics
        </span>
        <div className="flex items-center gap-2">
          <span className="text-[0.6rem] text-wavis-text-secondary/60">{pollLabel}</span>
          <button
            onClick={() => { void closeWindow(); }}
            className="text-wavis-text-secondary hover:text-wavis-text leading-none px-1"
            title="Close"
          >
            ×
          </button>
        </div>
      </div>

      {/* Network */}
      <Section>
        <SectionHeader>Network</SectionHeader>
        {snap.network ? (
          <>
            <ChartRow label="RTT" value={`${Math.round(snap.network.rttMs)} ms`}
              chartKey="rtt" chartData={chartData.rtt} chartColor="#60a5fa" chartUnit=" ms" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow label="Packet loss" value={`${fmt1(snap.network.packetLossPercent)}%`}
              chartKey="loss" chartData={chartData.loss} chartColor="#f87171" chartUnit="%" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow label="Jitter" value={`${Math.round(snap.network.jitterMs)} ms`}
              chartKey="jitter" chartData={chartData.jitter} chartColor="#fbbf24" chartUnit=" ms" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow
              label="MOS (est.)"
              value={`${snap.network.mos.toFixed(1)}  ${mosLabel(snap.network.mos)}`}
              colorClass={mosColor(snap.network.mos)}
              chartKey="mos" chartData={chartData.mos} chartColor="#34d399" chartUnit="" chartDomain={[1, 4.5]}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow
              label="Jitter buffer"
              value={snap.network.jitterBufferDelayMs > 0 ? `${snap.network.jitterBufferDelayMs} ms` : '—'}
              chartKey="jitterBuf" chartData={chartData.jitterBuf} chartColor="#a78bfa" chartUnit=" ms" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow
              label="Concealment"
              value={`${snap.network.concealmentEventsPerInterval} events`}
              colorClass={snap.network.concealmentEventsPerInterval > 10 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
              chartKey="concealment" chartData={chartData.concealment} chartColor="#fb923c" chartUnit=" evt" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow
              label="Avail. bandwidth"
              value={snap.network.availableBandwidthKbps > 0 ? `${(snap.network.availableBandwidthKbps / 1000).toFixed(1)} Mbps` : '—'}
              chartKey="bandwidth" chartData={chartData.bandwidth} chartColor="#38bdf8" chartUnit=" Mbps" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <MetricRow
              label="Candidate"
              value={candidateLabel(snap.network.candidateType)}
              colorClass={snap.network.candidateType === 'relay' ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            />
          </>
        ) : (
          <MetricRow label="Status" value="No session" dim />
        )}
      </Section>

      {/* Audio */}
      <Section>
        <SectionHeader>Audio</SectionHeader>
        {snap.audio ? (
          <>
            <MetricRow
              label="Local RMS"
              value={`${snap.audio.localRms.toFixed(3)}  (${snap.audio.localSpeaking ? 'speaking' : 'silent'})`}
            />
            <MetricRow
              label="Remote speaking"
              value={`${snap.audio.remoteSpeakingCount} / ${snap.audio.participantCount - 1}`}
            />
          </>
        ) : (
          <MetricRow label="Status" value="No session" dim />
        )}
      </Section>

      {/* Memory (Process tree) */}
      <Section>
        <SectionHeader>Memory — Process Tree</SectionHeader>
        {snap.rss ? (
          <>
            <ChartRow
              label="RSS"
              value={`${fmtMb(snap.rss.mb)}${delta(snap.rss.mb, baseSnap?.rss?.mb)}`}
              chartKey="rss" chartData={chartData.rss} chartColor="#a78bfa" chartUnit=" MB" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <MetricRow label="Child processes" value={String(snap.rss.childCount)} dim />
            <div className="text-[0.55rem] text-wavis-text-secondary/50 mt-0.5">
              Working Set on Windows · RSS on macOS/Linux
            </div>
          </>
        ) : (
          <MetricRow label="Status" value="Unavailable" dim />
        )}
        {snap.cpuPercent !== null ? (
          <ChartRow
            label="CPU"
            value={`${fmt1(snap.cpuPercent)}%`}
            chartKey="cpu" chartData={chartData.cpu} chartColor="#34d399" chartUnit="%" chartDomain={[0, 100]}
            openCharts={openCharts} onToggle={toggleChart} />
        ) : (
          <MetricRow label="CPU" value="Measuring…" dim />
        )}
      </Section>

      {/* Browser (main window) */}
      <Section>
        <SectionHeader>Browser — Main Window</SectionHeader>
        {snap.jsHeap ? (
          <ChartRow
            label="JS Heap"
            value={`${fmtMb(snap.jsHeap.usedMb)}${delta(snap.jsHeap.usedMb, baseSnap?.jsHeap?.usedMb)} / ${fmtMb(snap.jsHeap.totalMb)}`}
            chartKey="jsHeap" chartData={chartData.jsHeap} chartColor="#c084fc" chartUnit=" MB" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
        ) : (
          <MetricRow label="JS Heap" value="N/A (macOS)" dim />
        )}
        <ChartRow
          label="DOM nodes"
          value={`${snap.domNodes.toLocaleString()}${deltaInt(snap.domNodes, baseSnap?.domNodes)}`}
          chartKey="domNodes" chartData={chartData.domNodes} chartColor="#94a3b8" chartUnit="" chartDomain={[0, 'auto']}
          openCharts={openCharts} onToggle={toggleChart} />
      </Section>

      {/* Screen share capture */}
      <Section>
        <SectionHeader>Screen Share Capture (5s)</SectionHeader>
        {snap.share ? (
          <>
            <ChartRow
              label="Bitrate"
              value={`${fmt1(snap.share.bitrateKbps / 1000)} Mbps`}
              chartKey="shareBitrate" chartData={chartData.shareBitrate} chartColor="#fbbf24" chartUnit=" Mbps" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow
              label="FPS"
              value={fmt1(snap.share.fps)}
              chartKey="shareFps" chartData={chartData.shareFps} chartColor="#34d399" chartUnit=" fps" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <MetricRow
              label="Resolution"
              value={snap.share.frameWidth > 0 ? `${snap.share.frameWidth}×${snap.share.frameHeight}` : '—'}
              colorClass={snap.share.frameHeight > 0 && snap.share.frameHeight < 720 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            />
            <MetricRow
              label="Limit reason"
              value={snap.share.qualityLimitationReason || 'none'}
              colorClass={snap.share.qualityLimitationReason && snap.share.qualityLimitationReason !== 'none' ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            />
            <MetricRow
              label="Outbound loss"
              value={`${fmt1(snap.share.packetLossPercent)}%`}
            />
            <ChartRow
              label="PLIs / interval"
              value={String(snap.share.pliCount)}
              colorClass={snap.share.pliCount > 0 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
              chartKey="sharePli" chartData={chartData.sharePli} chartColor="#f87171" chartUnit=" pli" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <ChartRow
              label="NACKs / interval"
              value={String(snap.share.nackCount)}
              colorClass={snap.share.nackCount > 5 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
              chartKey="shareNack" chartData={chartData.shareNack} chartColor="#fb923c" chartUnit=" nack" chartDomain={[0, 'auto']}
              openCharts={openCharts} onToggle={toggleChart} />
            <MetricRow
              label="Avail. bandwidth"
              value={snap.share.availableBandwidthKbps > 0 ? `${(snap.share.availableBandwidthKbps / 1000).toFixed(1)} Mbps` : '—'}
            />
          </>
        ) : (
          <MetricRow label="Status" value="Not sharing" dim />
        )}
        {snap.shareStartedAt && (
          <div className="text-[0.6rem] text-wavis-text-secondary/60 mt-0.5">
            {snap.shareStoppedAt
              ? `Started ${snap.shareStartedAt} · Stopped ${snap.shareStoppedAt}`
              : `Started ${snap.shareStartedAt}`}
          </div>
        )}
      </Section>

      {/* Screen Share (Received) — viewer perspective, only shown when watching a share */}
      {snap.videoReceive && (
        <Section>
          <SectionHeader>Screen Share Received (10s)</SectionHeader>
          <ChartRow
            label="FPS"
            value={fmt1(snap.videoReceive.fps)}
            chartKey="recvFps" chartData={chartData.recvFps} chartColor="#34d399" chartUnit=" fps" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
          <MetricRow
            label="Resolution"
            value={snap.videoReceive.frameWidth > 0 ? `${snap.videoReceive.frameWidth}×${snap.videoReceive.frameHeight}` : '—'}
          />
          <ChartRow
            label="Inbound loss"
            value={`${fmt1(snap.videoReceive.packetLossPercent)}%`}
            colorClass={snap.videoReceive.packetLossPercent > 5 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            chartKey="recvLoss" chartData={chartData.recvLoss} chartColor="#f87171" chartUnit="%" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
          <ChartRow
            label="Jitter buffer"
            value={snap.videoReceive.jitterBufferDelayMs > 0 ? `${snap.videoReceive.jitterBufferDelayMs} ms` : '—'}
            chartKey="recvJitterBuf" chartData={chartData.recvJitterBuf} chartColor="#a78bfa" chartUnit=" ms" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
          <MetricRow
            label="Frames dropped"
            value={String(snap.videoReceive.framesDropped)}
            colorClass={snap.videoReceive.framesDropped > 0 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
          />
          <ChartRow
            label="Freeze events"
            value={snap.videoReceive.freezeCount > 0
              ? `${snap.videoReceive.freezeCount}  (${snap.videoReceive.freezeDurationMs} ms)`
              : '0'}
            colorClass={snap.videoReceive.freezeCount > 0 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            chartKey="recvFreeze" chartData={chartData.recvFreeze} chartColor="#fb923c" chartUnit=" evt" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
          <ChartRow
            label="PLIs sent"
            value={String(snap.videoReceive.pliCount)}
            colorClass={snap.videoReceive.pliCount > 0 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            chartKey="recvPli" chartData={chartData.recvPli} chartColor="#f87171" chartUnit=" pli" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
          <ChartRow
            label="NACKs sent"
            value={String(snap.videoReceive.nackCount)}
            colorClass={snap.videoReceive.nackCount > 5 ? 'text-wavis-warn font-mono tabular-nums' : undefined}
            chartKey="recvNack" chartData={chartData.recvNack} chartColor="#fb923c" chartUnit=" nack" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
          <ChartRow
            label="Avg decode"
            value={snap.videoReceive.avgDecodeTimeMs > 0 ? `${snap.videoReceive.avgDecodeTimeMs.toFixed(1)} ms` : '—'}
            chartKey="recvDecode" chartData={chartData.recvDecode} chartColor="#38bdf8" chartUnit=" ms" chartDomain={[0, 'auto']}
            openCharts={openCharts} onToggle={toggleChart} />
        </Section>
      )}

      {/* Warnings */}
      {activeWarnings.length > 0 && (
        <Section>
          <SectionHeader>Warnings</SectionHeader>
          <div className="flex flex-col gap-1">
            {activeWarnings.map((w) => (
              <div key={w.key} className="text-wavis-danger text-xs">
                ⚠ {w.message}
              </div>
            ))}
          </div>
        </Section>
      )}

      {/* Actions */}
      <div className="flex gap-2 flex-wrap">
        <ActionButton onClick={handleSetBaseline}>
          {bl ? 'Update Baseline' : 'Set Baseline'}
        </ActionButton>
        {bl && (
          <ActionButton onClick={handleClearBaseline}>Clear Baseline</ActionButton>
        )}
        <ActionButton onClick={handleCopySnapshot}>
          {copied ? 'Copied!' : 'Copy Snapshot'}
        </ActionButton>
      </div>

      {bl && (
        <div className="text-[0.6rem] text-wavis-text-secondary/60">
          Baseline set {new Date(bl.capturedAt).toLocaleTimeString()}
        </div>
      )}
    </div>
  );
}
