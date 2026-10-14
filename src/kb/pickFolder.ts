import { isTauri } from "@/utils/platform";

/** Native folder chooser; `null` when cancelled or unavailable (web mode). */
export const pickFolder = async (title: string): Promise<string | null> => {
  if (!isTauri()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title });
  return typeof picked === "string" ? picked : null;
};
