import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { KbWizard } from "./KbWizard";
import { configFixture, volumeFixture } from "./testkit";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  app: { setAnalyticsCurrentView: vi.fn() },
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("@/store/useAppStore", () => ({ useAppStore: { getState: () => mocks.app } }));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const probes = [
  { source: "claude", root: "/Users/me/.claude", exists: true, sessions: 40, bytes: 320_000_000 },
  { source: "codex", root: "/Users/me/.codex", exists: true, sessions: 228, bytes: 2_500_000_000 },
];
const goodDir = {
  path: "/Users/me/AgentKB", exists: false, will_create: true, writable: true,
  free_bytes: 120e9, existing: null, db_too_new: false, overlaps_source: null, error: null,
};
const connected = {
  state: "connected", volume: volumeFixture, free_bytes: 50e9, existing_bytes: 0,
};

const open = async (mode: "first" | "rerun" = "first", config = configFixture()) => {
  useKbStore.setState({
    ...useKbStore.getInitialState(),
    configState: { state: mode === "first" ? "missing" : "valid", config, config_path: "/x" } as never,
  });
  render(<KbWizard />);
  act(() => useKbStore.getState().openWizard(mode));
  await screen.findByText("kb.wizard.title");
};

const isNextDisabled = () =>
  (screen.getByRole("button", { name: "kb.wizard.next" }) as HTMLButtonElement).disabled;
const dataDirReady = async () => {
  await screen.findByText(/kb\.test\.dataDir\.ok/);
  await waitFor(() => expect(isNextDisabled()).toBe(false));
};
const sourcesReady = () => screen.findAllByText(/kb\.test\.sources\.found/);
const next = () => fireEvent.click(screen.getByRole("button", { name: "kb.wizard.next" }));
const back = () => fireEvent.click(screen.getByRole("button", { name: "kb.wizard.back" }));
const current = () => screen.getByRole("listitem", { current: "step" }).textContent;

