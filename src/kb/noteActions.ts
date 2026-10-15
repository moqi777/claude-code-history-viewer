import { toast } from "sonner";
import { openKbSession } from "./openSession";
import { focusOfNote, sessionFromNote } from "./searchHit";
import type { KbNote } from "./types";

/** Open the note's session at the first message it is based on. */
export const openNote = async (
  note: KbNote,
  t: (key: string, options?: Record<string, unknown>) => string,
) => {
  try {
    const found = await openKbSession(sessionFromNote(note), focusOfNote(note));
    if (!found) toast.info(t("kb.search.focusFailed"));
  } catch (e) {
    toast.error(t("kb.toast.openFailed", { message: e instanceof Error ? e.message : String(e) }));
  }
};
