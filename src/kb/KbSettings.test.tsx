import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { overviewFixture } from "./fixtures";
import { KbSettings } from "./KbSettings";
import { configFixture, volumeFixture } from "./testkit";
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
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const probes = [
  { source: "claude", root: "/c", exists: true, sessions: 40, bytes: 1e6 },
  { source: "codex", root: "/x", exists: false, sessions: 0, bytes: 0 },
];

const openSettings = async (config = configFixture()) => {
  mocks.api.getConfig.mockResolvedValue(config);
  render(<KbSettings />);
  act(() => useKbStore.getState().openSettings());
  await screen.findByText("kb.settings.title");
  await screen.findByLabelText("kb.field.exclude");
};
const save = () => screen.getByRole("button", { name: "kb.settings.save" }) as HTMLButtonElement;

describe("KbSettings", () => {
  beforeEach(() => {
    useKbStore.setState({ ...useKbStore.getInitialState(), overview: overviewFixture() });
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.api.probeSources.mockResolvedValue(probes);
    mocks.api.getConfigState.mockResolvedValue({ state: "valid", config: configFixture(), config_path: "/x" });
    mocks.api.getStatus.mockResolvedValue(overviewFixture());
    mocks.api.listSessions.mockResolvedValue([]);
    mocks.api.getBackupPresence.mockResolvedValue({ state: "disabled" });
    mocks.api.listExternalVolumes.mockResolvedValue([volumeFixture]);
  });

  it("test buttons give a clear sentence for every setting", async () => {
    mocks.api.testScheduleEnv.mockResolvedValue({
      supported: true, launchctl_available: true, launch_agents_writable: true,
      exe: "/Applications/AgentKB.app/Contents/MacOS/AgentKB", stability: "stable", ok: true,
    });
    await openSettings();
    expect((await screen.findAllByText(/kb\.test\.sources\.found/))[0]?.textContent).toContain('"sessions":40');
    expect(screen.getByText(/kb\.test\.sources\.missing/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "kb.field.testSchedule" }));
    expect(await screen.findByText(/kb\.test\.schedule\.ok/)).toBeTruthy();

    fireEvent.change(screen.getByLabelText("kb.move.newDir"), { target: { value: "/Volumes/Big/KB" } });
    mocks.api.changeDataDirCheck.mockResolvedValue({
      target_state: "empty", needed_bytes: 3.1e9, free_bytes: 120e9, problem: null,
    });
    const testButtons = screen.getAllByRole("button", { name: "kb.field.test" });
    fireEvent.click(testButtons[testButtons.length - 1] as HTMLElement);
    expect(await screen.findByText(/kb\.move\.ready/)).toBeTruthy();
    expect(mocks.api.changeDataDirCheck).toHaveBeenCalledWith("/Volumes/Big/KB");
  });

  it("save is off until something changes, and saving reports the new interval", async () => {
    await openSettings();
    expect(save().disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "kb.field.minutes {\"count\":5}" }));
    expect(save().disabled).toBe(false);
    mocks.api.saveConfig.mockImplementation(async (config) => ({ config, schedule: {}, schedule_error: null }));
    fireEvent.click(save());
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalledWith("kb.settings.saved"));
    expect(mocks.api.saveConfig.mock.calls[0]?.[0].interval_secs).toBe(300);
    await waitFor(() => expect(save().disabled).toBe(true));
  });

  it.each([5, 10, 15])("an interval of %i minutes selects its own preset button, not 'custom'", async (minutes) => {
    await openSettings({ ...configFixture(), interval_secs: minutes * 60 });
    const preset = screen.getByRole("button", { name: `kb.field.minutes {"count":${minutes}}` });
    expect(preset.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "kb.field.custom" }).getAttribute("aria-pressed")).toBe("false");
  });

  it("an interval that is not a preset selects 'custom'", async () => {
    await openSettings({ ...configFixture(), interval_secs: 7 * 60 });
    expect(screen.getByRole("button", { name: "kb.field.custom" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("a custom interval below one minute cannot be saved", async () => {
    await openSettings();
    fireEvent.click(screen.getByRole("button", { name: "kb.field.custom" }));
    fireEvent.change(screen.getByLabelText("kb.field.customMinutes"), { target: { value: "0" } });
    expect((await screen.findByRole("alert")).textContent).toBe("kb.field.intervalInvalid");
    expect(save().disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("kb.field.customMinutes"), { target: { value: "1" } });
    expect(save().disabled).toBe(false);
    mocks.api.saveConfig.mockImplementation(async (config) => ({ config, schedule: {}, schedule_error: null }));
    fireEvent.click(save());
    await waitFor(() => expect(mocks.api.saveConfig).toHaveBeenCalled());
    expect(mocks.api.saveConfig.mock.calls[0]?.[0].interval_secs).toBe(60);
  });

  it("excluded paths must be absolute", async () => {
    await openSettings();
    const box = screen.getByLabelText("kb.field.exclude");
    fireEvent.change(box, { target: { value: "relative/path" } });
    expect(screen.getByText("kb.field.excludeInvalid")).toBeTruthy();
    expect(save().disabled).toBe(true);
    fireEvent.change(box, { target: { value: "/tmp/a\n/tmp/b\n\n" } });
    expect(save().disabled).toBe(false);
    mocks.api.saveConfig.mockImplementation(async (config) => ({ config, schedule: null, schedule_error: null }));
    fireEvent.click(save());
    await waitFor(() => expect(mocks.api.saveConfig).toHaveBeenCalled());
    expect(mocks.api.saveConfig.mock.calls[0]?.[0].exclude_prefixes).toEqual(["/tmp/a", "/tmp/b"]);
  });

  it("excluded working directories are their own list, also absolute-only", async () => {
    await openSettings();
    const box = screen.getByLabelText("kb.field.excludeCwd");
    fireEvent.change(box, { target: { value: "scratch" } });
    expect(save().disabled).toBe(true);
    fireEvent.change(box, { target: { value: "/Users/me/scratch\n" } });
    expect(save().disabled).toBe(false);
    mocks.api.saveConfig.mockImplementation(async (config) => ({ config, schedule: null, schedule_error: null }));
    fireEvent.click(save());
    await waitFor(() => expect(mocks.api.saveConfig).toHaveBeenCalled());
    const sent = mocks.api.saveConfig.mock.calls[0]?.[0];
    expect(sent.exclude_cwd_prefixes).toEqual(["/Users/me/scratch"]);
    expect(sent.exclude_prefixes).toEqual([]);
  });

  it("a schedule failure after a save is reported, not hidden", async () => {
    await openSettings();
    fireEvent.click(screen.getByRole("button", { name: "kb.field.minutes {\"count\":15}" }));
    mocks.api.saveConfig.mockImplementation(async (config) => ({
      config, schedule: null, schedule_error: "launchctl failed",
    }));
    fireEvent.click(save());
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("launchctl failed");
  });

  it("a save the backend rejects is shown to the user", async () => {
    await openSettings();
    fireEvent.click(screen.getByRole("button", { name: "kb.field.minutes {\"count\":15}" }));
    mocks.api.saveConfig.mockRejectedValue(new Error("the backup disk test did not pass"));
    fireEvent.click(save());
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("did not pass");
  });

  it("a new backup disk must pass its test before it can be saved", async () => {
    await openSettings();
    fireEvent.click(screen.getByRole("checkbox", { name: "kb.field.mirrorEnabled" }));
    fireEvent.click(await screen.findByRole("button", { name: /Backup1/ }));
    expect(save().disabled).toBe(true);
    mocks.api.testMirror.mockResolvedValue({
      state: "connected", volume: volumeFixture, free_bytes: 5e9, existing_bytes: 0,
    });
    const tests = screen.getAllByRole("button", { name: "kb.field.test" });
    fireEvent.click(tests[0] as HTMLElement);
    expect(await screen.findByText(/kb\.test\.mirror\.connected/)).toBeTruthy();
    await waitFor(() => expect(save().disabled).toBe(false));
  });

  it("moving the data folder: refuses bad targets, moves good ones, keeps the user informed", async () => {
    await openSettings();
    const input = screen.getByLabelText("kb.move.newDir");
    const testMove = () => {
      const tests = screen.getAllByRole("button", { name: "kb.field.test" });
      fireEvent.click(tests[tests.length - 1] as HTMLElement);
    };
    fireEvent.change(input, { target: { value: "/Users/me/AgentKB/sub" } });
    mocks.api.changeDataDirCheck.mockResolvedValue({
      target_state: "empty", needed_bytes: 1, free_bytes: 1, problem: "nested",
    });
    testMove();
    expect((await screen.findByRole("alert")).textContent).toBe("kb.move.problem.nested");
    expect(screen.queryByRole("button", { name: "kb.move.start" })).toBeNull();

    fireEvent.change(input, { target: { value: "/Volumes/Big/KB" } });
    mocks.api.changeDataDirCheck.mockResolvedValue({
      target_state: "empty", needed_bytes: 3e9, free_bytes: 100e9, problem: null,
    });
    testMove();
    mocks.api.changeDataDirRun.mockResolvedValue({
      report: { files: 10, bytes: 1, verified_files: 10, duration_ms: 5 }, schedule_error: null,
    });
    fireEvent.click(await screen.findByRole("button", { name: "kb.move.start" }));
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalled());
    expect(mocks.api.changeDataDirRun).toHaveBeenCalledWith("/Volumes/Big/KB", false);
    expect(mocks.toast.success.mock.calls[0]?.[0]).toContain("kb.move.done");
  });

  it("a failed move says nothing was switched", async () => {
    await openSettings();
    fireEvent.change(screen.getByLabelText("kb.move.newDir"), { target: { value: "/Volumes/Big/KB" } });
    mocks.api.changeDataDirCheck.mockResolvedValue({
      target_state: "empty", needed_bytes: 1, free_bytes: 10, problem: null,
    });
    const tests = screen.getAllByRole("button", { name: "kb.field.test" });
    fireEvent.click(tests[tests.length - 1] as HTMLElement);
    mocks.api.changeDataDirRun.mockRejectedValue(new Error("verification failed"));
    fireEvent.click(await screen.findByRole("button", { name: "kb.move.start" }));
    await waitFor(() => expect(mocks.toast.error).toHaveBeenCalled());
    expect(mocks.toast.error.mock.calls[0]?.[0]).toContain("kb.move.failed");
  });

  it("an existing AgentKB folder is offered as 'use this folder', not copied over", async () => {
    await openSettings();
    fireEvent.change(screen.getByLabelText("kb.move.newDir"), { target: { value: "/Volumes/Big/KB" } });
    mocks.api.changeDataDirCheck.mockResolvedValue({
      target_state: "existing_data", needed_bytes: 1, free_bytes: 10, problem: null,
    });
    const tests = screen.getAllByRole("button", { name: "kb.field.test" });
    fireEvent.click(tests[tests.length - 1] as HTMLElement);
    mocks.api.changeDataDirRun.mockResolvedValue({ report: null, schedule_error: null });
    fireEvent.click(await screen.findByRole("button", { name: "kb.move.useExisting" }));
    await waitFor(() => expect(mocks.api.changeDataDirRun).toHaveBeenCalledWith("/Volumes/Big/KB", true));
  });

  it("can start the wizard again", async () => {
    await openSettings();
    fireEvent.click(screen.getByRole("button", { name: "kb.settings.runWizard" }));
    expect(useKbStore.getState().wizard).toEqual({ open: true, mode: "rerun" });
    expect(useKbStore.getState().settingsOpen).toBe(false);
  });
});