describe("KbWizard", () => {
  beforeEach(() => {
    for (const fn of Object.values(mocks.api)) fn.mockReset();
    mocks.app.setAnalyticsCurrentView.mockReset();
    mocks.api.probeSources.mockResolvedValue(probes);
    mocks.api.testDataDir.mockResolvedValue(goodDir);
    mocks.api.testScheduleEnv.mockResolvedValue({
      supported: true, launchctl_available: true, launch_agents_writable: true,
      exe: "/Applications/AgentKB.app/Contents/MacOS/AgentKB", stability: "stable", ok: true,
    });
    mocks.api.listExternalVolumes.mockResolvedValue([volumeFixture]);
    mocks.api.testMirror.mockResolvedValue(connected);
    mocks.api.getConfigState.mockResolvedValue({ state: "valid", config: configFixture(), config_path: "/x" });
  });

  it("walks forward and back through all six steps without losing choices", async () => {
    await open();
    expect(current()).toContain("kb.wizard.step.sources");
    expect((await sourcesReady())[0]?.textContent).toContain('"sessions":40');
    expect((screen.getByRole("button", { name: "kb.wizard.back" }) as HTMLButtonElement).disabled).toBe(true);
    next();
    expect(current()).toContain("kb.wizard.step.dataDir");
    await dataDirReady();
    next();
    expect(current()).toContain("kb.wizard.step.interval");
    fireEvent.click(screen.getByRole("button", { name: "kb.field.minutes {\"count\":15}" }));
    next();
    expect(current()).toContain("kb.wizard.step.mirror");
    next(); // the disk is optional: skipping is allowed
    expect(current()).toContain("kb.wizard.step.digest");
    next(); // so is the digest
    expect(current()).toContain("kb.wizard.step.confirm");
    expect(screen.getByText(/"count":15/)).toBeTruthy();
    back();
    back();
    back();
    expect(current()).toContain("kb.wizard.step.interval");
    expect(
      (screen.getByRole("button", { name: "kb.field.minutes {\"count\":15}" }) as HTMLButtonElement)
        .getAttribute("aria-pressed"),
    ).toBe("true");
  });

  it("needs at least one source", async () => {
    await open();
    await sourcesReady();
    const boxes = screen.getAllByRole("checkbox");
    fireEvent.click(boxes[0] as HTMLElement);
    fireEvent.click(boxes[1] as HTMLElement);
    expect(isNextDisabled()).toBe(true);
    fireEvent.click(boxes[1] as HTMLElement);
    expect(isNextDisabled()).toBe(false);
  });

  it("will not leave the data folder step until the folder tests fine, and says why", async () => {
    mocks.api.testDataDir.mockResolvedValue({ ...goodDir, writable: false, error: "Permission denied" });
    await open();
    await sourcesReady();
    next();
    expect((await screen.findByRole("alert")).textContent).toContain("Permission denied");
    expect(isNextDisabled()).toBe(true);

    mocks.api.testDataDir.mockResolvedValue({ ...goodDir, overlaps_source: "/Users/me/.claude" });
    fireEvent.click(screen.getByRole("button", { name: "kb.field.test" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("kb.test.dataDir.overlapsSource"));
    expect(isNextDisabled()).toBe(true);
  });

  it("a folder with existing data is announced and reused", async () => {
    mocks.api.testDataDir.mockResolvedValue({
      ...goodDir, exists: true, will_create: false,
      existing: { sessions: 268, bytes: 2.9e9, has_db: true, has_raw: true },
    });
    await open();
    await sourcesReady();
    next();
    expect(await screen.findByText(/kb\.test\.dataDir\.existing/)).toBeTruthy();
    await waitFor(() => expect(isNextDisabled()).toBe(false));
    next();
    next();
    next();
    next();
    expect(screen.getByText(/kb\.wizard\.reuseExisting.*"sessions":268/)).toBeTruthy();
  });

  it("the interval step has a result for its test button", async () => {
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    fireEvent.click(screen.getByRole("button", { name: "kb.field.testSchedule" }));
    expect(await screen.findByText(/kb\.test\.schedule\.ok/)).toBeTruthy();
    // A dev build gets a warning, not a silent pass.
    mocks.api.testScheduleEnv.mockResolvedValue({
      supported: true, launchctl_available: true, launch_agents_writable: true,
      exe: "/repo/target/debug/x", stability: "not_in_app", ok: false,
    });
    fireEvent.click(screen.getByRole("button", { name: "kb.field.testSchedule" }));
    expect(await screen.findByText("kb.test.schedule.devBuild")).toBeTruthy();
  });

  it("the disk step: skipping is fine, enabling requires a passing test", async () => {
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    next();
    expect(current()).toContain("kb.wizard.step.mirror");
    expect(isNextDisabled()).toBe(false);

    fireEvent.click(screen.getByRole("checkbox", { name: "kb.field.mirrorEnabled" }));
    expect(isNextDisabled()).toBe(true);
    fireEvent.click(await screen.findByRole("button", { name: /Backup1/ }));
    fireEvent.click(screen.getByRole("button", { name: "kb.field.test" }));
    expect(await screen.findByText(/kb\.test\.mirror\.connected/)).toBeTruthy();
    await waitFor(() => expect(isNextDisabled()).toBe(false));

    mocks.api.testMirror.mockResolvedValue({ state: "not_connected", reason: "gone" });
    fireEvent.click(screen.getByRole("button", { name: "kb.field.test" }));
    expect(await screen.findByText("kb.test.mirror.notConnected")).toBeTruthy();
    await waitFor(() => expect(isNextDisabled()).toBe(true));
  });

  it("an internal disk is refused with a clear message", async () => {
    mocks.api.testMirror.mockResolvedValue({ state: "unsupported", reason: { kind: "internal", name: "Macintosh HD" } });
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    next();
    fireEvent.click(screen.getByRole("checkbox", { name: "kb.field.mirrorEnabled" }));
    fireEvent.click(await screen.findByRole("button", { name: /Backup1/ }));
    fireEvent.click(screen.getByRole("button", { name: "kb.field.test" }));
    expect((await screen.findByRole("alert")).textContent).toBe("kb.test.mirror.internal");
    await waitFor(() => expect(isNextDisabled()).toBe(true));
  });

  it("commits only at the very end, then offers to enter the knowledge base", async () => {
    mocks.api.wizardCommit.mockResolvedValue({
      sync: { status: "ok", changed_files: 268, copied_bytes: 2_900_000_000, errors: [] },
      schedule: {}, schedule_error: null, backup_started: false,
    });
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    next();
    next();
    next();
    expect(mocks.api.wizardCommit).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "kb.wizard.start" }));
    expect(await screen.findByText("kb.wizard.done")).toBeTruthy();
    expect(mocks.api.wizardCommit).toHaveBeenCalledTimes(1);
    expect(mocks.api.wizardCommit.mock.calls[0]?.[0].data_dir).toBe("/Users/me/AgentKB");
    // The finished screen must survive the state reload the commit triggers.
    await waitFor(() => expect(mocks.api.getConfigState).toHaveBeenCalled());
    expect(screen.getByText("kb.wizard.done")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "kb.wizard.enter" }));
    expect(useKbStore.getState().wizard.open).toBe(false);
    expect(mocks.app.setAnalyticsCurrentView).toHaveBeenCalledWith("kb");
  });

  it("a failed first collection is explained, nothing is claimed saved, and it can be retried", async () => {
    mocks.api.wizardCommit.mockRejectedValueOnce(new Error("the first collection failed: disk full"));
    mocks.api.wizardCommit.mockResolvedValueOnce({
      sync: { status: "ok", changed_files: 1, copied_bytes: 1, errors: [] },
      schedule: null, schedule_error: "launchctl exploded", backup_started: true,
    });
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    next();
    next();
    next();
    fireEvent.click(screen.getByRole("button", { name: "kb.wizard.start" }));
    expect((await screen.findByRole("alert")).textContent).toContain("disk full");
    expect(screen.queryByText("kb.wizard.done")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "kb.wizard.retry" }));
    expect(await screen.findByText("kb.wizard.done")).toBeTruthy();
    expect(screen.getByText(/kb\.wizard\.doneScheduleFailed/)).toBeTruthy();
    expect(screen.getByText("kb.wizard.doneBackup")).toBeTruthy();
  });

  it("the digest step: optional, and it only turns on after the test passes, remembering the CLI's path", async () => {
    mocks.api.digestDailyTokens.mockResolvedValue(null);
    mocks.api.wizardCommit.mockResolvedValue({
      sync: { status: "ok", changed_files: 1, copied_bytes: 1, errors: [] },
      schedule: null, schedule_error: null, backup_started: false,
    });
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    next();
    next();
    expect(current()).toContain("kb.wizard.step.digest");
    expect(isNextDisabled()).toBe(false);
    expect(screen.getByText("kb.digest.sharedQuota")).toBeTruthy();
    const enable = screen.getByRole("checkbox", { name: "kb.digest.field.enable" }) as HTMLInputElement;
    expect(enable.disabled).toBe(true);

    mocks.api.digestTest.mockResolvedValueOnce({
      ok: false, kind: "auth", message: "Invalid API key", cli: "/Users/me/.local/bin/claude",
      duration_ms: 900, input_tokens: 0, output_tokens: 0, model: null,
    });
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.field.test" }));
    expect((await screen.findByRole("alert")).textContent).toContain("kb.digest.test.auth");
    expect(enable.disabled).toBe(true);

    mocks.api.digestTest.mockResolvedValueOnce({
      ok: true, kind: null, message: null, cli: "/Users/me/.local/bin/claude",
      duration_ms: 11_000, input_tokens: 3200, output_tokens: 4, model: "claude-sonnet-5-5",
    });
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.field.test" }));
    expect(await screen.findByText(/kb\.digest\.test\.ok/)).toBeTruthy();
    await waitFor(() => expect(enable.disabled).toBe(false));
    fireEvent.click(enable);
    next();
    expect(screen.getByText(/kb\.digest\.summaryOn/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "kb.wizard.start" }));
    await screen.findByText("kb.wizard.done");
    const sent = mocks.api.wizardCommit.mock.calls[0]?.[0].digest;
    expect(sent.enabled).toBe(true);
    expect(sent.cli_path).toBe("/Users/me/.local/bin/claude");
    // The test was run with the settings on screen.
    expect(mocks.api.digestTest.mock.calls[0]?.[0].engine).toBe("claude");
  });

  it("changing the AI invalidates an earlier passing test", async () => {
    mocks.api.digestDailyTokens.mockResolvedValue(null);
    await open();
    await sourcesReady();
    next();
    await dataDirReady();
    next();
    next();
    next();
    mocks.api.digestTest.mockResolvedValue({
      ok: true, kind: null, message: null, cli: "/x/claude",
      duration_ms: 1000, input_tokens: 1, output_tokens: 1, model: "m",
    });
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.field.test" }));
    const enable = screen.getByRole("checkbox", { name: "kb.digest.field.enable" }) as HTMLInputElement;
    await waitFor(() => expect(enable.disabled).toBe(false));
    fireEvent.click(screen.getByRole("radio", { name: "kb.digest.engine.codex" }));
    expect(enable.disabled).toBe(true);
    expect(screen.queryByText(/kb\.digest\.test\.ok/)).toBeNull();
  });

  it("'later' closes it without writing anything, and reopening starts from the beginning", async () => {
    await open();
    await sourcesReady();
    next();
    fireEvent.click(screen.getByRole("button", { name: "kb.wizard.later" }));
    expect(useKbStore.getState().wizard.open).toBe(false);
    expect(mocks.api.wizardCommit).not.toHaveBeenCalled();
    expect(mocks.api.saveConfig).not.toHaveBeenCalled();
    act(() => useKbStore.getState().openWizard("first"));
    expect(current()).toContain("kb.wizard.step.sources");
  });

  it("re-running the wizard keeps the data folder locked", async () => {
    await open("rerun");
    await sourcesReady();
    next();
    expect((screen.getByLabelText("kb.field.dataDir") as HTMLInputElement).readOnly).toBe(true);
    expect(screen.getByText("kb.wizard.dataDirLocked")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "kb.field.choose" })).toBeNull();
  });
});
