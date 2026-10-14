import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

import { Settings, MessageSquare, Folder, Archive, SlidersHorizontal, DatabaseBackup } from "lucide-react";


import { useTranslation } from "react-i18next";
import { useModal } from "@/contexts/modal";
import { DesktopOnly } from "@/contexts/platform";
import { ThemeMenuGroup } from "./ThemeMenuGroup";
import { LanguageMenuGroup } from "./LanguageMenuGroup";
import { FilterMenuGroup } from "./FilterMenuGroup";
import { ViewMenuGroup } from "./ViewMenuGroup";
import { FontMenuGroup } from "./FontMenuGroup";
import { AccessibilityMenuGroup } from "./AccessibilityMenuGroup";

interface SettingDropdownProps {
  /** Claude-only tool; disabled for other providers. */
  onOpenArchive?: () => void;
  archiveDisabled?: boolean;
  onOpenSettingsManager?: () => void;
  onOpenKb?: () => void;
}

export const SettingDropdown = ({
  onOpenArchive,
  archiveDisabled = false,
  onOpenSettingsManager,
  onOpenKb,
}: SettingDropdownProps) => {
  const { t } = useTranslation();
  const { openModal } = useModal();

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            id="app-settings-button"
            className="p-2 rounded-lg transition-colors cursor-pointer relative text-muted-foreground/50 hover:text-foreground/80 hover:bg-muted"
            aria-label={t("common.settings.title")}
          >
            <Settings className="w-5 h-5 text-foreground" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-60">
          {(onOpenSettingsManager || onOpenArchive) && (
            <>
              <DropdownMenuLabel>{t("common.settings.tools", "Claude Code")}</DropdownMenuLabel>
              <DropdownMenuSeparator />
              {onOpenSettingsManager && (
                <DropdownMenuItem onClick={onOpenSettingsManager}>
                  <SlidersHorizontal className="mr-2 h-4 w-4 text-foreground" />
                  <span>{t("settingsManager.title")}</span>
                </DropdownMenuItem>
              )}
              {onOpenArchive && (
                <DropdownMenuItem onClick={onOpenArchive} disabled={archiveDisabled}>
                  <Archive className="mr-2 h-4 w-4 text-foreground" />
                  <span>
                    {archiveDisabled
                      ? t("common.settings.claudeOnly", { name: t("archive.title") })
                      : t("archive.title")}
                  </span>
                </DropdownMenuItem>
              )}
              <DropdownMenuSeparator />
            </>
          )}
          <DesktopOnly>
            <DropdownMenuItem onClick={onOpenKb}>
              <DatabaseBackup className="mr-2 h-4 w-4 text-foreground" />
              <span>{t("kb.menuItem")}</span>
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </DesktopOnly>
          <DropdownMenuLabel>{t('common.settings.title')}</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            onClick={() => openModal("folderSelector", { mode: "change" })}
          >
            <Folder className="mr-2 h-4 w-4 text-foreground" />
            <span>{t('common.settings.changeFolder')}</span>
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => openModal("feedback")}>
            <MessageSquare className="mr-2 h-4 w-4 text-foreground" />
            <span>{t("feedback.title")}</span>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <FilterMenuGroup />

          <DropdownMenuSeparator />
          <ViewMenuGroup />

          <DropdownMenuSeparator />
          <FontMenuGroup />

          <DropdownMenuSeparator />
          <AccessibilityMenuGroup />

          <DropdownMenuSeparator />
          <ThemeMenuGroup />

          <DropdownMenuSeparator />
          <LanguageMenuGroup />

        </DropdownMenuContent>
      </DropdownMenu>
    </>
  );
};
