import { beforeEach, describe, expect, it, vi } from "vitest";
import { sessionFixture } from "./fixtures";
import { isKbProject, openKbSession, syntheticProject } from "./openSession";

const mocks = vi.hoisted(() => ({
  listSessions: vi.fn(),
  app: {
    exitSessionSelectionMode: vi.fn(),
    setViewingGlobalStats: vi.fn(),
    setAnalyticsCurrentView: vi.fn(),
    selectSession: vi.fn(),
  },
  setState: vi.fn(),
}));

vi.mock("./api", () => ({ kbApi: { listSessions: mocks.listSessions } }));
vi.mock("@/store/useAppStore", () => ({
  useAppStore: { getState: () => mocks.app, setState: mocks.setState },
}));

const codex = sessionFixture({
  file_path: "/AgentKB/raw/codex/sessions/r.jsonl",
  session_id: "/AgentKB/raw/codex/sessions/r.jsonl",
  provider: "codex",
  project_name: "claude-code-history-viewer",
  cwd: "/Users/me/Dev/claude-code-history-viewer",
  last_modified: "2026-10-14T09:30:00Z",
  message_count: 10,
});
const sibling = sessionFixture({
  file_path: "/AgentKB/raw/codex/sessions/s.jsonl",
  provider: "codex",
  cwd: codex.cwd,
  project_name: codex.project_name,
  last_modified: "2026-10-15T09:30:00Z",
  message_count: 5,
  source_status: "deleted",
});

describe("opening a backed-up session", () => {
  beforeEach(() => {
    mocks.listSessions.mockReset();
    mocks.setState.mockReset();
    for (const fn of Object.values(mocks.app)) fn.mockReset();
    mocks.app.selectSession.mockResolvedValue(undefined);
  });

  it("builds a stand-in project that carries the source and the working directory", () => {
    const project = syntheticProject(codex, [codex, sibling]);
    expect(project).toMatchObject({
      name: "claude-code-history-viewer",
      path: "kb://codex//Users/me/Dev/claude-code-history-viewer",
      actual_path: "/Users/me/Dev/claude-code-history-viewer",
      provider: "codex",
      session_count: 2,
      message_count: 15,
      last_modified: "2026-10-15T09:30:00Z",
    });
    expect(project.path_status).toBeUndefined();
    expect(isKbProject(project)).toBe(true);
    expect(isKbProject({ path: "/Users/me/.claude/projects/x" })).toBe(false);
    expect(isKbProject(null)).toBe(false);
  });

  it("a session without a working directory gets no resume command", () => {
    const project = syntheticProject(sessionFixture({ cwd: null, project_name: "odd" }), []);
    expect(project.path_status).toBe("unavailable");
    expect(project.actual_path).toBe("");
  });

  it("puts the project's backed-up sessions in the middle column and opens the chosen one", async () => {
    mocks.listSessions.mockResolvedValue([sibling, codex]);
    await openKbSession(codex);
    expect(mocks.listSessions).toHaveBeenCalledWith({
      cwd: codex.cwd,
      source: "codex",
      include_subagents: true,
      include_empty: true,
      limit: 500,
    });
    const patch = mocks.setState.mock.calls[0]?.[0];
    expect(patch.selectedProject.provider).toBe("codex");
    expect(patch.selectedProject.name).toBe("claude-code-history-viewer");
    expect(patch.sessions).toEqual([sibling, codex]);
    expect(patch.hasMoreSessions).toBe(false);
    expect(mocks.app.exitSessionSelectionMode).toHaveBeenCalled();
    expect(mocks.app.setViewingGlobalStats).toHaveBeenCalledWith(false);
    expect(mocks.app.setAnalyticsCurrentView).toHaveBeenCalledWith("messages");
    expect(mocks.app.selectSession).toHaveBeenCalledWith(codex);
  });

  it("still lists the opened session if the sibling query somehow misses it", async () => {
    mocks.listSessions.mockResolvedValue([sibling]);
    await openKbSession(codex);
    expect(mocks.setState.mock.calls[0]?.[0].sessions).toEqual([codex, sibling]);
  });

  it("without a working directory only that session is listed", async () => {
    const lone = sessionFixture({ cwd: null });
    await openKbSession(lone);
    expect(mocks.listSessions).not.toHaveBeenCalled();
    expect(mocks.setState.mock.calls[0]?.[0].sessions).toEqual([lone]);
  });

  it("propagates a failure so the page can tell the user", async () => {
    mocks.listSessions.mockResolvedValue([codex]);
    mocks.app.selectSession.mockRejectedValue(new Error("no such file"));
    await expect(openKbSession(codex)).rejects.toThrow("no such file");
  });
});
