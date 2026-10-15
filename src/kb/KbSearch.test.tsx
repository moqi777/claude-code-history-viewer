import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { noteFixture, overviewFixture, sessionFixture } from "./fixtures";
import { KbHome } from "./KbHome";
import { configFixture } from "./testkit";
import type { KbSearchResponse, KbSessionHit } from "./types";
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

const hit = (patch: Partial<KbSessionHit> = {}): KbSessionHit => ({
  rank: 1,
  score: 7.4,
  session: {
    source: "codex",
    session_id: "01a08505",
    title: "排查 Ghostty 卡死",
    client: "vscode",
    project: "ghostty-debug",
    cwd: "/Users/me/ghostty-debug",
    started_at: "2026-09-09T15:15:47Z",
    last_activity_at: "2026-09-09T16:02:10Z",
    source_status: "present",
    is_subagent: false,
    parent_session_id: null,
    message_count: 30,
  },
  resume: { available: true, command: "codex resume 01a08505", note: null },
  paths: { mirror: "/AgentKB/raw/codex/sessions/r.jsonl", original: "/home/.codex/r.jsonl" },
  hit_messages: 3,
  hits: [
    {
      seq: 26,
      kind: "assistant",
      at: "2026-09-09T15:40:12Z",
      msg_key: null,
      snippet: "…不能仅凭 objc_retain 宣布已死锁，应保存全线程 sample…",
      highlights: [[21, 23]],
      show: "AgentKB --kb-show codex:01a08505 --around 26",
    },
  ],
  ...patch,
});

const response = (patch: Partial<KbSearchResponse> = {}): KbSearchResponse => ({
  query: "死锁",
  tokens: ["死锁"],
  mode_used: "all",
  read_mode: "normal",
  data_as_of: "2026-10-15T00:30:22Z",
  matched_sessions: 11,
  returned: 1,
  results: [hit()],
  hint: null,
  ...patch,
});

const type = (text: string) =>
  fireEvent.change(screen.getByLabelText("kb.search.placeholder"), { target: { value: text } });

