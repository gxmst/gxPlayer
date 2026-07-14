export type PlaybackStatus =
  | "idle"
  | "loading"
  | "playing"
  | "paused"
  | "buffering"
  | "stopped"
  | "failed";

export type QueueItem = {
  location: string;
  title: string;
  durationSeconds: number | null;
  online: boolean;
};

export type RuntimeStatus = {
  generation: number;
  state: "no_source" | "initializing" | "ready" | "failed";
  activeSourceId: string | null;
  capabilities: unknown;
  error: string | null;
  updateAlert?: unknown | null;
};

export type ListedSource = {
  id: string;
  scriptPath: string;
  origin: string;
  importedAtMs: number;
  metadata: {
    name: string;
    description: string;
    author: string;
    homepage: string;
    version: string;
  };
  updatesEnabled: boolean;
  enabled: boolean;
  preferred: boolean;
  userPriority: number;
  effectivePriority: number | null;
  hasConfig: boolean;
  capabilities: Array<{
    platform: string;
    qualities: string[];
  }>;
  health: SourceHealthSummary;
};

export type SourceHealthSummary = {
  state: "unknown" | "healthy" | "degraded" | "unhealthy";
  sampleCount: number;
  successCount: number;
  successRatePercent: number | null;
  averageLatencyMs: number | null;
  lastSuccess: boolean | null;
  lastLatencyMs: number | null;
  lastRecordedAtMs: number | null;
};

export type CatalogTrack = {
  providerId: string;
  providerTrackId: string;
  title: string;
  artist: string;
  album: string;
  durationMs: number | null;
  artworkUrl: string | null;
  resolverPayload: unknown;
  preview: unknown | null;
};

export type OnlinePlaybackResult = {
  outcome: "started" | "failed" | "cancelled" | "stale";
  attempts: ResolveAttemptDiagnostic[];
  error?: string | null;
  track: CatalogTrack;
  sourceId: string | null;
  sourceName: string | null;
  quality: string | null;
  cacheHit: boolean;
};

export type ResolveAttemptDiagnostic = {
  sourceId: string | null;
  sourceName: string | null;
  providerId: string;
  providerTrackId: string;
  quality: string | null;
  stage: string;
  success: boolean;
  error: string | null;
};

export type ProxyMode = "auto" | "on" | "off";

export type ProxyStatus = {
  mode: ProxyMode;
  detected: boolean;
  effective: boolean;
};

export type DiagnosticLogStatus = {
  enabled: boolean;
};

export type DiagnosticLogEntry = {
  timestampMs: number;
  category: string;
  source: string | null;
  summary: string;
};

export type DiagnosticLogExportResult = {
  path: string;
  entryCount: number;
};

export type CacheStatus = {
  revision: number;
  directory: string;
  customDirectory: string | null;
  limitBytes: number;
  totalBytes: number;
  entryCount: number;
  pinnedCount: number;
};

/** Offline/cache list row — never includes absolute disk paths. */
export type CacheEntryView = {
  providerId: string;
  providerTrackId: string;
  quality: string;
  title: string;
  artist: string;
  album: string;
  byteLen: number;
  sourceSampleRate: number | null;
  sourceBitDepth: number | null;
  sourceChannels: number | null;
  mediaType: string;
  pinned: boolean;
  lastAccessedAtMs: number;
  completedAtMs: number;
  fileName: string;
};

export type LyricDocument = {
  instrumental: boolean;
  lines: Array<{ timestampMs: number | null; text: string }>;
};

export type LibraryTrack = {
  id: number;
  path: string;
  title: string;
  artist: string;
  album: string;
  durationSeconds: number | null;
  favorite: boolean;
  addedAtMs: number;
  missing?: boolean;
};

export type LibraryImportResult = {
  imported: LibraryTrack[];
  failures: Array<{ path: string; error: string }>;
};

export type HistoryEntry = {
  id: number;
  playedAtMs: number;
  kind: string;
  title: string;
  artist: string;
  path: string | null;
  providerId: string | null;
  providerTrackId: string | null;
  quality: string | null;
};

export type PlaylistSummary = {
  id: number;
  name: string;
  trackCount: number;
  createdAtMs: number;
};

export type EqBand = {
  enabled: boolean;
  kind: "peak" | "low_shelf" | "high_shelf" | "low_pass" | "high_pass";
  frequencyHz: number;
  gainDb: number;
  q: number;
};

export type DspSettings = {
  enabled: boolean;
  eqEnabled: boolean;
  eqBands: EqBand[];
  crossfeed: { enabled: boolean; amount: number; delayMs: number; cutoffHz: number };
  hrtf: { enabled: boolean; mix: number; outputGainDb: number };
  limiter: { enabled: boolean; ceilingDb: number; releaseMs: number };
};

export type PlayMode = "sequential" | "repeat_all" | "repeat_one" | "shuffle";

export type EngineSnapshot = {
  status: PlaybackStatus;
  queue: QueueItem[];
  queueIndex: number | null;
  positionSeconds: number;
  durationSeconds: number | null;
  volume: number;
  audioMode: "music" | "cinema_game";
  playMode: PlayMode;
  dspSettings: DspSettings;
  generation: number;
  underrunCallbacks: number;
  outputSampleRate?: number | null;
  sourceSampleRate?: number | null;
  sourceBitDepth?: number | null;
  sourceChannels?: number | null;
  error: string | null;
  outputDevice?: string | null;
};

export type ViewId =
  | "discovery"
  | "search"
  | "artist"
  | "library"
  | "favorites"
  | "history"
  | "playlist"
  | "sources"
  | "settings"
  | "now-playing";

export const EMPTY_ENGINE: EngineSnapshot = {
  status: "idle",
  queue: [],
  queueIndex: null,
  positionSeconds: 0,
  durationSeconds: null,
  volume: 1,
  audioMode: "music",
  playMode: "sequential",
  dspSettings: {
    enabled: false,
    eqEnabled: false,
    eqBands: [{ enabled: true, kind: "peak", frequencyHz: 1000, gainDb: 0, q: 1 }],
    crossfeed: { enabled: false, amount: 0.18, delayMs: 0.28, cutoffHz: 700 },
    hrtf: { enabled: false, mix: 0.72, outputGainDb: -6 },
    limiter: { enabled: false, ceilingDb: -1, releaseMs: 80 },
  },
  generation: 0,
  underrunCallbacks: 0,
  sourceSampleRate: null,
  sourceBitDepth: null,
  sourceChannels: null,
  outputSampleRate: null,
  error: null,
  outputDevice: null,
};
