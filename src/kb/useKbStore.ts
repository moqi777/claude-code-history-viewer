import { create } from "zustand";
import { kbApi } from "./api";
import type {
  KbOverview,
  KbRunSummary,
  KbSession,
  KbSessionQuery,
  KbSourceFilter,
} from "./types";

/**
 * Standalone store for the knowledge-base panel. Kept out of the base
 * `useAppStore` so the base slices and types stay untouched.
 *
 * Actions that can fail return an `{ error }` result instead of throwing so the
 * component decides how to tell the user (toast).
 */
export interface KbState {
  isOpen: boolean;
  overview: KbOverview | null;
  sessions: KbSession[];
  filter: KbSourceFilter;
  includeSubagents: boolean;
  includeEmpty: boolean;
  isLoading: boolean;
  isSyncing: boolean;
  isSchedulePending: boolean;
  loadError: string | null;
}

export interface KbActions {
  open: () => void;
  close: () => void;
  setFilter: (filter: KbSourceFilter) => Promise<void>;
  setIncludeSubagents: (include: boolean) => Promise<void>;
  setIncludeEmpty: (include: boolean) => Promise<void>;
  refresh: () => Promise<void>;
  syncNow: () => Promise<{ summary?: KbRunSummary; error?: string }>;
  toggleSchedule: () => Promise<{ error?: string }>;
}

const errorMessage = (e: unknown): string =>
  e instanceof Error ? e.message : String(e);

export const queryFor = (
  filter: KbSourceFilter,
  includeSubagents: boolean,
  includeEmpty = false,
): KbSessionQuery => ({
  include_subagents: includeSubagents,
  ...(includeEmpty ? { include_empty: true } : {}),
  ...(filter === "claude" || filter === "codex" ? { source: filter } : {}),
  ...(filter === "deleted" ? { only_deleted: true } : {}),
});

const initialState: KbState = {
  isOpen: false,
  overview: null,
  sessions: [],
  filter: "all",
  includeSubagents: false,
  includeEmpty: false,
  isLoading: false,
  isSyncing: false,
  isSchedulePending: false,
  loadError: null,
};

// Responses of overlapping refreshes can arrive out of order; only the newest
// request may write.
let refreshEpoch = 0;

export const useKbStore = create<KbState & KbActions>((set, get) => ({
  ...initialState,

  open: () => {
    set({ isOpen: true });
    void get().refresh();
  },

  close: () => set({ isOpen: false }),

  setFilter: async (filter) => {
    set({ filter });
    await get().refresh();
  },

  setIncludeSubagents: async (includeSubagents) => {
    set({ includeSubagents });
    await get().refresh();
  },

  setIncludeEmpty: async (includeEmpty) => {
    set({ includeEmpty });
    await get().refresh();
  },

  refresh: async () => {
    const epoch = ++refreshEpoch;
    set({ isLoading: true });
    try {
      const { filter, includeSubagents, includeEmpty } = get();
      const [overview, sessions] = await Promise.all([
        kbApi.getStatus(),
        kbApi.listSessions(queryFor(filter, includeSubagents, includeEmpty)),
      ]);
      if (epoch !== refreshEpoch) return;
      set({ overview, sessions, loadError: null, isLoading: false });
    } catch (e) {
      if (epoch !== refreshEpoch) return;
      set({ loadError: errorMessage(e), isLoading: false });
    }
  },

  syncNow: async () => {
    if (get().isSyncing) return { error: "busy" };
    set({ isSyncing: true });
    try {
      const summary = await kbApi.syncNow();
      await get().refresh();
      return { summary };
    } catch (e) {
      return { error: errorMessage(e) };
    } finally {
      set({ isSyncing: false });
    }
  },

  toggleSchedule: async () => {
    if (get().isSchedulePending) return {};
    set({ isSchedulePending: true });
    try {
      const installed = get().overview?.schedule.installed === true;
      await (installed ? kbApi.uninstallSchedule() : kbApi.installSchedule());
      await get().refresh();
      return {};
    } catch (e) {
      return { error: errorMessage(e) };
    } finally {
      set({ isSchedulePending: false });
    }
  },
}));
