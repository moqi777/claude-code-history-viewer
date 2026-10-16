import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ImportDialog, TransferSection } from "./KbTransfer";
import { splitTransferError, transferErrorText } from "./transferErrors";
import { configFixture } from "./testkit";
import type { KbImportInspect, KbImportPrepared, KbImportReport } from "./types";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  dialog: { save: vi.fn(), open: vi.fn() },
  toast: { success: vi.fn(), error: vi.fn() },
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("@tauri-apps/plugin-dialog", () => mocks.dialog);
vi.mock("@/utils/platform", () => ({ isTauri: () => true }));
vi.mock("sonner", () => ({ toast: mocks.toast }));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const info = (patch: Partial<KbImportInspect> = {}): KbImportInspect => ({
  export_id: "id1",
  exported_at: "2026-10-16T08:40:12Z",
  app_version: "0.5.0",
  schema_version: 5,
  device_name: "MacBook-A",
  device_os: "macos",
  counts: { sessions: 313, notes: 116, sessions_with_notes: 19, raw_files: 527, note_files: 19 },
  uncompressed_bytes: 3.9e9,
  file_bytes: 2.1e9,
  local: { empty: true, sessions: 0, notes: 0, raw_files: 0, note_files: 0 },
  needed_bytes: 4.3e9,
  free_bytes: 80e9,
  problem: null,
  ...patch,
});

const merge = {
  sessions_added: 23, files_added: 25, identical: 290, extended: 1, kept_both_live: 1,
  kept_both_diverged: 1, kept_both_other: 0, row_collisions: 0, notes_added: 3,
  notes_identical: 10, notes_conflicts: 5,
};

const prepared = (patch: Partial<KbImportPrepared> = {}): KbImportPrepared => ({
  export_id: "id1",
  mode: "empty",
  device_name: "MacBook-A",
  counts: info().counts,
  summary: null,
  added_exclusions: [],
  local: info().local,
  ...patch,
});

const report = (patch: Partial<KbImportReport> = {}): KbImportReport => ({
  mode: "empty",
  export_id: "id1",
  device_name: "MacBook-A",
  exported_at: "2026-10-16T08:40:12Z",
  summary: null,
  checks: [
    { name: "sessions", expected: 313, actual: 313 },
    { name: "notes", expected: 116, actual: 116 },
  ],
  all_match: true,
  backup_db: null,
  backup_config: null,
  added_exclusions: [],
  config: configFixture(),
  config_written: true,
  warnings: [],
  duration_ms: 9000,
  ...patch,
});

describe("transfer errors", () => {
  it("split the code from the message and pick the wording by the code", () => {
    expect(splitTransferError(new Error("corrupt: bad byte"))).toEqual({ code: "corrupt", message: "bad byte" });
    expect(splitTransferError("plain text")).toEqual({ code: "", message: "plain text" });
    const t = ((key: string, o?: object) => `${key} ${JSON.stringify(o ?? {})}`) as never;
    expect(transferErrorText("digest_running: a digest is running", t)).toContain("kb.transfer.error.digest_running");
    expect(transferErrorText("weird: thing", t)).toContain("kb.transfer.error.generic");
    expect(transferErrorText("cancelled: cancelled", t)).toContain("kb.transfer.error.cancelled");
  });
});

