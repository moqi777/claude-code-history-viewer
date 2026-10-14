import { beforeEach, describe, expect, it, vi } from "vitest";
import { overviewFixture, sessionFixture } from "./fixtures";
import { queryFor, useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  getStatus: vi.fn(),
  syncNow: vi.fn(),
  listSessions: vi.fn(),
  installSchedule: vi.fn(),
  uninstallSchedule: vi.fn(),
}));
vi.mock("./api", () => ({ kbApi: mocks }));

const reset = () => {
  useKbStore.setState(useKbStore.getInitialState(), true);
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.getStatus.mockResolvedValue(overviewFixture());
  mocks.listSessions.mockResolvedValue([sessionFixture()]);
};

describe("useKbStore", () => {
  beforeEach(reset);

  it("builds the backend query from the filter", () => {
    expect(queryFor("all", false)).toEqual({ include_subagents: false });
    expect(queryFor("codex", true)).toEqual({ include_subagents: true, source: "codex" });
    expect(queryFor("deleted", false)).toEqual({ include_subagents: false, only_deleted: true });
    expect(queryFor("all", false, true)).toEqual({ include_subagents: false, include_empty: true });
  });

  it("open() shows the panel and loads status and sessions", async () => {
    useKbStore.getState().open();
    expect(useKbStore.getState().isOpen).toBe(true);
    await vi.waitFor(() => expect(useKbStore.getState().sessions).toHaveLength(1));
    expect(useKbStore.getState().overview?.data_dir).toBe("/Users/me/AgentKB");
    expect(useKbStore.getState().loadError).toBeNull();
  });

  it("a failed load is reported in state, not thrown", async () => {
    mocks.getStatus.mockRejectedValue(new Error("db locked"));
    await useKbStore.getState().refresh();
    expect(useKbStore.getState().loadError).toBe("db locked");
    expect(useKbStore.getState().isLoading).toBe(false);
  });

  it("only the newest overlapping refresh may write", async () => {
    let releaseFirst: (v: unknown) => void = () => {};
    mocks.listSessions
      .mockImplementationOnce(() => new Promise((resolve) => { releaseFirst = resolve; }))
      .mockResolvedValueOnce([sessionFixture({ summary: "newest" })]);
    const first = useKbStore.getState().refresh();
    await useKbStore.getState().refresh();
    releaseFirst([sessionFixture({ summary: "stale" })]);
    await first;
    expect(useKbStore.getState().sessions[0]?.summary).toBe("newest");
  });

  it("changing the filter re-queries with it", async () => {
    await useKbStore.getState().setFilter("deleted");
    expect(mocks.listSessions).toHaveBeenLastCalledWith({
      include_subagents: false,
      only_deleted: true,
    });
  });

  it("empty sessions are hidden by default and requested explicitly", async () => {
    await useKbStore.getState().refresh();
    expect(mocks.listSessions).toHaveBeenLastCalledWith({ include_subagents: false });
    await useKbStore.getState().setIncludeEmpty(true);
    expect(mocks.listSessions).toHaveBeenLastCalledWith({
      include_subagents: false,
      include_empty: true,
    });
  });

  it("syncNow runs once at a time, refreshes, and reports failures as a result", async () => {
    mocks.syncNow.mockResolvedValue({ status: "ok", changed_files: 1, errors: [] });
    const first = useKbStore.getState().syncNow();
    expect(useKbStore.getState().isSyncing).toBe(true);
    expect(await useKbStore.getState().syncNow()).toEqual({ error: "busy" });
    expect((await first).summary?.status).toBe("ok");
    expect(mocks.syncNow).toHaveBeenCalledTimes(1);
    expect(useKbStore.getState().isSyncing).toBe(false);

    mocks.syncNow.mockRejectedValue("boom");
    expect(await useKbStore.getState().syncNow()).toEqual({ error: "boom" });
    expect(useKbStore.getState().isSyncing).toBe(false);
  });

  it("toggleSchedule uninstalls when installed and installs otherwise", async () => {
    await useKbStore.getState().refresh();
    mocks.uninstallSchedule.mockResolvedValue({});
    expect(await useKbStore.getState().toggleSchedule()).toEqual({});
    expect(mocks.uninstallSchedule).toHaveBeenCalled();

    mocks.getStatus.mockResolvedValue(
      overviewFixture({ schedule: { ...overviewFixture().schedule, installed: false, loaded: false } }),
    );
    await useKbStore.getState().refresh();
    mocks.installSchedule.mockRejectedValue(new Error("launchctl failed"));
    expect(await useKbStore.getState().toggleSchedule()).toEqual({ error: "launchctl failed" });
    expect(useKbStore.getState().isSchedulePending).toBe(false);
  });
});
