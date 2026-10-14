import { api } from "@/services/api";
import type {
  KbOverview,
  KbRunSummary,
  KbScheduleStatus,
  KbSession,
  KbSessionQuery,
} from "./types";

export const kbApi = {
  getStatus: () => api<KbOverview>("kb_get_status"),
  syncNow: () => api<KbRunSummary>("kb_sync_now"),
  listSessions: (filter: KbSessionQuery) =>
    api<KbSession[]>("kb_list_sessions", { filter }),
  installSchedule: () => api<KbScheduleStatus>("kb_schedule_install"),
  uninstallSchedule: () => api<KbScheduleStatus>("kb_schedule_uninstall"),
};
