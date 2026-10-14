import { create } from "zustand";
import { kbApi } from "./api";
import type {
  KbBackupPresence,
  KbBackupSummary,
  KbConfigStateInfo,
  KbFacets,
  KbFilters,
  KbOverview,
  KbProgress,
  KbRunSummary,
  KbSession,
  KbSessionQuery,
} from "./types";

/**
 * Standalone store for the knowledge-base page, settings and wizard. Kept out
 * of the base `useAppStore` so the base slices and types stay untouched.
 *
 * Actions that can fail return an `{ error }` result instead of throwing so the
 * component decides how to tell the user (toast).
 */
export type WizardMode = "first" | "rerun";

export interface KbState {
  overview: KbOverview | null;
  sessions: KbSession[];
  facets: KbFacets;
  presence: KbBackupPresence | null;
  configState: KbConfigStateInfo | null;
  filters: KbFilters;
  progress: KbProgress | null;
  isLoading: boolean;
  isSyncing: boolean;
  isBackingUp: boolean;
  isRepairing: boolean;
  loadError: string | null;
  wizard: { open: boolean; mode: WizardMode };
  settingsOpen: boolean;
}

export interface KbActions {
  refresh: () => Promise<void>;
  refreshPresence: () => Promise<void>;
  loadFacets: () => Promise<void>;
  loadConfigState: () => Promise<KbConfigStateInfo | null>;
  setFilters: (patch: Partial<KbFilters>) => Promise<void>;
  syncNow: () => Promise<{ summary?: KbRunSummary; error?: string }>;
  backupNow: () => Promise<{ summary?: KbBackupSummary; error?: string }>;
  repairSchedule: () => Promise<{ error?: string }>;
  dismissScheduleNotice: () => Promise<void>;
  restoreDefaultConfig: () => Promise<{ error?: string }>;
  setProgress: (progress: KbProgress | null) => void;
  openWizard: (mode: WizardMode) => void;
  closeWizard: () => void;
  openSettings: () => void;
  closeSettings: () => void;
}

const errorMessage = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export const defaultFilters: KbFilters = {
  source: "all",
  client: null,
  project: null,
  range: "all",
  onlyDeleted: false,
  includeSubagents: false,
  includeEmpty: false,
  query: "",
};

const DAY_MS = 24 * 60 * 60 * 1000;

export const queryFor = (filters: KbFilters, now = Date.now()): KbSessionQuery => {
  const days = filters.range === "7d" ? 7 : filters.range === "30d" ? 30 : 0;
  const query = filters.query.trim();
  return {
    include_subagents: filters.includeSubagents,
    ...(filters.includeEmpty ? { include_empty: true } : {}),
    ...(filters.source !== "all" ? { source: filters.source } : {}),
    ...(filters.onlyDeleted ? { only_deleted: true } : {}),
    ...(filters.client != null ? { client: filters.client } : {}),
    ...(filters.project != null ? { project: filters.project } : {}),
    ...(days > 0 ? { since: new Date(now - days * DAY_MS).toISOString() } : {}),
    ...(query !== "" ? { query } : {}),
  };
};

const initialState: KbState = {
  overview: null,
  sessions: [],
  facets: { clients: [], projects: [] },
  presence: null,
  configState: null,
  filters: defaultFilters,
  progress: null,
  isLoading: false,
  isSyncing: false,
  isBackingUp: false,
  isRepairing: false,
  loadError: null,
  wizard: { open: false, mode: "first" },
  settingsOpen: false,
};

// Responses of overlapping refreshes can arrive out of order; only the newest
// request may write.
let refreshEpoch = 0;

export const useKbStore = create<KbState & KbActions>((set, get) => ({
  ...initialState,

  refresh: async () => {
    const epoch = ++refreshEpoch;
    set({ isLoading: true });
    try {
      const [overview, sessions] = await Promise.all([
        kbApi.getStatus(),
        kbApi.listSessions(queryFor(get().filters)),
      ]);
      if (epoch !== refreshEpoch) return;
      set({ overview, sessions, loadError: null, isLoading: false });
    } catch (e) {
      if (epoch !== refreshEpoch) return;
      set({ loadError: errorMessage(e), isLoading: false });
    }
  },

  refreshPresence: async () => {
    try {
      set({ presence: await kbApi.getBackupPresence() });
    } catch {
      set({ presence: null });
    }
  },

  loadFacets: async () => {
    try {
      set({ facets: await kbApi.getFacets() });
    } catch {
      /* the filters just stay empty */
    }
  },

  loadConfigState: async () => {
    try {
      const configState = await kbApi.getConfigState();
      set({ configState });
      return configState;
    } catch (e) {
      set({ loadError: errorMessage(e) });
      return null;
    }
  },

  setFilters: async (patch) => {
    set({ filters: { ...get().filters, ...patch } });
    await get().refresh();
  },

  syncNow: async () => {
    if (get().isSyncing) return { error: "busy" };
    set({ isSyncing: true });
    try {
      const summary = await kbApi.syncNow();
      await Promise.all([get().refresh(), get().loadFacets(), get().refreshPresence()]);
      return { summary };
    } catch (e) {
      return { error: errorMessage(e) };
    } finally {
      set({ isSyncing: false, progress: null });
    }
  },

  backupNow: async () => {
    if (get().isBackingUp) return { error: "busy" };
    set({ isBackingUp: true });
    try {
      const summary = await kbApi.backupNow();
      await Promise.all([get().refresh(), get().refreshPresence()]);
      return { summary };
    } catch (e) {
      return { error: errorMessage(e) };
    } finally {
      set({ isBackingUp: false, progress: null });
    }
  },

  repairSchedule: async () => {
    if (get().isRepairing) return {};
    set({ isRepairing: true });
    try {
      await kbApi.installSchedule();
      await get().refresh();
      return {};
    } catch (e) {
      return { error: errorMessage(e) };
    } finally {
      set({ isRepairing: false });
    }
  },

  dismissScheduleNotice: async () => {
    try {
      await kbApi.dismissNotice("schedule_repaired");
    } finally {
      await get().refresh();
    }
  },

  restoreDefaultConfig: async () => {
    try {
      await kbApi.restoreDefaultConfig();
      await get().loadConfigState();
      return {};
    } catch (e) {
      return { error: errorMessage(e) };
    }
  },

  setProgress: (progress) => set({ progress }),
  openWizard: (mode) => set({ wizard: { open: true, mode } }),
  closeWizard: () => set({ wizard: { ...get().wizard, open: false } }),
  openSettings: () => set({ settingsOpen: true }),
  closeSettings: () => set({ settingsOpen: false }),
}));
