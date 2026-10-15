import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { noteFixture } from "./fixtures";
import { KbSessionNotesButton } from "./KbSessionNotes";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
  jump: vi.fn(),
  session: { current: null as unknown },
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("sonner", () => ({ toast: mocks.toast }));
vi.mock("./openSession", () => ({ jumpToFocus: mocks.jump, openKbSession: vi.fn() }));
vi.mock("@/store/useAppStore", () => ({
  useAppStore: (select: (s: unknown) => unknown) => select({ selectedSession: mocks.session.current }),
}));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const session = (patch = {}) => ({
  provider: "claude",
  actual_session_id: "8e6f1373",
  is_subagent: false,
  ...patch,
});

describe("KbSessionNotesButton", () => {
  beforeEach(() => {
    useKbStore.setState(useKbStore.getInitialState(), true);
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.jump.mockReset();
    mocks.session.current = session();
    mocks.api.digestStatus.mockResolvedValue(undefined);
    mocks.api.notesForSession.mockResolvedValue({
      notes: [noteFixture(), noteFixture({ entry_id: "n2", title: "Second", evidence: { seq_from: 50, seq_to: 60, msg_key: "k50", at: null, kind: "user" } })],
      status: "notes",
      digested_at: "2026-10-16T03:12:44Z",
    });
  });

  it("counts the session's notes on the button and lists them in the panel", async () => {
    render(<KbSessionNotesButton />);
    const button = await screen.findByRole("button", { name: /kb\.sessionNotes\.button/ });
    await waitFor(() => expect(button.textContent).toContain('"count":2'));
    expect(mocks.api.notesForSession).toHaveBeenCalledWith("claude", "8e6f1373");
    fireEvent.click(button);
    expect(await screen.findByText("Second")).toBeTruthy();
    expect(screen.getByText("kb.sessionNotes.disclaimer")).toBeTruthy();
  });

  it("jumps to the messages a note is based on, in the session that is already open", async () => {
    mocks.jump.mockResolvedValue(true);
    render(<KbSessionNotesButton />);
    fireEvent.click(await screen.findByRole("button", { name: /kb\.sessionNotes\.button/ }));
    const jumps = await screen.findAllByRole("button", { name: /kb\.sessionNotes\.jump/ });
    fireEvent.click(jumps[1] as HTMLElement);
    await waitFor(() => expect(mocks.jump).toHaveBeenCalledWith({ msgKey: "k50", at: null, kind: "user" }));
  });

  it("digests this session now, or says why it will not", async () => {
    mocks.api.digestRun.mockResolvedValueOnce({ state: "nothing", message: "nothing new in this session since its last note" });
    mocks.api.digestRun.mockResolvedValueOnce({ state: "started" });
    render(<KbSessionNotesButton />);
    fireEvent.click(await screen.findByRole("button", { name: /kb\.sessionNotes\.button/ }));
    const now = await screen.findByRole("button", { name: "kb.sessionNotes.digestNow" });
    fireEvent.click(now);
    await waitFor(() =>
      expect(mocks.toast.info).toHaveBeenCalledWith("nothing new in this session since its last note"),
    );
    fireEvent.click(now);
    await waitFor(() => expect(mocks.toast.success).toHaveBeenCalledWith("kb.digest.toast.sessionStarted"));
    expect(mocks.api.digestRun).toHaveBeenLastCalledWith({ kind: "session", source: "claude", session_id: "8e6f1373" });
  });

  it("is not offered on a sub-agent session (its notes belong to the parent)", () => {
    mocks.session.current = session({ is_subagent: true });
    const { container } = render(<KbSessionNotesButton />);
    expect(container.textContent).toBe("");
  });
});
