import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2, StickyNote } from "lucide-react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui";
import { useAppStore } from "@/store/useAppStore";
import { formatTime } from "@/utils/time";
import { kbApi } from "./api";
import { NoteCard } from "./KbNotes";
import { jumpToFocus } from "./openSession";
import { focusOfNote } from "./searchHit";
import type { KbNote, KbSessionNotes, KbSource } from "./types";
import { useKbStore } from "./useKbStore";

/**
 * In the viewer's header while a backed-up session is open: "Notes (n)", a
 * panel with that session's notes, and "digest this one now".
 */
export const KbSessionNotesButton = () => {
  const { t } = useTranslation();
  const session = useAppStore((s) => s.selectedSession);
  const [open, setOpen] = useState(false);
  const [data, setData] = useState<KbSessionNotes | null>(null);
  const [starting, setStarting] = useState(false);
  const running = useKbStore((s) => s.digest?.running != null);

  const source = (session?.provider ?? null) as KbSource | null;
  const sessionId = session?.actual_session_id ?? null;
  const isSub = (session as { is_subagent?: boolean } | null)?.is_subagent === true;

  const load = useCallback(async () => {
    if (source == null || sessionId == null) return;
    try {
      setData(await kbApi.notesForSession(source, sessionId));
    } catch {
      setData(null);
    }
  }, [source, sessionId]);

  useEffect(() => {
    setData(null);
    setOpen(false);
    void load();
  }, [load]);

  // A digest that just finished brings new notes.
  useEffect(() => {
    if (!running) void load();
  }, [running, load]);

  if (source == null || sessionId == null || isSub) return null;

  const digestNow = async () => {
    setStarting(true);
    const { started, error } = await useKbStore
      .getState()
      .runDigest({ kind: "session", source, session_id: sessionId });
    setStarting(false);
    if (error != null) toast.error(t("kb.digest.toast.failed", { message: error }));
    else if (started?.state === "busy") toast.info(t("kb.digest.toast.busy"));
    else if (started?.state === "nothing") toast.info(started.message);
    else toast.success(t("kb.digest.toast.sessionStarted"));
  };

  const jump = async (note: KbNote) => {
    setOpen(false);
    const found = await jumpToFocus(focusOfNote(note));
    if (!found) toast.info(t("kb.search.focusFailed"));
  };

  const count = data?.notes.length ?? 0;
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-muted-foreground hover:text-foreground hover:bg-muted transition-colors border border-border/50 text-xs"
      >
        <StickyNote className="w-3.5 h-3.5" aria-hidden="true" />
        {t("kb.sessionNotes.button", { count })}
      </button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-w-xl max-h-[85vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>{t("kb.sessionNotes.title")}</DialogTitle>
            <DialogDescription>
              {data?.status == null
                ? t("kb.sessionNotes.never")
                : data.status === "partial"
                  ? t("kb.sessionNotes.partial", { time: formatTime(data.digested_at ?? "") })
                  : t("kb.sessionNotes.digestedAt", { time: formatTime(data.digested_at ?? "") })}
            </DialogDescription>
          </DialogHeader>
          {data != null && data.notes.length === 0 && data.status === "nothing" && (
            <p className="text-sm text-muted-foreground">{t("kb.sessionNotes.nothing")}</p>
          )}
          <ul className="divide-y rounded-md border">
            {(data?.notes ?? []).map((note) => (
              <div key={note.entry_id}>
                <NoteCard note={note} startOpen />
                <div className="px-4 pb-3 pl-10">
                  <Button size="sm" variant="outline" onClick={() => void jump(note)}>
                    {t("kb.sessionNotes.jump", {
                      from: note.evidence.seq_from,
                      to: note.evidence.seq_to,
                    })}
                  </Button>
                </div>
              </div>
            ))}
          </ul>
          <p className="text-xs text-muted-foreground">{t("kb.sessionNotes.disclaimer")}</p>
          <DialogFooter>
            <Button variant="outline" disabled={starting || running} onClick={() => void digestNow()}>
              {starting && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
              {t("kb.sessionNotes.digestNow")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
};
