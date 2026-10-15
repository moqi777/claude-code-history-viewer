import { beforeEach, describe, expect, it, vi } from "vitest";
import { overviewFixture, sessionFixture } from "./fixtures";
import { configFixture } from "./testkit";
import { defaultFilters, queryFor, useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({ current: {} as ReturnType<typeof import("./testkit").apiMocks> }));
vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.current = apiMocks();
  return { kbApi: mocks.current };
});

const api = () => mocks.current;

const reset = () => {
  useKbStore.setState(useKbStore.getInitialState(), true);
  for (const mock of Object.values(api())) mock.mockReset();
  api().getStatus.mockResolvedValue(overviewFixture());
  api().listSessions.mockResolvedValue([sessionFixture()]);
  api().getFacets.mockResolvedValue({ clients: [], projects: [] });
  api().getBackupPresence.mockResolvedValue({ state: "disabled" });
};

describe("useKbStore", () => {
  beforeEach(reset);

  it("builds the backend query from the filters", () => {
    const now = Date.parse("2026-10-14T00:00:00Z");
    expect(queryFor(defaultFilters, now)).toEqual({ include_subagents: false });
    expect(queryFor({ ...defaultFilters, source: "codex", includeSubagents: true }, now)).toEqual({
      include_subagents: true,
      source: "codex",
    });
    expect(queryFor({ ...defaultFilters, onlyDeleted: true }, now)).toEqual({
      include_subagents: false,
      only_deleted: true,
    });
    expect(queryFor({ ...defaultFilters, includeEmpty: true }, now)).toEqual({
      include_subagents: false,
      include_empty: true,
    });
    expect(
      queryFor({ ...defaultFilters, client: "cli", project: "p", query: " fix ", range: "7d" }, now),
    ).toEqual({
      include_subagents: false,
      client: "cli",
      project: "p",
      query: "fix",
      since: "2026-10-07T00:00:00.000Z",
    });
  });

  it("refresh loads status and sessions", async () => {
    await useKbStore.getState().refresh();
    expect(useKbStore.getState().sessions).toHaveLength(1);
    expect(useKbStore.getState().overview?.data_dir).toBe("/Users/me/AgentKB");
    expect(useKbStore.getState().loadError).toBeNull();
  });

  it("a failed load is reported in state, not thrown", async () => {
    api().getStatus.mockRejectedValue(new Error("db locked"));
    await useKbStore.getState().refresh();
    expect(useKbStore.getState().loadError).toBe("db locked");
    expect(useKbStore.getState().isLoading).toBe(false);
  });

  it("only the newest overlapping refresh may write", async () => {
    let releaseFirst: (v: unknown) => void = () => {};
    api()
      .listSessions.mockImplementationOnce(() => new Promise((resolve) => { releaseFirst = resolve; }))
      .mockResolvedValueOnce([sessionFixture({ summary: "newest" })]);
    const first = useKbStore.getState().refresh();
    await useKbStore.getState().refresh();
    releaseFirst([sessionFixture({ summary: "stale" })]);
    await first;
    expect(useKbStore.getState().sessions[0]?.summary).toBe("newest");
  });

  it("changing a filter re-queries with it", async () => {
    await useKbStore.getState().setFilters({ onlyDeleted: true });
    expect(api().listSessions).toHaveBeenLastCalledWith({
      include_subagents: false,
      only_deleted: true,
    });
    await useKbStore.getState().setFilters({ includeEmpty: true });
    expect(api().listSessions).toHaveBeenLastCalledWith({
      include_subagents: false,
      include_empty: true,
      only_deleted: true,
    });
  });

  it("syncNow runs once at a time, refreshes, and reports failures as a result", async () => {
    api().syncNow.mockResolvedValue({ status: "ok", changed_files: 1, errors: [] });
    const first = useKbStore.getState().syncNow();
    expect(useKbStore.getState().isSyncing).toBe(true);
    expect(await useKbStore.getState().syncNow()).toEqual({ error: "busy" });
    expect((await first).summary?.status).toBe("ok");
    expect(api().syncNow).toHaveBeenCalledTimes(1);
    expect(useKbStore.getState().isSyncing).toBe(false);

    api().syncNow.mockRejectedValue("boom");
    expect(await useKbStore.getState().syncNow()).toEqual({ error: "boom" });
    expect(useKbStore.getState().isSyncing).toBe(false);
  });

  it("backupNow is serialised, refreshes and reports a failure as a result", async () => {
    api().backupNow.mockResolvedValue({ status: "ok", copied_files: 3, errors: [] });
    const first = useKbStore.getState().backupNow();
    expect(await useKbStore.getState().backupNow()).toEqual({ error: "busy" });
    expect((await first).summary?.copied_files).toBe(3);
    api().backupNow.mockRejectedValue(new Error("disk gone"));
    expect(await useKbStore.getState().backupNow()).toEqual({ error: "disk gone" });
    expect(useKbStore.getState().isBackingUp).toBe(false);
  });

  it("repairSchedule reinstalls the task and surfaces a failure", async () => {
    api().installSchedule.mockResolvedValue({});
    expect(await useKbStore.getState().repairSchedule()).toEqual({});
    expect(api().installSchedule).toHaveBeenCalledTimes(1);
    api().installSchedule.mockRejectedValue(new Error("launchctl failed"));
    expect(await useKbStore.getState().repairSchedule()).toEqual({ error: "launchctl failed" });
    expect(useKbStore.getState().isRepairing).toBe(false);
  });

  it("dismissing the schedule notice asks the backend then refreshes", async () => {
    api().dismissNotice.mockResolvedValue(undefined);
    await useKbStore.getState().dismissScheduleNotice();
    expect(api().dismissNotice).toHaveBeenCalledWith("schedule_repaired");
    expect(api().getStatus).toHaveBeenCalled();
  });

  it("the wizard and settings open and close without touching the config", () => {
    useKbStore.getState().openWizard("rerun");
    expect(useKbStore.getState().wizard).toEqual({ open: true, mode: "rerun" });
    useKbStore.getState().closeWizard();
    expect(useKbStore.getState().wizard.open).toBe(false);
    useKbStore.getState().openSettings();
    expect(useKbStore.getState().settingsOpen).toBe(true);
    expect(api().saveConfig).not.toHaveBeenCalled();
  });

  it("restoring defaults sets the broken file aside, then reloads the state", async () => {
    api().restoreDefaultConfig.mockResolvedValue("/x/config.json.corrupt-1");
    api().getConfigState.mockResolvedValue({
      state: "missing",
      config: configFixture(),
      config_path: "/x/config.json",
    });
    expect(await useKbStore.getState().restoreDefaultConfig()).toEqual({});
    expect(useKbStore.getState().configState?.state).toBe("missing");
  });

  describe("digest", () => {
    it("keeps the status the card shows and survives a failed or empty poll", async () => {
      api().digestStatus.mockResolvedValueOnce({ enabled: true, failures: [] });
      await useKbStore.getState().refreshDigest();
      expect(useKbStore.getState().digest).toMatchObject({ enabled: true });
      api().digestStatus.mockRejectedValueOnce(new Error("locked"));
      await useKbStore.getState().refreshDigest();
      expect(useKbStore.getState().digest).toMatchObject({ enabled: true });
      api().digestStatus.mockResolvedValueOnce(undefined);
      await useKbStore.getState().refreshDigest();
      expect(useKbStore.getState().digest).toBeNull();
    });

    it("starts a manual digest and then refreshes the card", async () => {
      api().digestRun.mockResolvedValue({ state: "started" });
      api().digestStatus.mockResolvedValue({ enabled: true, running: { total: 3 } });
      const out = await useKbStore.getState().runDigest({ kind: "all" });
      expect(out.started).toEqual({ state: "started" });
      expect(api().digestRun).toHaveBeenCalledWith({ kind: "all" });
      expect(useKbStore.getState().digest?.running).toMatchObject({ total: 3 });
    });

    it("reports a failure to start instead of throwing", async () => {
      api().digestRun.mockRejectedValue(new Error("no such session"));
      expect(await useKbStore.getState().runDigest({ kind: "all" })).toEqual({ error: "no such session" });
      api().digestCancel.mockRejectedValue(new Error("nothing running"));
      expect(await useKbStore.getState().cancelDigest()).toEqual({ error: "nothing running" });
    });

    it("remembers which home tab is open", () => {
      expect(useKbStore.getState().homeTab).toBe("sessions");
      useKbStore.getState().setHomeTab("notes");
      expect(useKbStore.getState().homeTab).toBe("notes");
    });
  });
});
