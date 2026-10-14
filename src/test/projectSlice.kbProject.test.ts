import { describe, expect, it, vi } from "vitest";
import { create } from "zustand";
import { api } from "../services/api";
import { createProjectSlice, type ProjectSlice } from "../store/slices/projectSlice";
import type { ClaudeProject, ClaudeSession } from "../types";

vi.mock("../services/api", () => ({ api: vi.fn() }));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

const kbProject: ClaudeProject = {
  name: "proj",
  path: "kb://codex//Users/me/proj",
  actual_path: "/Users/me/proj",
  session_count: 2,
  message_count: 3,
  last_modified: "2026-10-14T00:00:00Z",
  provider: "codex",
};

const session = (id: string): ClaudeSession => ({
  session_id: id,
  actual_session_id: id,
  file_path: `/AgentKB/raw/codex/sessions/${id}.jsonl`,
  project_name: "proj",
  message_count: 1,
  first_message_time: "",
  last_message_time: "",
  last_modified: "2026-10-14T00:00:00Z",
  has_tool_use: false,
  has_errors: false,
  provider: "codex",
});

const makeStore = () => {
  const scanProjects = vi.fn();
  const store = create<ProjectSlice & { scanProjects: typeof scanProjects }>()(
    (set, get, storeApi) =>
      ({
        ...createProjectSlice(set as never, get as never, storeApi as never),
        scanProjects,
        selectedProject: kbProject,
        sessions: [session("a"), session("b")],
        selectedSession: session("a"),
      }) as never,
  );
  return { store, scanProjects };
};

// AgentKB opens a backed-up session inside a stand-in `kb://` project. The base
// store must leave it alone: there is no directory behind it to reload.
describe("AgentKB stand-in project (kb://)", () => {
  it("reloadProjectSessions does not hit the backend or replace the list", async () => {
    vi.mocked(api).mockClear();
    const { store } = makeStore();
    await store.getState().reloadProjectSessions(kbProject);
    expect(api).not.toHaveBeenCalled();
    expect(store.getState().sessions).toHaveLength(2);
    expect(store.getState().selectedSession?.session_id).toBe("a");
  });

  it("the refresh button keeps the open session instead of clearing the selection", async () => {
    vi.mocked(api).mockClear();
    const { store, scanProjects } = makeStore();
    await store.getState().refreshAllConversations();
    expect(scanProjects).not.toHaveBeenCalled();
    expect(store.getState().selectedProject?.path).toBe(kbProject.path);
    expect(store.getState().selectedSession?.session_id).toBe("a");
  });

  it("real projects still reload as before", async () => {
    vi.mocked(api).mockReset();
    vi.mocked(api).mockResolvedValue({ sessions: [], total: 0, has_more: false });
    const { store } = makeStore();
    const real: ClaudeProject = { ...kbProject, path: "/Users/me/.claude/projects/-x", provider: "claude" };
    await store.getState().reloadProjectSessions(real).catch(() => undefined);
    expect(api).toHaveBeenCalledWith(
      "load_provider_sessions_page",
      expect.objectContaining({ projectPath: real.path }),
    );
  });
});
