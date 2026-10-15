import { beforeEach, describe, expect, it, vi } from "vitest";
import { openKbSearch } from "./openSearch";
import { useKbStore } from "./useKbStore";

const mocks = vi.hoisted(() => ({
  tauri: true,
  setAnalyticsCurrentView: vi.fn(),
}));

vi.mock("@/utils/platform", () => ({ isTauri: () => mocks.tauri }));
vi.mock("@/store/useAppStore", () => ({
  useAppStore: { getState: () => ({ setAnalyticsCurrentView: mocks.setAnalyticsCurrentView }) },
}));

describe("opening the knowledge base's search", () => {
  beforeEach(() => {
    mocks.tauri = true;
    mocks.setAnalyticsCurrentView.mockReset();
    useKbStore.setState({ homeTab: "notes", searchFocusPending: false });
  });

  it("shows the knowledge base home on its sessions tab and asks for the search box's focus", () => {
    expect(openKbSearch()).toBe(true);
    expect(mocks.setAnalyticsCurrentView).toHaveBeenCalledWith("kb");
    expect(useKbStore.getState()).toMatchObject({ homeTab: "sessions", searchFocusPending: true });
  });

  it("does nothing where there is no knowledge base (the WebUI)", () => {
    mocks.tauri = false;
    expect(openKbSearch()).toBe(false);
    expect(mocks.setAnalyticsCurrentView).not.toHaveBeenCalled();
    expect(useKbStore.getState().searchFocusPending).toBe(false);
  });
});
