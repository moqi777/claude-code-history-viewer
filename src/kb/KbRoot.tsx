import { useEffect } from "react";
import { useAppStore } from "@/store/useAppStore";
import { isTauri } from "@/utils/platform";
import { KbSettings } from "./KbSettings";
import { KbWizard } from "./KbWizard";
import type { KbProgress } from "./types";
import { useKbStore } from "./useKbStore";

/**
 * Mounted once near the app root. Hosts the wizard and settings dialogs, feeds
 * long-operation progress into the store, and on start sends a user who has not
 * finished setup to the knowledge-base page with the wizard open.
 */
export const KbRoot = () => {
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<KbProgress>("kb-progress", (event) =>
          useKbStore.getState().setProgress(event.payload),
        ),
      )
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {
        /* progress is a nicety; the operation itself does not depend on it */
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void (async () => {
      const info = await useKbStore.getState().loadConfigState();
      if (info == null) return;
      if (info.state !== "valid") {
        useAppStore.getState().setAnalyticsCurrentView("kb");
      }
      if (info.state === "missing") useKbStore.getState().openWizard("first");
    })();
  }, []);

  return (
    <>
      <KbWizard />
      <KbSettings />
    </>
  );
};
