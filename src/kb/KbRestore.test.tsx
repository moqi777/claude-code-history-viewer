import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { BackupFolderLine, RestoreDialog } from "./KbRestore";
import { overviewFixture } from "./fixtures";
import type { KbBackupEntry, KbRestoreInspect, KbRestoreReport } from "./types";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  dialog: { open: vi.fn() },
  toast: { success: vi.fn(), error: vi.fn() },
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("@tauri-apps/plugin-dialog", () => mocks.dialog);
vi.mock("@/utils/platform", () => ({ isTauri: () => true }));
vi.mock("sonner", () => ({ toast: mocks.toast }));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const FOLDER = "/Volumes/Share/AgentKB-backup";

const info = (patch: Partial<KbRestoreInspect> = {}): KbRestoreInspect => ({
  sessions: 321,
  notes: 116,
  raw_files: 322,
  raw_bytes: 3.9e9,
  note_files: 22,
  snapshot_at: "2026-10-19T08:00:00Z",
  schema_version: 9,
  needs_upgrade: false,
  free_bytes: 50e9,
  needed_bytes: 5e9,
  current_sessions: 5,
  current_notes: 1,
  current_empty: false,
  ...patch,
});

const entry = (folder: string, patch: Partial<KbBackupEntry> = {}): KbBackupEntry => ({
  folder,
  name: "Mac",
  os: "macos",
  last_backup_at: "2026-10-19T08:00:00Z",
  sessions: 321,
  this_computer: true,
  old_layout: false,
  ...patch,
});

const report = (patch: Partial<KbRestoreReport> = {}): KbRestoreReport => ({
  sessions: 321,
  notes: 116,
  raw_files: 322,
  raw_bytes: 3.9e9,
  set_aside: "/Users/me/AgentKB/before-restore-20261019T090000Z",
  caught_up_files: 2,
  caught_up_bytes: 1000,
  missing_files: 0,
  rehomed_files: 0,
  warnings: [],
  duration_ms: 1000,
  ...patch,
});

const open = (configuredFolder: string | null = FOLDER) =>
  render(<RestoreDialog open onClose={vi.fn()} configuredFolder={configuredFolder} />);

