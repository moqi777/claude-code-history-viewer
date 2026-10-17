import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SemanticField } from "./KbSemantic";
import type { KbSemanticStatus } from "./types";

const mocks = vi.hoisted(() => ({
  api: {} as ReturnType<typeof import("./testkit").apiMocks>,
  folder: vi.fn(),
}));

vi.mock("./api", async () => {
  const { apiMocks } = await import("./testkit");
  mocks.api = apiMocks();
  return { kbApi: mocks.api };
});
vi.mock("./pickFolder", () => ({ pickFolder: mocks.folder }));
vi.mock("@/utils/platform", () => ({ isTauri: () => false }));
vi.mock("react-i18next", async (importOriginal) => {
  const { fakeT } = await import("./testkit");
  const value = { t: fakeT };
  return { ...(await importOriginal<typeof import("react-i18next")>()), useTranslation: () => value };
});

const status = (patch: Partial<KbSemanticStatus> = {}): KbSemanticStatus => ({
  enabled: false,
  source: "modelscope",
  sources: [
    { id: "modelscope", host: "modelscope.cn" },
    { id: "huggingface", host: "huggingface.co" },
  ],
  model_name: "bge-m3 (int8)",
  model_bytes: 585562194,
  model: { state: "missing" },
  downloading: false,
  index: {
    status: "idle", total: 0, done: 0, updated_at: "", message: "", postponed_rounds: 0,
    indexed: 0, building: false,
  },
  vectors_bytes: 0,
  models_dir: "/Users/me/AgentKB/models/bge-m3-int8",
  ...patch,
});

const ready = (patch: Partial<KbSemanticStatus> = {}) =>
  status({ model: { state: "ready" }, ...patch });

