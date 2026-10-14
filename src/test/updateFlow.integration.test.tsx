import { describe, it, expect, vi, beforeAll, afterAll, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { SettingDropdown } from "../layouts/Header/SettingDropdown/index";
import { PlatformProvider } from "../contexts/platform";

// Simulate Tauri environment so isTauri() returns true
beforeAll(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).__TAURI_INTERNALS__ = {};
});
afterAll(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  delete (window as any).__TAURI_INTERNALS__;
});

const {
  mockCheck,
  mockRelaunch,
  mockGetVersion,
  mockOpenModal,
} = vi.hoisted(() => ({
  mockCheck: vi.fn(),
  mockRelaunch: vi.fn(),
  mockGetVersion: vi.fn(),
  mockOpenModal: vi.fn(),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: mockCheck,
}));

vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: mockRelaunch,
}));

vi.mock("@tauri-apps/api/app", () => ({
  getVersion: mockGetVersion,
}));

vi.mock("react-i18next", async () => {
  const actual = await vi.importActual<typeof import("react-i18next")>(
    "react-i18next"
  );

  return {
    ...actual,
    useTranslation: () => ({
      t: (key: string) => key,
    }),
  };
});

vi.mock("@/contexts/modal", () => ({
  useModal: () => ({
    openModal: mockOpenModal,
  }),
}));

vi.mock("@/components/ui/dropdown-menu", () => ({
  DropdownMenu: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="dropdown-menu">{children}</div>
  ),
  DropdownMenuTrigger: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  DropdownMenuContent: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="dropdown-content">{children}</div>
  ),
  DropdownMenuItem: ({
    children,
    onClick,
    disabled,
  }: {
    children: React.ReactNode;
    onClick?: () => void;
    disabled?: boolean;
  }) => (
    <button onClick={onClick} disabled={disabled}>
      {children}
    </button>
  ),
  DropdownMenuLabel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DropdownMenuSeparator: () => <hr />,
}));

vi.mock("../layouts/Header/SettingDropdown/ThemeMenuGroup", () => ({
  ThemeMenuGroup: () => <div data-testid="theme-group" />,
}));

vi.mock("../layouts/Header/SettingDropdown/LanguageMenuGroup", () => ({
  LanguageMenuGroup: () => <div data-testid="language-group" />,
}));

vi.mock("../layouts/Header/SettingDropdown/FilterMenuGroup", () => ({
  FilterMenuGroup: () => <div data-testid="filter-group" />,
}));

vi.mock("../layouts/Header/SettingDropdown/ViewMenuGroup", () => ({
  ViewMenuGroup: () => <div data-testid="view-group" />,
}));

vi.mock("../layouts/Header/SettingDropdown/FontMenuGroup", () => ({
  FontMenuGroup: () => <div data-testid="font-group" />,
}));

vi.mock("../layouts/Header/SettingDropdown/AccessibilityMenuGroup", () => ({
  AccessibilityMenuGroup: () => <div data-testid="accessibility-group" />,
}));


// AgentKB ships without any update mechanism: the updater would fetch from the
// original project and accept packages signed by its author's key.
describe("Updates are disabled", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetVersion.mockResolvedValue("1.0.0");
  });

  it("offers no 'check for updates' entry and never calls the updater plugin", async () => {
    render(
      <PlatformProvider>
        <SettingDropdown />
      </PlatformProvider>
    );

    expect(screen.queryByText("common.settings.checkUpdate")).toBeNull();
    expect(screen.queryByText("common.settings.checking")).toBeNull();
    // Let any stray effect run.
    await Promise.resolve();
    expect(mockCheck).not.toHaveBeenCalled();
  });

  it("does not react to the legacy manual-update-check event", async () => {
    render(
      <PlatformProvider>
        <SettingDropdown />
      </PlatformProvider>
    );
    window.dispatchEvent(new Event("manual-update-check"));
    await Promise.resolve();
    expect(mockCheck).not.toHaveBeenCalled();
  });
});
