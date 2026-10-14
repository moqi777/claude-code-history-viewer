import { vi } from "vitest";
import type { KbConfig } from "./types";

/** Fresh mock functions for every `kbApi` method. */
export const apiMocks = () => ({
  getStatus: vi.fn(),
  getBackupPresence: vi.fn(),
  syncNow: vi.fn(),
  backupNow: vi.fn(),
  listSessions: vi.fn(),
  getFacets: vi.fn(),
  dismissNotice: vi.fn(),
  installSchedule: vi.fn(),
  uninstallSchedule: vi.fn(),
  getConfigState: vi.fn(),
  getConfig: vi.fn(),
  saveConfig: vi.fn(),
  restoreDefaultConfig: vi.fn(),
  probeSources: vi.fn(),
  testDataDir: vi.fn(),
  testScheduleEnv: vi.fn(),
  listExternalVolumes: vi.fn(),
  testMirror: vi.fn(),
  wizardCommit: vi.fn(),
  changeDataDirCheck: vi.fn(),
  changeDataDirRun: vi.fn(),
});

/** `t` that makes assertions readable: `key {"param":1}`. */
export const fakeT = (key: string, options?: Record<string, unknown>): string =>
  options == null ? key : `${key} ${JSON.stringify(options)}`;

export const configFixture = (patch: Partial<KbConfig> = {}): KbConfig => ({
  version: 1,
  data_dir: "/Users/me/AgentKB",
  interval_secs: 600,
  schedule_enabled: true,
  sources: {
    claude: { enabled: true, root: null },
    codex: { enabled: true, root: null },
  },
  exclude_prefixes: [],
  mirror: { enabled: false, target_dir: null, volume_uuid: null, volume_name: null },
  ...patch,
});

export const volumeFixture = {
  uuid: "D62F7387-D951-4AF4-9536-94103B9E94E9",
  mount_point: "/Volumes/Backup1",
  name: "Backup1",
  internal: false,
  fs_type: "exfat",
  writable: true,
};