describe("TransferSection export", () => {
  beforeEach(() => {
    useKbStore.setState({ ...useKbStore.getInitialState() });
    for (const group of [mocks.api, mocks.dialog, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
  });

  it("warns that the file is not redacted, exports to the chosen place and reports size and time", async () => {
    render(<TransferSection />);
    expect(screen.getByText("kb.transfer.export.warn")).toBeTruthy();
    mocks.dialog.save.mockResolvedValue("/Volumes/Disk/AgentKB.agentkb");
    mocks.api.exportStart.mockResolvedValue({
      path: "/Volumes/Disk/AgentKB.agentkb", export_id: "x", file_bytes: 2.1e9, uncompressed_bytes: 3.9e9,
      files: 548, sessions: 313, notes: 116, sessions_with_notes: 19, raw_files: 527, note_files: 19,
      duration_ms: 8200, paused_collection: false,
    });
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.export.button" }));
    expect(await screen.findByText(/kb\.transfer\.export\.done/)).toBeTruthy();
    const text = screen.getByText(/kb\.transfer\.export\.done/).textContent ?? "";
    expect(text).toContain('"sessions":313');
    expect(text).toContain("2.10 GB");
    expect(text).toContain('"seconds":8');
    expect(mocks.api.exportStart).toHaveBeenCalledWith("/Volumes/Disk/AgentKB.agentkb", true);
    expect(screen.getByText("/Volumes/Disk/AgentKB.agentkb")).toBeTruthy();
  });

  it("does nothing when the save dialog is cancelled", async () => {
    render(<TransferSection />);
    mocks.dialog.save.mockResolvedValue(null);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.export.button" }));
    await waitFor(() => expect(mocks.dialog.save).toHaveBeenCalled());
    expect(mocks.api.exportStart).not.toHaveBeenCalled();
  });

  it("shows progress with a cancel button, and says why a running digest stops it", async () => {
    render(<TransferSection />);
    mocks.dialog.save.mockResolvedValue("/x/a.agentkb");
    let fail: (e: Error) => void = () => undefined;
    mocks.api.exportStart.mockReturnValue(new Promise((_, reject) => { fail = reject; }));
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.export.button" }));
    const cancel = await screen.findByRole("button", { name: "kb.transfer.cancel" });
    act(() =>
      useKbStore.getState().setProgress({
        op: "export", phase: "write", done: 3, total: 10, bytes_done: 1e9, bytes_total: 4e9,
      }),
    );
    expect((await screen.findByRole("status")).textContent).toContain("kb.transfer.progress.export.write");
    fireEvent.click(cancel);
    expect(mocks.api.transferCancel).toHaveBeenCalled();
    await act(async () => fail(new Error("digest_running: a digest is running")));
    expect((await screen.findByRole("alert")).textContent).toContain("kb.transfer.error.digest_running");
  });
});

describe("ImportDialog", () => {
  const onImported = vi.fn();
  const onClose = vi.fn();
  beforeEach(() => {
    useKbStore.setState({ ...useKbStore.getInitialState() });
    onImported.mockReset();
    onClose.mockReset();
    for (const group of [mocks.api, mocks.dialog, mocks.toast]) {
      for (const fn of Object.values(group)) fn.mockReset();
    }
    mocks.api.syncNow.mockResolvedValue({});
    mocks.api.getConfigState.mockResolvedValue({ state: "valid", config: configFixture(), config_path: "/x" });
  });

  const pick = async (inspect = info()) => {
    mocks.dialog.open.mockResolvedValue("/Volumes/Disk/AgentKB.agentkb");
    mocks.api.importInspect.mockResolvedValue(inspect);
    render(<ImportDialog open wizard={false} onClose={onClose} onImported={onImported} />);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.pick" }));
    await screen.findByText(/kb\.transfer\.import\.from/);
  };

  it("looks at the file first, then unpacks, previews and only then imports", async () => {
    await pick();
    expect(screen.getByText(/kb\.transfer\.import\.from/).textContent).toContain("MacBook-A");
    expect(mocks.api.importPrepare).not.toHaveBeenCalled();

    mocks.api.importPrepare.mockResolvedValue(prepared());
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.check" }));
    expect(await screen.findByText(/kb\.transfer\.import\.previewEmpty/)).toBeTruthy();
    expect(mocks.api.importPrepare).toHaveBeenCalledWith("/Volumes/Disk/AgentKB.agentkb", false, undefined);
    expect(mocks.api.importCommit).not.toHaveBeenCalled();

    mocks.api.importCommit.mockResolvedValue(report());
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.confirm" }));
    expect(await screen.findByText(/kb\.transfer\.import\.done/)).toBeTruthy();
    // settings writes config.json; the wizard does not
    expect(mocks.api.importCommit).toHaveBeenCalledWith("/Volumes/Disk/AgentKB.agentkb", false, true, undefined);
    expect(onImported).toHaveBeenCalledTimes(1);
    expect(mocks.api.syncNow).toHaveBeenCalled();
  });

  it("a merge preview names what is added, kept in both versions and in conflict", async () => {
    await pick(info({ local: { empty: false, sessions: 40, notes: 5, raw_files: 40, note_files: 3 } }));
    mocks.api.importPrepare.mockResolvedValue(
      prepared({ mode: "merge", summary: merge, added_exclusions: ["/Users/me/secret"] }),
    );
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.check" }));
    const text = (await screen.findByText(/kb\.transfer\.import\.previewMerge/)).textContent ?? "";
    expect(text).toContain('"added":23');
    expect(text).toContain('"same":290');
    expect(text).toContain('"conflicts":5');
    expect(screen.getByText(/kb\.transfer\.import\.previewBoth/).textContent).toContain('"count":2');
    expect(screen.getByText(/kb\.transfer\.import\.exclusions/).textContent).toContain("/Users/me/secret");
  });

  it("a damaged file is refused with a plain sentence and nothing is committed", async () => {
    await pick();
    mocks.api.importPrepare.mockRejectedValue(new Error("corrupt: the checksum of raw/x is wrong"));
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.check" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("kb.transfer.error.corrupt");
    expect(mocks.api.importCommit).not.toHaveBeenCalled();
    // choose again goes back to the start
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.again" }));
    expect(screen.getByRole("button", { name: "kb.transfer.import.pick" })).toBeTruthy();
  });

  it("a file from a newer version cannot be unpacked", async () => {
    await pick(info({ problem: "too_new" }));
    expect(screen.queryByRole("button", { name: "kb.transfer.import.check" })).toBeNull();
    expect((await screen.findByRole("alert")).textContent).toContain("kb.transfer.error.too_new");
  });

  it("leaving after the preview removes the unpacked copy", async () => {
    await pick();
    mocks.api.importPrepare.mockResolvedValue(prepared());
    mocks.api.importDiscard.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.check" }));
    await screen.findByText(/kb\.transfer\.import\.previewEmpty/);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.cancel" }));
    expect(mocks.api.importDiscard).toHaveBeenCalledWith("/Volumes/Disk/AgentKB.agentkb", undefined);
    expect(onClose).toHaveBeenCalled();
  });

  it("the wizard leaves config.json alone and uses the folder it was given", async () => {
    mocks.dialog.open.mockResolvedValue("/f.agentkb");
    mocks.api.importInspect.mockResolvedValue(info());
    mocks.api.importPrepare.mockResolvedValue(prepared());
    mocks.api.importCommit.mockResolvedValue(report({ config_written: false }));
    render(<ImportDialog open wizard dataDir="/Users/me/AgentKB" onClose={onClose} onImported={onImported} />);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.pick" }));
    await screen.findByText(/kb\.transfer\.import\.from/);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.check" }));
    await screen.findByText(/kb\.transfer\.import\.previewEmpty/);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.confirm" }));
    await screen.findByText(/kb\.transfer\.import\.doneWizard/);
    expect(mocks.api.importCommit).toHaveBeenCalledWith("/f.agentkb", false, false, "/Users/me/AgentKB");
    expect(mocks.api.syncNow).not.toHaveBeenCalled();
  });

  it("reports numbers that differ from the file without calling the import failed", async () => {
    await pick();
    mocks.api.importPrepare.mockResolvedValue(prepared());
    mocks.api.importCommit.mockResolvedValue(
      report({ all_match: false, checks: [{ name: "sessions", expected: 313, actual: 312 }] }),
    );
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.check" }));
    await screen.findByText(/kb\.transfer\.import\.previewEmpty/);
    fireEvent.click(screen.getByRole("button", { name: "kb.transfer.import.confirm" }));
    expect((await screen.findByText(/kb\.transfer\.import\.mismatch/)).textContent).toContain("sessions 312/313");
  });
});
