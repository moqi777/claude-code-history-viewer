import { useAppStore } from "@/store/useAppStore";
import { isTauri } from "@/utils/platform";
import { useKbStore } from "./useKbStore";

/**
 * ⌘K and the header's search button: go to the knowledge base's search (all
 * backed-up sessions of both tools) instead of the base app's live search of
 * `~/.claude`. Returns false where there is no knowledge base (the WebUI),
 * so the caller keeps the base behaviour.
 */
export function openKbSearch(): boolean {
  if (!isTauri()) return false;
  useAppStore.getState().setAnalyticsCurrentView("kb");
  useKbStore.getState().requestSearchFocus();
  return true;
}
