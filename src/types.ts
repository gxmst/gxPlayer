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
  active: boolean;
  hasConfig: boolean;
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
  track: CatalogTrack;
  sourceId: string | null;
  sourceName: string | null;
  quality: string | null;
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

export type EngineSnapshot = {
  status: PlaybackStatus;
  queue: QueueItem[];
  queueIndex: number | null;
  positionSeconds: number;
  durationSeconds: number | null;
  volume: number;
  audioMode: "music" | "cinema_game";
  dspSettings: DspSettings;
  generation: number;
  underrunCallbacks: number;
  outputSampleRate?: number | null;
  error: string | null;
  outputDevice?: string | null;
};

export type ViewId =
  | "discovery"
  | "search"
  | "library"
  | "favorites"
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
  outputSampleRate: null,
  error: null,
  outputDevice: null,
};
