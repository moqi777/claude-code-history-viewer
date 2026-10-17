import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { noteFixture } from "./fixtures";
import { KbNotes } from "./KbNotes";

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

describe("KbNotes", () => {
  beforeEach(() => {
    for (const group of [mocks.api, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.open.mockReset();
    mocks.api.notesList.mockResolvedValue({ total: 1, notes: [noteFixture()] });
  });

  it("lists the newest notes with the conclusion up front and the rest folded away", async () => {
    render(<KbNotes />);
    expect(await screen.findByText("WindowServer 内存膨胀至 28GB 的根因是 QQ")).toBeTruthy();
    expect(screen.getByText(/kb\.notes\.conclusion/).textContent).toContain("持有者是 QQ");
    expect(screen.queryByText(/IOSurface，QQ 一直不释放/)).toBeNull();
    expect(mocks.api.notesList.mock.calls[0]?.[0]).toMatchObject({ limit: 50, offset: 0 });

    fireEvent.click(screen.getByRole("button", { expanded: false }));
    expect(screen.getByText(/IOSurface，QQ 一直不释放/)).toBeTruthy();
    expect(screen.getByText(/sudo footprint/)).toBeTruthy();
  });

  it("opens the session at the first message the note is based on", async () => {
    mocks.open.mockResolvedValue(true);
    render(<KbNotes />);
    fireEvent.click(await screen.findByRole("button", { name: /kb\.notes\.openAria/ }));
    await waitFor(() => expect(mocks.open).toHaveBeenCalledTimes(1));
    const [session, focus] = mocks.open.mock.calls[0] ?? [];
    expect(session.file_path).toBe("/AgentKB/raw/claude/projects/p/8e6f1373.jsonl");
    expect(session.actual_session_id).toBe("8e6f1373");
    expect(focus).toEqual({ msgKey: "uuid-1", at: "2026-10-15T01:00:00Z", kind: "user" });
  });

  it("finds a Codex note's message by time and role, since it has no key", async () => {
    mocks.open.mockResolvedValue(false);
    mocks.api.notesList.mockResolvedValue({
      total: 1,
      notes: [
        noteFixture({
          evidence: { seq_from: 7, seq_to: 9, msg_key: null, at: "2026-10-15T01:07:00Z", kind: "assistant" },
        }),
      ],
    });
    render(<KbNotes />);
    fireEvent.click(await screen.findByRole("button", { name: /kb\.notes\.openAria/ }));
    await waitFor(() => expect(mocks.open).toHaveBeenCalled());
    expect(mocks.open.mock.calls[0]?.[1]).toEqual({
      msgKey: null, at: "2026-10-15T01:07:00Z", kind: "assistant",
    });
    await waitFor(() => expect(mocks.toast.info).toHaveBeenCalledWith("kb.search.focusFailed"));
  });

  it("searches only the notes, and filters by source", async () => {
    render(<KbNotes />);
    await screen.findByText("WindowServer 内存膨胀至 28GB 的根因是 QQ");
    const input = screen.getByLabelText("kb.notes.searchPlaceholder");
    mocks.api.notesList.mockClear();
    fireEvent.change(input, { target: { value: "QQ" } });
    await new Promise((r) => setTimeout(r, 400));
    expect(mocks.api.notesList).not.toHaveBeenCalled(); // typing alone does not search
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });
    expect(mocks.api.notesList).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Escape" });
    fireEvent.keyDown(input, { key: "Escape", isComposing: true });
    expect((input as HTMLInputElement).value).toBe("QQ");
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() =>
      expect(mocks.api.notesList).toHaveBeenLastCalledWith(expect.objectContaining({ query: "QQ" })),
    );
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "codex" } });
    await waitFor(() =>
      expect(mocks.api.notesList).toHaveBeenLastCalledWith(
        expect.objectContaining({ query: "QQ", source: "codex" }),
      ),
    );
  });

  it("explains an empty page and an empty search differently", async () => {
    mocks.api.notesList.mockResolvedValue({ total: 0, notes: [] });
    render(<KbNotes />);
    expect(await screen.findByText("kb.notes.empty")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("kb.notes.searchPlaceholder"), { target: { value: "zzz" } });
    fireEvent.click(screen.getByRole("button", { name: "kb.search.button" }));
    expect(await screen.findByText("kb.notes.noMatch")).toBeTruthy();
  });

  it("pages through a long list", async () => {
    const note = (n: number) => noteFixture({ entry_id: `n${n}`, title: `Note ${n}` });
    mocks.api.notesList.mockResolvedValueOnce({ total: 60, notes: [note(1), note(2)] });
    mocks.api.notesList.mockResolvedValueOnce({ total: 60, notes: [note(3)] });
    render(<KbNotes />);
    await screen.findByText("Note 2");
    fireEvent.click(screen.getByRole("button", { name: "kb.notes.more" }));
    expect(await screen.findByText("Note 3")).toBeTruthy();
    expect(screen.getByText("Note 1")).toBeTruthy();
    expect(mocks.api.notesList.mock.calls[1]?.[0]).toMatchObject({ offset: 2 });
  });

  it("reports a failure to load", async () => {
    mocks.api.notesList.mockRejectedValue(new Error("db is locked"));
    render(<KbNotes />);
    expect((await screen.findByRole("alert")).textContent).toContain("db is locked");
  });
});
