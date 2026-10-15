import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { overviewFixture, sessionFixture } from "./fixtures";
import { KbHome } from "./KbHome";
import { configFixture, volumeFixture } from "./testkit";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
  open: vi.fn(),
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("sonner", () => ({ toast: mocks.toast }));
vi.mock("./openSession", () => ({ openKbSession: mocks.open }));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const deletedSession = sessionFixture({
  file_path: "/AgentKB/raw/codex/sessions/r.jsonl",
  session_id: "/AgentKB/raw/codex/sessions/r.jsonl",
  summary: "Gone from disk",
  provider: "codex",
  source_status: "deleted",
});

const mirrorOverview = (patch = {}) =>
  overviewFixture({
    mirror: { enabled: true, target_dir: "/Volumes/Backup1/AgentKB-backup", volume_name: "Backup1" },
    ...patch,
  });

const show = async () => {
  render(<KbHome />);
  await screen.findByText("kb.status.lastRun");
};

describe("KbHome", () => {
  beforeEach(() => {
    useKbStore.setState(useKbStore.getInitialState(), true);
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.open.mockReset();
    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    mocks.api.listSessions.mockResolvedValue([sessionFixture(), deletedSession]);
    mocks.api.getFacets.mockResolvedValue({ clients: ["cli", "exec"], projects: [["claude", "proj"]] });
    mocks.api.getBackupPresence.mockResolvedValue({ state: "disabled" });
    mocks.api.getConfigState.mockResolvedValue({
      state: "valid",
      config: configFixture(),
      config_path: "/x/config.json",
    });
  });

  it("shows status and marks sessions whose source was deleted", async () => {
    await show();
    expect(screen.getByText("/Users/me/AgentKB")).toBeTruthy();
    expect(screen.getByText(/kb\.status\.schedule\.active/)).toBeTruthy();
    expect(screen.getByText("Fix the login bug")).toBeTruthy();
    const badges = screen.getAllByText("kb.sessions.deletedBadge");
    expect(badges).toHaveLength(1);
    expect(badges[0]?.closest("button")?.textContent).toContain("Gone from disk");
    expect(screen.getByText(/kb\.version/).textContent).toContain("0.3.1");
  });

  it("opens a session through the viewer integration", async () => {
    mocks.open.mockResolvedValue(undefined);
    await show();
    fireEvent.click(screen.getByRole("button", { name: /Gone from disk/ }));
    await waitFor(() => expect(mocks.open).toHaveBeenCalledTimes(1));
    expect(mocks.open.mock.calls[0]?.[0].file_path).toBe(deletedSession.file_path);
  });

  it("tells the user when a session cannot be opened", async () => {
    mocks.open.mockRejectedValue(new Error("no such file"));
    await show();
    fireEvent.click(screen.getByRole("button", { name: /Gone from disk/ }));
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("no such file");
  });

  it("collect now: shows 'collecting', then reports the result", async () => {
    let finish: (v: unknown) => void = () => {};
    mocks.api.syncNow.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    await show();
    fireEvent.click(screen.getByRole("button", { name: "kb.action.syncNow" }));
    const busy = await screen.findByRole("button", { name: "kb.action.syncing" });
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    finish({ status: "ok", changed_files: 2, copied_bytes: 2048, errors: [] });
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalled());
    expect(mocks.toast.success.mock.calls[0]?.[0]).toContain("kb.toast.syncDone");
  });

  it("says so when another process holds the lock", async () => {
    await show();
    mocks.api.syncNow.mockResolvedValue({ status: "skipped", reason: "locked", errors: [] });
    fireEvent.click(screen.getByRole("button", { name: "kb.action.syncNow" }));
    await waitFor(() => expect(mocks.toast.info).toHaveBeenCalledWith("kb.toast.syncBusy"));
  });

  it("a failed collection is reported to the user", async () => {
    mocks.api.syncNow.mockRejectedValue(new Error("disk full"));
    await show();
    fireEvent.click(screen.getByRole("button", { name: "kb.action.syncNow" }));
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("disk full");
  });

  it("filters go through the backend", async () => {
    await show();
    fireEvent.click(screen.getByLabelText("kb.filters.onlyDeleted"));
    await waitFor(() =>
      expect(mocks.api.listSessions).toHaveBeenLastCalledWith({
        include_subagents: false,
        only_deleted: true,
      }),
    );
    fireEvent.change(screen.getByLabelText("kb.filters.source"), { target: { value: "codex" } });
    await waitFor(() =>
      expect(mocks.api.listSessions).toHaveBeenLastCalledWith({
        include_subagents: false,
        only_deleted: true,
        source: "codex",
      }),
    );
    fireEvent.change(screen.getByLabelText("kb.filters.client"), { target: { value: "cli" } });
    await waitFor(() =>
      expect(mocks.api.listSessions.mock.calls.at(-1)?.[0]).toMatchObject({ client: "cli" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "kb.filters.reset" }));
    await waitFor(() =>
      expect(mocks.api.listSessions).toHaveBeenLastCalledWith({ include_subagents: false }),
    );
  });

  it("surfaces load errors", async () => {
    mocks.api.getStatus.mockRejectedValue(new Error("db locked"));
    render(<KbHome />);
    expect((await screen.findByText(/db locked/)).textContent).toContain("db locked");
  });

  // ---- backup ----

  it("hides the backup button when no disk is configured", async () => {
    await show();
    expect(screen.queryByRole("button", { name: "kb.action.backupNow" })).toBeNull();
  });

  it("backup now is disabled while the disk is away and enabled when connected", async () => {
    mocks.api.getStatus.mockResolvedValue(mirrorOverview());
    mocks.api.getBackupPresence.mockResolvedValue({ state: "not_connected", reason: "x" });
    await show();
    const button = () => screen.getByRole("button", { name: "kb.action.backupNow" }) as HTMLButtonElement;
    expect(button().disabled).toBe(true);
    expect(button().title).toBe("kb.action.backupNeedsDisk");

    mocks.api.getBackupPresence.mockResolvedValue({ state: "connected", volume: volumeFixture });
    await act(() => useKbStore.getState().refreshPresence());
    expect(button().disabled).toBe(false);
    mocks.api.backupNow.mockResolvedValue({
      status: "ok", copied_files: 2, appended_files: 1, copied_bytes: 4096, errors: [],
    });
    fireEvent.click(button());
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalled());
    expect(mocks.toast.success.mock.calls[0]?.[0]).toContain("kb.toast.backupDone");
    expect(mocks.api.backupNow).toHaveBeenCalledTimes(1);
  });

  it("distinguishes collecting from backing up and explains the disabled collect button", async () => {
    mocks.api.getStatus.mockResolvedValue(
      mirrorOverview({
        running: true,
        activity: "backing_up",
        backup: { last_at: null, last_result: null, last_skipped_at: null, last_bytes: null, progress: [50, 200] },
      }),
    );
    mocks.api.getBackupPresence.mockResolvedValue({ state: "connected", volume: volumeFixture });
    await show();
    expect(screen.getAllByText(/kb\.status\.activity\.backingUp/)[0]?.textContent).toContain('"percent":25');
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("25");
    const collect = screen.getByRole("button", { name: "kb.action.syncNow" }) as HTMLButtonElement;
    expect(collect.disabled).toBe(true);
    expect(collect.title).toBe("kb.action.syncDisabledDuringBackup");
    expect(screen.queryByText("kb.status.activity.collecting")).toBeNull();
  });

  it("warns when the disk has not been backed up for a week", async () => {
    const old = new Date(Date.now() - 9 * 24 * 3600 * 1000).toISOString();
    mocks.api.getStatus.mockResolvedValue(
      mirrorOverview({
        backup: { last_at: old, last_result: "ok", last_skipped_at: null, last_bytes: 0, progress: null },
      }),
    );
    await show();
    expect(screen.getByText(/kb\.status\.backup\.stale/).textContent).toContain('"days":9');
  });

  // ---- schedule ----

  it("flags a dead scheduled task and repairs it on click", async () => {
    mocks.api.getStatus.mockResolvedValue(
      overviewFixture({
        schedule: { ...overviewFixture().schedule, program_exists: false, plist_program: "/gone/AgentKB" },
      }),
    );
    mocks.api.installSchedule.mockResolvedValue({});
    await show();
    expect(screen.getByText("kb.status.schedule.dead")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "kb.action.repairSchedule" }));
    await waitFor(() => expect(mocks.api.installSchedule).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalledWith("kb.toast.repaired"));
  });

  it("shows the 'moved to a new location' notice once and lets the user dismiss it", async () => {
    mocks.api.getStatus.mockResolvedValue(
      overviewFixture({
        schedule_notice: { at: "2026-10-14T10:00:00Z", from: "/old/AgentKB", to: "/Applications/AgentKB.app/x", reason: "the app moved" },
      }),
    );
    mocks.api.dismissNotice.mockResolvedValue(undefined);
    await show();
    expect(screen.getByText(/kb\.notice\.scheduleRepaired/).textContent).toContain("/Applications/AgentKB.app/x");
    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    fireEvent.click(screen.getByRole("button", { name: "kb.notice.dismiss" }));
    await waitFor(() => expect(mocks.api.dismissNotice).toHaveBeenCalledWith("schedule_repaired"));
    await waitFor(() => expect(screen.queryByText(/kb\.notice\.scheduleRepaired/)).toBeNull());
  });

  // ---- config states ----

  it("a damaged config shows a banner with a restore button that reopens the wizard", async () => {
    mocks.api.getConfigState
      .mockResolvedValueOnce({ state: "invalid", message: "not valid JSON", path: "/x", config: configFixture(), config_path: "/x" })
      .mockResolvedValue({ state: "missing", config: configFixture(), config_path: "/x" });
    mocks.api.restoreDefaultConfig.mockResolvedValue("/x/config.json.corrupt-1");
    await show();
    expect((await screen.findByText(/kb\.banner\.configInvalid/)).textContent).toContain("not valid JSON");
    fireEvent.click(screen.getByRole("button", { name: "kb.banner.restoreDefaults" }));
    await waitFor(() => expect(mocks.api.restoreDefaultConfig).toHaveBeenCalled());
    await waitFor(() => expect(useKbStore.getState().wizard.open).toBe(true));
  });

  it("without a config the page offers to start the wizard", async () => {
    mocks.api.getConfigState.mockResolvedValue({ state: "missing", config: configFixture(), config_path: "/x" });
    await show();
    fireEvent.click(await screen.findByRole("button", { name: "kb.banner.startWizard" }));
    expect(useKbStore.getState().wizard).toEqual({ open: true, mode: "first" });
  });

  it("settings opens from the header button", async () => {
    await show();
    fireEvent.click(screen.getByRole("button", { name: "kb.action.settings" }));
    expect(useKbStore.getState().settingsOpen).toBe(true);
  });
});
