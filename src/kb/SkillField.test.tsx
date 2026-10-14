import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { overviewFixture, sessionFixture } from "./fixtures";
import { KbHome } from "./KbHome";
import { SkillField } from "./SkillField";
import { configFixture } from "./testkit";
import type { KbSkillStatus } from "./types";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("sonner", () => ({ toast: mocks.toast }));
vi.mock("./openSession", () => ({ openKbSession: vi.fn() }));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const status = (patch: Partial<KbSkillStatus>): KbSkillStatus => ({
  target: "claude",
  tool_found: true,
  dir: "/Users/me/.claude/skills/agentkb",
  state: "not_installed",
  installed_version: null,
  installed_exe: null,
  installed_exe_exists: null,
  stale_reason: null,
  ...patch,
});

const both = (claude: Partial<KbSkillStatus>, codex: Partial<KbSkillStatus> = {}) => [
  status(claude),
  status({ target: "codex", dir: "/Users/me/.codex/skills/agentkb", ...codex }),
];

describe("agent skill settings", () => {
  beforeEach(() => {
    useKbStore.setState(useKbStore.getInitialState(), true);
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    window.localStorage.clear();
  });

  it("shows each tool's state and offers the matching action", async () => {
    mocks.api.skillStatus.mockResolvedValue(
      both({ state: "current" }, { state: "stale", stale_reason: "missing_exe" }),
    );
    render(<SkillField />);
    expect(await screen.findByText("kb.skill.state.current")).toBeTruthy();
    expect(screen.getByText("kb.skill.state.stale")).toBeTruthy();
    expect(screen.getByText("kb.skill.stale.missing_exe")).toBeTruthy();
    // Current: install is disabled, removal offered. Stale: update + removal.
    expect((screen.getAllByText("kb.skill.install")[0] as HTMLButtonElement).closest("button")?.disabled).toBe(true);
    expect(screen.getByText("kb.skill.update")).toBeTruthy();
    expect(screen.getAllByText("kb.skill.uninstall")).toHaveLength(2);
    expect(screen.getByText("kb.skill.permissionNote")).toBeTruthy();
  });

  it("installs, then reflects the new state", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({}, { tool_found: false }));
    mocks.api.skillInstall.mockResolvedValue(both({ state: "current" }, { tool_found: false }));
    render(<SkillField />);
    await screen.findByText("kb.skill.install");
    expect(screen.getByText("kb.skill.state.toolMissing")).toBeTruthy();
    fireEvent.click(screen.getByText("kb.skill.install"));
    await waitFor(() => expect(mocks.api.skillInstall).toHaveBeenCalledWith("claude"));
    expect(await screen.findByText("kb.skill.state.current")).toBeTruthy();
    expect(mocks.toast.success).toHaveBeenCalledWith("kb.skill.installed");
  });

  it("reports a failed install and leaves the state alone", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({}));
    mocks.api.skillInstall.mockRejectedValue(new Error("run from /Applications"));
    render(<SkillField />);
    fireEvent.click((await screen.findAllByText("kb.skill.install"))[0] as HTMLElement);
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("run from /Applications");
    expect(screen.getAllByText("kb.skill.state.notInstalled")).toHaveLength(2);
  });

  it("never offers to touch a skill it did not write", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({ state: "foreign" }));
    render(<SkillField />);
    expect(await screen.findByText("kb.skill.state.foreign")).toBeTruthy();
    // Only the other tool has an install button.
    expect(screen.getAllByText("kb.skill.install")).toHaveLength(1);
    expect(screen.queryByText("kb.skill.uninstall")).toBeNull();
  });

  it("removes a skill", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({ state: "current" }, { tool_found: false }));
    mocks.api.skillUninstall.mockResolvedValue(both({}, { tool_found: false }));
    render(<SkillField />);
    fireEvent.click(await screen.findByText("kb.skill.uninstall"));
    await waitFor(() => expect(mocks.api.skillUninstall).toHaveBeenCalledWith("claude"));
    expect(await screen.findByText("kb.skill.state.notInstalled")).toBeTruthy();
  });

  it("the test button runs a real search and says how many sessions and how fast", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({ state: "current" }));
    mocks.api.skillTest.mockResolvedValue({
      ok: true,
      query: "ghostty",
      matched_sessions: 4,
      millis: 96,
      message: null,
    });
    render(<SkillField />);
    await screen.findByText("kb.skill.state.current");
    fireEvent.click(screen.getByText("kb.skill.test"));
    expect(await screen.findByText(/kb\.test\.skill\.ok/)).toBeTruthy();
    expect(screen.getByText(/kb\.test\.skill\.ok/).textContent).toContain('"count":4');
    expect(screen.getByText(/kb\.test\.skill\.ok/).textContent).toContain('"millis":96');

    mocks.api.skillTest.mockResolvedValue({
      ok: false,
      query: "x",
      matched_sessions: null,
      millis: 5,
      message: "cannot run /gone",
    });
    fireEvent.click(screen.getByText("kb.skill.test"));
    expect((await screen.findByRole("alert")).textContent).toContain("cannot run /gone");
  });

  it("the test is not offered before anything is installed", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({}));
    render(<SkillField />);
    await screen.findAllByText("kb.skill.install");
    expect((screen.getByText("kb.skill.test").closest("button") as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("home page skill prompts", () => {
  beforeEach(() => {
    useKbStore.setState(useKbStore.getInitialState(), true);
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    window.localStorage.clear();
    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    mocks.api.listSessions.mockResolvedValue([sessionFixture()]);
    mocks.api.getFacets.mockResolvedValue({ clients: [], projects: [] });
    mocks.api.getBackupPresence.mockResolvedValue({ state: "disabled" });
    mocks.api.getConfigState.mockResolvedValue({
      state: "valid",
      config: configFixture(),
      config_path: "/x/config.json",
    });
  });

  it("offers the skill once to someone who has none, and remembers a refusal", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({}));
    const { unmount } = render(<KbHome />);
    expect(await screen.findByText("kb.skillPrompt.offer")).toBeTruthy();
    fireEvent.click(screen.getByText("kb.skillPrompt.dismiss"));
    expect(screen.queryByText("kb.skillPrompt.offer")).toBeNull();
    unmount();
    useKbStore.setState(useKbStore.getInitialState(), true);
    render(<KbHome />);
    await screen.findByText("Fix the login bug");
    expect(screen.queryByText("kb.skillPrompt.offer")).toBeNull();
  });

  it("the offer opens settings", async () => {
    mocks.api.skillStatus.mockResolvedValue(both({}));
    render(<KbHome />);
    fireEvent.click(await screen.findByText("kb.skillPrompt.open"));
    expect(useKbStore.getState().settingsOpen).toBe(true);
  });

  it("does not nag people who have no agent installed, or already use the skill", async () => {
    mocks.api.skillStatus.mockResolvedValue(
      both({ tool_found: false }, { tool_found: false }),
    );
    const { unmount } = render(<KbHome />);
    await screen.findByText("Fix the login bug");
    expect(screen.queryByText("kb.skillPrompt.offer")).toBeNull();
    unmount();
    useKbStore.setState(useKbStore.getInitialState(), true);
    mocks.api.skillStatus.mockResolvedValue(both({ state: "current" }));
    render(<KbHome />);
    await screen.findByText("Fix the login bug");
    expect(screen.queryByText("kb.skillPrompt.offer")).toBeNull();
  });

  it("an out-of-date skill is flagged and only rewritten when the user clicks update", async () => {
    mocks.api.skillStatus.mockResolvedValue(
      both({ state: "stale", stale_reason: "path" }, { state: "current" }),
    );
    mocks.api.skillInstall.mockResolvedValue(both({ state: "current" }, { state: "current" }));
    render(<KbHome />);
    expect(await screen.findByText("kb.skillPrompt.stale")).toBeTruthy();
    expect(mocks.api.skillInstall).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(screen.getByText("kb.skillPrompt.update"));
    });
    await waitFor(() => expect(mocks.api.skillInstall).toHaveBeenCalledTimes(1));
    expect(mocks.api.skillInstall).toHaveBeenCalledWith("claude");
    await waitFor(() => expect(screen.queryByText("kb.skillPrompt.stale")).toBeNull());
  });
});
