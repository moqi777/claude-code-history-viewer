import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { overviewFixture, sessionFixture } from "./fixtures";
import { KbPanel } from "./KbPanel";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {
    getStatus: vi.fn(),
    syncNow: vi.fn(),
    listSessions: vi.fn(),
    installSchedule: vi.fn(),
    uninstallSchedule: vi.fn(),
  },
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn() },
  app: { setAnalyticsCurrentView: vi.fn(), selectSession: vi.fn() },
}));

vi.mock("./api", () => ({ kbApi: mocks.api }));
vi.mock("sonner", () => ({ toast: mocks.toast }));
vi.mock("react-i18next", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-i18next")>()),
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options == null ? key : `${key} ${JSON.stringify(options)}`,
  }),
}));
vi.mock("@/store/useAppStore", () => ({
  useAppStore: { getState: () => mocks.app },
}));

const deletedSession = sessionFixture({
  file_path: "/AgentKB/raw/codex/sessions/r.jsonl",
  session_id: "/AgentKB/raw/codex/sessions/r.jsonl",
  summary: "Gone from disk",
  provider: "codex",
  source_status: "deleted",
});

const openPanel = async () => {
  render(<KbPanel />);
  act(() => useKbStore.getState().open());
  await screen.findByText("kb.status.dataDir");
};

describe("KbPanel", () => {
  beforeEach(() => {
    useKbStore.setState(useKbStore.getInitialState(), true);
    for (const group of [mocks.api, mocks.toast, mocks.app]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    mocks.api.listSessions.mockResolvedValue([sessionFixture(), deletedSession]);
  });

  it("shows status and marks sessions whose source was deleted", async () => {
    await openPanel();
    expect(screen.getByText("/Users/me/AgentKB")).toBeTruthy();
    expect(screen.getByText(/kb.status.schedule.active/)).toBeTruthy();
    expect(screen.getByText("Fix the login bug")).toBeTruthy();
    const badges = screen.getAllByText("kb.sessions.deletedBadge");
    expect(badges).toHaveLength(1);
    expect(badges[0]?.closest("button")?.textContent).toContain("Gone from disk");
  });

  it("opens a session in the existing viewer and closes the panel", async () => {
    mocks.app.selectSession.mockResolvedValue(undefined);
    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: /Gone from disk/ }));
    await waitFor(() => expect(mocks.app.selectSession).toHaveBeenCalledTimes(1));
    expect(mocks.app.setAnalyticsCurrentView).toHaveBeenCalledWith("messages");
    expect(mocks.app.selectSession.mock.calls[0]?.[0].file_path).toBe(deletedSession.file_path);
    await waitFor(() => expect(useKbStore.getState().isOpen).toBe(false));
  });

  it("tells the user when a session cannot be opened and stays open", async () => {
    mocks.app.selectSession.mockRejectedValue(new Error("no such file"));
    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: /Gone from disk/ }));
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(useKbStore.getState().isOpen).toBe(true);
  });

  it("collect now: shows 'collecting', then reports the result", async () => {
    let finish: (v: unknown) => void = () => {};
    mocks.api.syncNow.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: "kb.action.syncNow" }));
    const busy = await screen.findByRole("button", { name: "kb.action.syncing" });
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    finish({ status: "ok", changed_files: 2, copied_bytes: 2048, errors: [] });
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalled());
    expect(mocks.toast.success.mock.calls[0]?.[0]).toContain("kb.toast.syncDone");
  });

  it("shows 'collecting' when another process holds the lock, and says so on a skipped run", async () => {
    mocks.api.getStatus.mockResolvedValue(overviewFixture({ running: true }));
    await openPanel();
    expect((screen.getByRole("button", { name: "kb.action.syncing" }) as HTMLButtonElement).disabled).toBe(true);

    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    await act(() => useKbStore.getState().refresh());
    mocks.api.syncNow.mockResolvedValue({ status: "skipped", reason: "locked", errors: [] });
    fireEvent.click(screen.getByRole("button", { name: "kb.action.syncNow" }));
    await waitFor(() => expect(mocks.toast.info).toHaveBeenCalledWith("kb.toast.syncBusy"));
  });

  it("a failed collection is reported to the user", async () => {
    mocks.api.syncNow.mockRejectedValue(new Error("disk full"));
    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: "kb.action.syncNow" }));
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("disk full");
  });

  it("filters by source through the backend", async () => {
    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: "kb.sessions.filter.deleted" }));
    await waitFor(() =>
      expect(mocks.api.listSessions).toHaveBeenLastCalledWith({
        include_subagents: false,
        only_deleted: true,
      }),
    );
  });

  it("surfaces load errors", async () => {
    mocks.api.getStatus.mockRejectedValue(new Error("db locked"));
    render(<KbPanel />);
    act(() => useKbStore.getState().open());
    expect((await screen.findByRole("alert")).textContent).toContain("db locked");
  });
});