describe("home page search", () => {
  beforeEach(async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    useKbStore.setState(useKbStore.getInitialState(), true);
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.open.mockReset();
    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    mocks.api.listSessions.mockResolvedValue([sessionFixture()]);
    mocks.api.getFacets.mockResolvedValue({ clients: [], projects: [] });
    mocks.api.getBackupPresence.mockResolvedValue({ state: "disabled" });
    mocks.api.skillStatus.mockResolvedValue([]);
    mocks.api.getConfigState.mockResolvedValue({
      state: "valid",
      config: configFixture(),
      config_path: "/x/config.json",
    });
    render(<KbHome />);
    await screen.findByText("Fix the login bug");
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  const runSearch = async (outcome: unknown, text = "死锁") => {
    mocks.api.search.mockResolvedValue(outcome);
    type(text);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(400);
    });
  };

  it("searches once the typing pauses and replaces the session list with the hits", async () => {
    await runSearch({ state: "ok", response: response() });
    expect(mocks.api.search).toHaveBeenCalledTimes(1);
    expect(mocks.api.search.mock.calls[0]?.[0]).toMatchObject({
      query: "死锁",
      limit: 20,
      per_session: 2,
      include_subagents: false,
    });
    expect(screen.queryByText("Fix the login bug")).toBeNull();
    expect(screen.getByText("排查 Ghostty 卡死")).toBeTruthy();
    expect(screen.getByText(/kb\.search\.found/).textContent).toContain('"count":11');
    expect(screen.getByText("kb.search.kind.assistant")).toBeTruthy();
    expect(screen.getByText("kb.search.moreHits {\"count\":2}")).toBeTruthy();
    // The matched words are marked.
    const marks = document.querySelectorAll("mark");
    expect(marks).toHaveLength(1);
    expect(marks[0]?.textContent).toBe("死锁");
  });

  it("does not search on every keystroke", async () => {
    mocks.api.search.mockResolvedValue({ state: "ok", response: response() });
    type("死");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });
    type("死锁");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(400);
    });
    expect(mocks.api.search).toHaveBeenCalledTimes(1);
    expect(mocks.api.search.mock.calls[0]?.[0].query).toBe("死锁");
  });

  it("the filters narrow the search", async () => {
    await act(async () => {
      await useKbStore.getState().setFilters({ source: "codex", project: "ghostty-debug" });
    });
    await runSearch({ state: "ok", response: response() });
    expect(mocks.api.search.mock.calls[0]?.[0]).toMatchObject({
      source: "codex",
      project: "ghostty-debug",
    });
  });

  it("clicking a hit opens the session at that message", async () => {
    mocks.open.mockResolvedValue(true);
    await runSearch({ state: "ok", response: response() });
    fireEvent.click(document.querySelector("mark") as HTMLElement);
    await waitFor(() => expect(mocks.open).toHaveBeenCalledTimes(1));
    const [session, focus] = mocks.open.mock.calls[0] ?? [];
    expect(session.file_path).toBe("/AgentKB/raw/codex/sessions/r.jsonl");
    expect(session.actual_session_id).toBe("01a08505");
    expect(focus).toEqual({ msgKey: null, at: "2026-09-09T15:40:12Z", kind: "assistant" });
    expect(mocks.toast.info).not.toHaveBeenCalled();
  });

  it("says so when the exact message could not be found", async () => {
    mocks.open.mockResolvedValue(false);
    await runSearch({ state: "ok", response: response() });
    fireEvent.click(document.querySelector("mark") as HTMLElement);
    await waitFor(() => expect(mocks.toast.info).toHaveBeenCalledWith("kb.search.focusFailed"));
  });

  it("explains an empty result, a relaxed one, and an index that is not ready", async () => {
    await runSearch({ state: "ok", response: response({ results: [], matched_sessions: 0, returned: 0 }) });
    expect(screen.getByText("kb.search.none")).toBeTruthy();

    await runSearch({ state: "ok", response: response({ mode_used: "any" }) }, "死锁 redis");
    expect(screen.getByText(/kb\.search\.relaxed/)).toBeTruthy();

    await runSearch({ state: "not_ready", message: "building" }, "限流");
    expect(screen.getByText("kb.search.notReady")).toBeTruthy();
    expect(screen.queryByText("kb.search.none")).toBeNull();

    mocks.api.search.mockRejectedValue(new Error("boom"));
    type("超时");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(400);
    });
    expect((await screen.findByRole("alert")).textContent).toContain("boom");
  });

  it("clearing the box brings the session list back", async () => {
    await runSearch({ state: "ok", response: response() });
    fireEvent.click(screen.getByLabelText("kb.search.clear"));
    expect(screen.getByText("Fix the login bug")).toBeTruthy();
    expect(screen.queryByText("排查 Ghostty 卡死")).toBeNull();
  });

  it("an answer that arrives after the box was cleared is dropped", async () => {
    let resolve: (v: unknown) => void = () => undefined;
    mocks.api.search.mockReturnValue(new Promise((r) => (resolve = r)));
    type("死锁");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(400);
    });
    type("");
    await act(async () => {
      resolve({ state: "ok", response: response() });
    });
    expect(screen.getByText("Fix the login bug")).toBeTruthy();
    expect(useKbStore.getState().search.status).toBe("idle");
  });

  it("shows the AI's notes ahead of the message hits, and flags sessions that have notes", async () => {
    await runSearch({
      state: "ok",
      response: response({ notes: [noteFixture()], results: [hit({ has_note: true })] }),
    });
    const notes = screen.getByRole("region", { name: "kb.search.notesTitle" });
    expect(notes.textContent).toContain("WindowServer 内存膨胀至 28GB 的根因是 QQ");
    expect(screen.getByText("kb.search.hasNote")).toBeTruthy();
    const all = document.body.innerHTML;
    expect(all.indexOf("WindowServer 内存膨胀")).toBeLessThan(all.indexOf("排查 Ghostty 卡死"));
  });

  it("a search that only finds notes is not reported as empty", async () => {
    await runSearch({
      state: "ok",
      response: response({ notes: [noteFixture()], results: [], matched_sessions: 0, returned: 0 }),
    });
    expect(screen.queryByText("kb.search.none")).toBeNull();
    expect(screen.getByText("WindowServer 内存膨胀至 28GB 的根因是 QQ")).toBeTruthy();
  });
});
