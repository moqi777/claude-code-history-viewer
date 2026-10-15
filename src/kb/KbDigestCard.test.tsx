import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { digestStatusFixture } from "./fixtures";
import { KbDigestCard } from "./KbDigestCard";
import { configFixture } from "./testkit";
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

const show = (patch = {}) => {
  useKbStore.setState({
    ...useKbStore.getInitialState(),
    configState: { state: "valid", config: configFixture(), config_path: "/x" } as never,
    digest: digestStatusFixture(patch),
  });
  render(<KbDigestCard />);
};

describe("KbDigestCard", () => {
  beforeEach(() => {
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.api.digestStatus.mockResolvedValue(digestStatusFixture());
  });

  it("shows nothing before the first status arrives", () => {
    useKbStore.setState({
      ...useKbStore.getInitialState(),
      configState: { state: "valid", config: configFixture(), config_path: "/x" } as never,
    });
    const { container } = render(<KbDigestCard />);
    expect(container.textContent).toBe("");
  });

  it("when off, offers to turn it on and nothing else", () => {
    show({ enabled: false });
    expect(screen.getByText("kb.digest.off")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.turnOn" }));
    expect(useKbStore.getState().settingsOpen).toBe(true);
    expect(screen.queryByRole("button", { name: "kb.digest.runAll" })).toBeNull();
  });

  it("shows the last finished round with the tokens it used, what is waiting, and the next time", () => {
    show();
    expect(screen.getByText(/kb\.digest\.lastDone/).textContent).toContain('"count":12');
    expect(screen.getByText(/kb\.digest\.lastDone/).textContent).toContain("kb.digest.lastTokens");
    expect(screen.getByText(/kb\.digest\.pending/).textContent).toContain('"count":5');
    expect(screen.getByText(/kb\.digest\.pendingOlder/)).toBeTruthy();
    expect(screen.getByText(/kb\.digest\.next/)).toBeTruthy();
  });

  it("announces the cost of the first round before it starts", () => {
    show({ last_round: null, first_round_tokens: 1_200_000 });
    expect(screen.getByText("kb.digest.never")).toBeTruthy();
    expect(screen.getByText(/kb\.digest\.firstRound/)).toBeTruthy();
  });

  it("says plainly that a quota stop is not the user's doing", () => {
    show({
      paused: { reason: "quota", until: "2026-10-16T05:00:00Z", message: "You've hit your session limit" },
    });
    const line = screen.getByText(/kb\.digest\.paused\.quota/);
    expect(line.textContent).toContain("until");
  });

  it("explains a login or CLI problem and where to fix it", () => {
    show({ paused: { reason: "auth", until: "2026-10-16T05:00:00Z", message: "Invalid API key" } });
    expect(screen.getByText(/kb\.digest\.paused\.auth/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.settings" }));
    expect(useKbStore.getState().settingsOpen).toBe(true);
  });

  it("follows a running digest and can stop it", async () => {
    mocks.api.digestCancel.mockResolvedValue(true);
    show({
      running: {
        pid: 42, started_at: "2026-10-16T03:00:00Z", round_id: "2026-10-16",
        total: 12, finished: 3, current_title: "Fix the login bug",
      },
    });
    expect(screen.getByRole("status").textContent).toContain("kb.digest.working");
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("3");
    expect((screen.getByRole("button", { name: "kb.digest.runAll" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.stop" }));
    await waitFor(() => expect(mocks.api.digestCancel).toHaveBeenCalledTimes(1));
  });

  it("asks for confirmation with the estimate before digesting everything", async () => {
    mocks.api.digestEstimateAll.mockResolvedValue({
      sessions: 183, calls: 260, input_tokens: 12_400_000, minutes: 190, engine: "claude", model: "",
    });
    mocks.api.digestRun.mockResolvedValue({ state: "started" });
    show();
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.runAll" }));
    expect(await screen.findByText(/kb\.digest\.confirm\.summary/)).toBeTruthy();
    expect(screen.getByText(/kb\.digest\.confirm\.summary/).textContent).toContain('"sessions":183');
    expect(screen.getByText("kb.digest.sharedQuota")).toBeTruthy();
    expect(mocks.api.digestRun).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "kb.digest.confirm.cancel" }));
    expect(mocks.api.digestRun).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "kb.digest.runAll" }));
    fireEvent.click(await screen.findByRole("button", { name: "kb.digest.confirm.start" }));
    await waitFor(() => expect(mocks.api.digestRun).toHaveBeenCalledWith({ kind: "all" }));
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalled());
  });

  it("tells the user a digest is already running instead of starting a second one", async () => {
    mocks.api.digestEstimateAll.mockResolvedValue({
      sessions: 3, calls: 3, input_tokens: 30_000, minutes: 1, engine: "claude", model: "",
    });
    mocks.api.digestRun.mockResolvedValue({ state: "busy" });
    show();
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.runAll" }));
    fireEvent.click(await screen.findByRole("button", { name: "kb.digest.confirm.start" }));
    await waitFor(() => expect(mocks.toast.info).toHaveBeenCalledWith("kb.digest.toast.busy"));
    expect(mocks.toast.success).not.toHaveBeenCalled();
  });

  it("lists failed sessions with their error and retries one", async () => {
    mocks.api.digestRun.mockResolvedValue({ state: "started" });
    show({
      failures: [
        { source: "claude", session_id: "abc", title: "Broken one", attempts: 3, error: "invalid answer: no keywords" },
      ],
    });
    expect(screen.getByRole("alert").textContent).toContain('"count":1');
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.viewFailures" }));
    expect(await screen.findByText("Broken one")).toBeTruthy();
    expect(screen.getByText(/invalid answer: no keywords/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "kb.digest.retry" }));
    await waitFor(() =>
      expect(mocks.api.digestRun).toHaveBeenCalledWith({ kind: "session", source: "claude", session_id: "abc" }),
    );
  });
});