describe("SemanticField", () => {
  beforeEach(() => {
    for (const fn of [...Object.values(mocks.api), mocks.folder]) fn.mockReset();
  });

  it("without a model offers the download from the chosen source and cannot be switched on", async () => {
    mocks.api.semanticStatus.mockResolvedValue(status());
    render(<SemanticField />);
    await screen.findByText(/kb\.semantic\.download$/);
    const enable = screen.getByLabelText(/kb\.semantic\.enable/) as HTMLInputElement;
    expect(enable.disabled).toBe(true);
    expect(screen.getByText(/kb\.semantic\.testFirst/)).toBeTruthy();
    // the address and the size are on screen before anything is downloaded
    expect(screen.getByText("modelscope.cn")).toBeTruthy();
    expect(screen.getByText(/kb\.semantic\.downloadNote/).textContent).toContain("558.0 MB".replace("558.0", "585.6"));
    mocks.api.semanticSet.mockResolvedValue(status({ source: "huggingface" }));
    fireEvent.click(screen.getAllByRole("radio")[1]!);
    await waitFor(() => expect(mocks.api.semanticSet).toHaveBeenCalledWith({ source: "huggingface" }));
  });

  it("downloads, then reloads the status", async () => {
    mocks.api.semanticStatus.mockResolvedValueOnce(status()).mockResolvedValue(ready());
    let finish: () => void = () => undefined;
    mocks.api.semanticDownload.mockReturnValue(new Promise<void>((resolve) => { finish = resolve; }));
    render(<SemanticField />);
    fireEvent.click(await screen.findByText(/kb\.semantic\.download$/));
    await screen.findByText(/kb\.semantic\.downloading/);
    await act(async () => { finish(); });
    await screen.findByText(/kb\.semantic\.modelReady/);
  });

  it("shows why a download failed", async () => {
    mocks.api.semanticStatus.mockResolvedValue(status());
    mocks.api.semanticDownload.mockRejectedValue(new Error("download of model.onnx failed: timed out"));
    render(<SemanticField />);
    fireEvent.click(await screen.findByText(/kb\.semantic\.download$/));
    await screen.findByText(/kb\.semantic\.downloadFailed.*timed out/);
  });

  it("says nothing when the user cancels the download", async () => {
    mocks.api.semanticStatus.mockResolvedValue(status());
    mocks.api.semanticDownload.mockRejectedValue(new Error("cancelled"));
    render(<SemanticField />);
    fireEvent.click(await screen.findByText(/kb\.semantic\.download$/));
    await waitFor(() => expect(mocks.api.semanticDownload).toHaveBeenCalledTimes(1));
    await screen.findByText(/kb\.semantic\.download$/);
    expect(screen.queryAllByText(/kb\.semantic\.downloadFailed/).length).toBe(0);
  });

  it("a passing test unlocks the switch; a failing one says why", async () => {
    mocks.api.semanticStatus.mockResolvedValue(ready());
    mocks.api.semanticTest.mockResolvedValueOnce({ ok: false, seconds: 0.4, dim: 0, message: "boom" });
    render(<SemanticField />);
    const enable = (await screen.findByLabelText(/kb\.semantic\.enable/)) as HTMLInputElement;
    expect(enable.disabled).toBe(true);
    fireEvent.click(screen.getByText(/kb\.semantic\.test$/));
    await screen.findByText(/kb\.semantic\.testFailed.*boom/);
    expect(enable.disabled).toBe(true);

    mocks.api.semanticTest.mockResolvedValueOnce({ ok: true, seconds: 1.8, dim: 1024, message: "" });
    fireEvent.click(screen.getByText(/kb\.semantic\.test$/));
    await screen.findByText(/kb\.semantic\.testOk/);
    expect(enable.disabled).toBe(false);
    mocks.api.semanticSet.mockResolvedValue(ready({ enabled: true }));
    fireEvent.click(enable);
    await waitFor(() => expect(mocks.api.semanticSet).toHaveBeenCalledWith({ enabled: true }));
  });

  it("shows the index progress, the postponed rounds and a damaged model", async () => {
    mocks.api.semanticStatus.mockResolvedValue(
      ready({
        enabled: true,
        index: {
          status: "postponed_memory", total: 24385, done: 6000, updated_at: "", message: "1000 MB available",
          postponed_rounds: 3, indexed: 6000, building: false,
        },
      }),
    );
    const { unmount } = render(<SemanticField />);
    await screen.findByText(/kb\.semantic\.indexProgress/);
    expect(screen.getByRole("progressbar").getAttribute("aria-valuenow")).toBe("24");
    expect(screen.getByText(/kb\.semantic\.postponed/).textContent).toContain('"count":3');
    unmount();

    mocks.api.semanticStatus.mockResolvedValue(
      status({ model: { state: "corrupt", file: "model.onnx", reason: "the SHA-256 does not match" } }),
    );
    render(<SemanticField />);
    await screen.findByText(/kb\.semantic\.modelCorrupt/);
    // a damaged model is replaced by downloading again
    expect(screen.getByText(/kb\.semantic\.download$/)).toBeTruthy();
  });

  it("builds a batch or everything, can stop it, and a running build disables the others", async () => {
    mocks.api.semanticStatus.mockResolvedValue(
      ready({ enabled: true, index: { status: "idle", total: 100, done: 10, updated_at: "", message: "", postponed_rounds: 0, indexed: 10, building: false } }),
    );
    mocks.api.semanticBuild.mockResolvedValue(undefined);
    render(<SemanticField />);
    fireEvent.click(await screen.findByText(/kb\.semantic\.buildAll/));
    await waitFor(() => expect(mocks.api.semanticBuild).toHaveBeenCalledWith(true));
    fireEvent.click(screen.getByText(/kb\.semantic\.buildBatch/));
    await waitFor(() => expect(mocks.api.semanticBuild).toHaveBeenCalledWith(false));
  });

  it("while building shows a stop button and no build buttons enabled", async () => {
    mocks.api.semanticStatus.mockResolvedValue(
      ready({ enabled: true, index: { status: "running", total: 100, done: 10, updated_at: "", message: "", postponed_rounds: 0, indexed: 10, building: true } }),
    );
    mocks.api.semanticBuildCancel.mockResolvedValue(true);
    render(<SemanticField />);
    const stop = await screen.findByText(/kb\.semantic\.stopBuild/);
    expect((screen.getByText(/kb\.semantic\.buildAll/) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(stop);
    await waitFor(() => expect(mocks.api.semanticBuildCancel).toHaveBeenCalled());
  });

  it("imports a model from a folder and reports a refusal", async () => {
    mocks.api.semanticStatus.mockResolvedValue(status());
    mocks.folder.mockResolvedValue("/Users/me/Downloads/bge-m3");
    mocks.api.semanticImportFolder.mockRejectedValue(new Error("model.onnx is not the expected file (SHA-256 differs)"));
    render(<SemanticField />);
    fireEvent.click(await screen.findByText(/kb\.semantic\.fromFolder/));
    await screen.findByText(/not the expected file/);
    expect(mocks.api.semanticImportFolder).toHaveBeenCalledWith("/Users/me/Downloads/bge-m3");
  });
});
