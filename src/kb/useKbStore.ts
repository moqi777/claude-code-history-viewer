import { create } from "zustand";
import { kbApi } from "./api";
import type {
  KbDigestRunRequest,
  KbDigestRunStarted,
  KbDigestStatus,
  KbBackupPresence,
  KbBackupSummary,
  KbConfigStateInfo,
  KbFacets,
  KbFilters,
  KbOverview,
  KbProgress,
  KbRunSummary,
  KbSearchParams,
  KbSearchResponse,
  KbSession,
  KbSessionQuery,
  KbSkillStatus,
  KbSkillTarget,
} from "./types";

/**
 * Standalone store for the knowledge-base page, settings and wizard. Kept out
 * of the base `useAppStore` so the base slices and types stay untouched.
 *
 * Actions that can fail return an `{ error }` result instead of throwing so the
 * component decides how to tell the user (toast).
 */
export type WizardMode = "first" | "rerun";

export type KbSearchStatus = "idle" | "loading" | "ok" | "not_ready" | "no_data" | "error";

export type KbHomeTab = "sessions" | "notes";

export interface KbSearchState {
  query: string;
  status: KbSearchStatus;
  response: KbSearchResponse | null;
  message: string | null;
}

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
  search: KbSearchState;
  skills: KbSkillStatus[] | null;
  digest: KbDigestStatus | null;
  homeTab: KbHomeTab;
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
  setSearchQuery: (query: string) => void;
  runSearch: () => Promise<void>;
  loadSkills: () => Promise<void>;
  installSkill: (target: KbSkillTarget) => Promise<{ error?: string }>;
  uninstallSkill: (target: KbSkillTarget) => Promise<{ error?: string }>;
  refreshDigest: () => Promise<void>;
  setHomeTab: (tab: KbHomeTab) => void;
  /** Start a manual digest (its own process); the card follows its progress. */
  runDigest: (request: KbDigestRunRequest) => Promise<{ started?: KbDigestRunStarted; error?: string }>;
  cancelDigest: () => Promise<{ error?: string }>;
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

export const searchParamsFor = (query: string, filters: KbFilters, now = Date.now()): KbSearchParams => {
  const days = filters.range === "7d" ? 7 : filters.range === "30d" ? 30 : 0;
  return {
    query: query.trim(),
    limit: 20,
    per_session: 2,
    include_subagents: filters.includeSubagents,
    ...(filters.source !== "all" ? { source: filters.source } : {}),
    ...(filters.client != null ? { client: filters.client } : {}),
    ...(filters.project != null ? { project: filters.project } : {}),
    ...(days > 0 ? { since: new Date(now - days * DAY_MS).toISOString() } : {}),
  };
};

const idleSearch: KbSearchState = { query: "", status: "idle", response: null, message: null };

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
  search: idleSearch,
  skills: null,
  digest: null,
  homeTab: "sessions",
};

// Responses of overlapping refreshes can arrive out of order; only the newest
// request may write.
let refreshEpoch = 0;
let searchEpoch = 0;

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
    await Promise.all([get().refresh(), get().search.query.trim() !== "" ? get().runSearch() : undefined]);
  },

  setSearchQuery: (query) => {
    if (query.trim() === "") {
      searchEpoch++;
      set({ search: { ...idleSearch, query } });
    } else {
      set({ search: { ...get().search, query } });
    }
  },

  runSearch: async () => {
    const { query } = get().search;
    if (query.trim() === "") return;
    const epoch = ++searchEpoch;
    set({ search: { ...get().search, status: "loading", message: null } });
    try {
      const outcome = await kbApi.search(searchParamsFor(query, get().filters));
      // A newer search (or an emptied box) supersedes this answer.
      if (epoch !== searchEpoch) return;
      if (outcome.state === "ok") {
        set({ search: { query, status: "ok", response: outcome.response, message: null } });
      } else {
        set({ search: { query, status: outcome.state, response: null, message: outcome.message } });
      }
    } catch (e) {
      if (epoch !== searchEpoch) return;
      set({ search: { query, status: "error", response: null, message: errorMessage(e) } });
    }
  },

  loadSkills: async () => {
    try {
      set({ skills: await kbApi.skillStatus() });
    } catch {
      set({ skills: null });
    }
  },

  installSkill: async (target) => {
    try {
      set({ skills: await kbApi.skillInstall(target) });
      return {};
    } catch (e) {
      return { error: errorMessage(e) };
    }
  },

  uninstallSkill: async (target) => {
    try {
      set({ skills: await kbApi.skillUninstall(target) });
      return {};
    } catch (e) {
      return { error: errorMessage(e) };
    }
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

  refreshDigest: async () => {
    try {
      set({ digest: (await kbApi.digestStatus()) ?? null });
    } catch {
      /* the card keeps what it last showed */
    }
  },

  setHomeTab: (homeTab) => set({ homeTab }),

  runDigest: async (request) => {
    try {
      const started = await kbApi.digestRun(request);
      await get().refreshDigest();
      return { started };
    } catch (e) {
      return { error: errorMessage(e) };
    }
  },

  cancelDigest: async () => {
    try {
      await kbApi.digestCancel();
      await get().refreshDigest();
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