describe("RestoreDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useKbStore.setState({ progress: null });
    mocks.api.restoreList.mockImplementation(async (folder: string) => [entry(folder)]);
    mocks.api.restoreInspect.mockResolvedValue(info());
    mocks.api.restoreStart.mockResolvedValue(report());
    mocks.api.syncNow.mockResolvedValue({});
    mocks.api.getStatus.mockRejectedValue(new Error("not needed"));
    mocks.api.getFacets.mockResolvedValue({ clients: [], projects: [] });
  });

  it("offers the disk that is set up, and looks at it without restoring", async () => {
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await waitFor(() => expect(screen.getByText(/kb\.restore\.summary/)).toBeTruthy());
    expect(mocks.api.restoreInspect).toHaveBeenCalledWith(FOLDER);
    expect(mocks.api.restoreStart).not.toHaveBeenCalled();
    expect(screen.getByText(/kb\.restore\.willSetAside /).textContent).toContain('"sessions":5');
  });

  it("starts only after the confirm button, on the folder that was looked at", async () => {
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await screen.findByText(/kb\.restore\.summary/);
    expect(mocks.api.restoreStart).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("kb.restore.confirm"));
    await waitFor(() => expect(mocks.api.restoreStart).toHaveBeenCalledTimes(1));
    expect(mocks.api.restoreStart).toHaveBeenCalledWith(FOLDER);
  });

  it("without a set-up disk the folder is picked by hand", async () => {
    mocks.dialog.open.mockResolvedValue("/Volumes/Other/AgentKB-backup");
    open(null);
    expect(screen.queryByText(/kb\.restore\.useConfigured/)).toBeNull();
    fireEvent.click(screen.getByText("kb.restore.pick"));
    await screen.findByText(/kb\.restore\.summary/);
    expect(mocks.api.restoreInspect).toHaveBeenCalledWith("/Volumes/Other/AgentKB-backup");
  });

  it("a cancelled folder chooser does nothing", async () => {
    mocks.dialog.open.mockResolvedValue(null);
    open(null);
    fireEvent.click(screen.getByText("kb.restore.pick"));
    await waitFor(() => expect(mocks.dialog.open).toHaveBeenCalled());
    expect(mocks.api.restoreInspect).not.toHaveBeenCalled();
  });

  it("when finished, points to the semantic setup and to collecting now", async () => {
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await screen.findByText(/kb\.restore\.summary/);
    fireEvent.click(screen.getByText("kb.restore.confirm"));
    await screen.findByText("kb.restore.semanticTitle");
    expect(screen.getByText("kb.restore.goSemantic")).toBeTruthy();
    fireEvent.click(screen.getByText("kb.restore.collect"));
    expect(mocks.api.syncNow).toHaveBeenCalled();
    expect(screen.getByText(/kb\.restore\.doneAside/)).toBeTruthy();
  });

  it("a refused backup shows why and offers to start again", async () => {
    mocks.api.restoreInspect.mockRejectedValue(new Error("backup_incomplete: the folder has no kb.db"));
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    const text = await screen.findByText(/kb\.restore\.error\.backup_incomplete/);
    expect(text.textContent).toContain("the folder has no kb.db");
    expect(screen.getByText("kb.transfer.import.again")).toBeTruthy();
    expect(mocks.api.restoreStart).not.toHaveBeenCalled();
  });

  it("a failed restore says so and the confirm button is gone", async () => {
    mocks.api.restoreStart.mockRejectedValue(new Error("busy: a collection is running"));
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await screen.findByText(/kb\.restore\.summary/);
    fireEvent.click(screen.getByText("kb.restore.confirm"));
    await screen.findByText(/kb\.restore\.error\.busy/);
    expect(screen.queryByText("kb.restore.confirm")).toBeNull();
  });

  it("will not start when the disk has too little room", async () => {
    mocks.api.restoreInspect.mockResolvedValue(info({ free_bytes: 1e9, needed_bytes: 5e9 }));
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await screen.findByText(/kb\.restore\.needSpace/);
    expect((screen.getByText("kb.restore.confirm").closest("button") as HTMLButtonElement).disabled).toBe(true);
  });

  it("when the folder holds several computers, asks which one", async () => {
    mocks.api.restoreList.mockResolvedValue([
      entry(`${FOLDER}/Mac-aaaaaaaa`, { name: "Mac" }),
      entry(`${FOLDER}/Win-bbbbbbbb`, { name: "Win", os: "windows", this_computer: false }),
      entry(FOLDER, { old_layout: true, name: "", this_computer: false }),
    ]);
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await screen.findByText("kb.restore.pickComputer");
    expect(mocks.api.restoreInspect).not.toHaveBeenCalled();
    expect(screen.getByText("kb.restore.oldLayout")).toBeTruthy();
    fireEvent.click(screen.getByText("Win"));
    await screen.findByText(/kb\.restore\.summary/);
    expect(mocks.api.restoreInspect).toHaveBeenCalledWith(`${FOLDER}/Win-bbbbbbbb`);
    expect(mocks.api.restoreStart).not.toHaveBeenCalled();
  });

  it("a folder with no backup in it is refused", async () => {
    mocks.api.restoreList.mockResolvedValue([]);
    open();
    fireEvent.click(screen.getByText(/kb\.restore\.useConfigured/));
    await screen.findByText(/kb\.restore\.error\.backup_incomplete/);
    expect(mocks.api.restoreInspect).not.toHaveBeenCalled();
  });
});

describe("BackupFolderLine", () => {
  it("shows the folder of this computer and a note about an older backup", () => {
    useKbStore.setState({
      overview: overviewFixture({
        mirror: { enabled: true, target_dir: "/Volumes/D/AgentKB-backup", volume_name: "D" },
        backup: {
          last_at: null,
          last_result: null,
          last_skipped_at: null,
          last_bytes: null,
          progress: null,
          folder: "/Volumes/D/AgentKB-backup/Mac-aaaaaaaa",
          old_layout: true,
        },
      }),
    });
    render(<BackupFolderLine />);
    expect(screen.getByText(/kb\.backupFolder\.line/).textContent).toContain("Mac-aaaaaaaa");
    expect(screen.getByText("kb.backupFolder.oldLayout")).toBeTruthy();
  });

  it("says the folder comes with the first backup", () => {
    useKbStore.setState({
      overview: overviewFixture({
        mirror: { enabled: true, target_dir: "/Volumes/D/AgentKB-backup", volume_name: "D" },
      }),
    });
    render(<BackupFolderLine />);
    expect(screen.getByText("kb.backupFolder.notYet")).toBeTruthy();
  });
});
