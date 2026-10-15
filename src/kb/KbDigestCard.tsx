import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { AlertTriangle, Loader2, StickyNote, Pause, Square } from "lucide-react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui";
import { formatTime } from "@/utils/time";
import { kbApi } from "./api";
import { formatMinutes, formatTokens, pausedText } from "./digestFormat";
import type { KbDigestEstimate, KbDigestFailure, KbDigestStatus } from "./types";
import { useKbStore } from "./useKbStore";

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** The home page's "daily digest" card: what ran last, what is waiting, what went wrong. */
export const KbDigestCard = () => {
  const { t } = useTranslation();
  const digest = useKbStore((s) => s.digest);
  const configState = useKbStore((s) => s.configState);
  const [confirm, setConfirm] = useState<KbDigestEstimate | null>(null);
  const [estimating, setEstimating] = useState(false);
  const [failuresOpen, setFailuresOpen] = useState(false);

  if (digest == null || configState?.state !== "valid") return null;

  const openSettings = () => useKbStore.getState().openSettings();

  const startAll = async () => {
    setEstimating(true);
    try {
      setConfirm(await kbApi.digestEstimateAll());
    } catch (e) {
      toast.error(t("kb.digest.toast.failed", { message: errorText(e) }));
    } finally {
      setEstimating(false);
    }
  };

  const confirmAll = async () => {
    setConfirm(null);
    const { started, error } = await useKbStore.getState().runDigest({ kind: "all" });
    if (error != null) toast.error(t("kb.digest.toast.failed", { message: error }));
    else if (started?.state === "busy") toast.info(t("kb.digest.toast.busy"));
    else toast.success(t("kb.digest.toast.started"));
  };

  const running = digest.running != null;
  return (
    <section
      aria-label={t("kb.digest.title")}
      className="space-y-2 rounded-md border p-3 text-sm"
    >
      <h3 className="flex items-center gap-2 font-medium">
        <StickyNote className="h-4 w-4" aria-hidden="true" />
        {t("kb.digest.title")}
      </h3>

      {!digest.enabled && !running ? (
        <div className="flex flex-wrap items-center gap-3">
          <span className="min-w-0 flex-1 text-muted-foreground">{t("kb.digest.off")}</span>
          <Button size="sm" onClick={openSettings}>
            {t("kb.digest.turnOn")}
          </Button>
        </div>
      ) : (
        <Body digest={digest} onOpenFailures={() => setFailuresOpen(true)} />
      )}

      {(digest.enabled || running) && (
        <div className="flex flex-wrap items-center gap-2 pt-1">
          <Button
            size="sm"
            variant="outline"
            disabled={running || estimating}
            onClick={() => void startAll()}
          >
            {estimating && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
            {t("kb.digest.runAll")}
          </Button>
          <Button size="sm" variant="ghost" onClick={openSettings}>
            {t("kb.digest.settings")}
          </Button>
        </div>
      )}

      <ConfirmDialog
        estimate={confirm}
        onCancel={() => setConfirm(null)}
        onConfirm={() => void confirmAll()}
      />
      <FailuresDialog
        open={failuresOpen}
        failures={digest.failures}
        onClose={() => setFailuresOpen(false)}
      />
    </section>
  );
};

const Body = ({
  digest,
  onOpenFailures,
}: {
  digest: KbDigestStatus;
  onOpenFailures: () => void;
}) => {
  const { t } = useTranslation();
  const { running, paused, last_round: last } = digest;

  const stop = async () => {
    const { error } = await useKbStore.getState().cancelDigest();
    if (error != null) toast.error(t("kb.digest.toast.failed", { message: error }));
  };

  return (
    <div className="space-y-1.5">
      {running != null && (
        <div role="status" aria-live="polite" className="space-y-1">
          <p className="flex flex-wrap items-center gap-2">
            <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
            <span className="min-w-0 flex-1 break-words">
              {running.current_title != null
                ? t("kb.digest.working", {
                    done: running.finished,
                    total: running.total,
                    title: running.current_title,
                  })
                : t("kb.digest.workingNoTitle", { done: running.finished, total: running.total })}
            </span>
            <Button size="sm" variant="outline" onClick={() => void stop()}>
              <Square className="mr-1 h-3 w-3" aria-hidden="true" />
              {t("kb.digest.stop")}
            </Button>
          </p>
          {running.total > 0 && (
            <div
              className="h-1.5 overflow-hidden rounded bg-muted"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={running.total}
              aria-valuenow={running.finished}
            >
              <div
                className="h-full bg-accent"
                style={{ width: `${Math.round((running.finished / running.total) * 100)}%` }}
              />
            </div>
          )}
        </div>
      )}

      {paused != null && running == null && (
        <p role="status" className="flex items-start gap-2 text-warning">
          <Pause className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
          <span className="min-w-0 break-words">
            {pausedText(paused, formatTime(paused.until), digest.engine, t)}
          </span>
        </p>
      )}

      {last != null ? (
        <p>
          {last.state === "done"
            ? t("kb.digest.lastDone", {
                time: formatTime(last.completed_at ?? last.planned_at),
                count: last.done,
              })
            : t("kb.digest.lastOpen", { time: formatTime(last.planned_at) })}
          {last.input_tokens + last.output_tokens > 0 && (
            <>
              {" · "}
              {t("kb.digest.lastTokens", {
                input: formatTokens(last.input_tokens, t),
                output: formatTokens(last.output_tokens, t),
              })}
            </>
          )}
        </p>
      ) : (
        <p className="text-muted-foreground">{t("kb.digest.never")}</p>
      )}

      {digest.first_round_tokens != null && (
        <p className="text-muted-foreground">
          {t("kb.digest.firstRound", { tokens: formatTokens(digest.first_round_tokens, t) })}
        </p>
      )}

      <p className="text-muted-foreground">
        {t("kb.digest.pending", { count: digest.pending_auto })}
        {digest.pending_older > 0 && <> · {t("kb.digest.pendingOlder", { count: digest.pending_older })}</>}
      </p>

      {digest.failures.length > 0 && (
        <p role="alert" className="flex flex-wrap items-center gap-2 text-destructive">
          <AlertTriangle className="h-4 w-4" aria-hidden="true" />
          {t("kb.digest.failures", { count: digest.failures.length })}
          <Button size="sm" variant="outline" onClick={onOpenFailures}>
            {t("kb.digest.viewFailures")}
          </Button>
        </p>
      )}

      {digest.next_run_at != null && digest.enabled && (
        <p className="text-xs text-muted-foreground">
          {t("kb.digest.next", { time: formatTime(digest.next_run_at) })}
        </p>
      )}
    </div>
  );
};

const ConfirmDialog = ({
  estimate,
  onCancel,
  onConfirm,
}: {
  estimate: KbDigestEstimate | null;
  onCancel: () => void;
  onConfirm: () => void;
}) => {
  const { t } = useTranslation();
  return (
    <Dialog open={estimate != null} onOpenChange={(open) => !open && onCancel()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("kb.digest.confirm.title")}</DialogTitle>
          <DialogDescription>{t("kb.digest.sharedQuota")}</DialogDescription>
        </DialogHeader>
        {estimate != null &&
          (estimate.sessions === 0 ? (
            <p className="text-sm">{t("kb.digest.confirm.none")}</p>
          ) : (
            <div className="space-y-2 text-sm">
              <p>
                {t("kb.digest.confirm.summary", {
                  sessions: estimate.sessions,
                  tokens: formatTokens(estimate.input_tokens, t),
                  time: formatMinutes(estimate.minutes, t),
                })}
              </p>
              <p className="text-muted-foreground">
                {t("kb.digest.confirm.using", {
                  engine: t(`kb.digest.engine.${estimate.engine}`),
                  model: estimate.model === "" ? t("kb.digest.defaultModel") : estimate.model,
                })}
              </p>
            </div>
          ))}
        <DialogFooter>
          <Button variant="ghost" onClick={onCancel}>
            {t("kb.digest.confirm.cancel")}
          </Button>
          <Button disabled={estimate == null || estimate.sessions === 0} onClick={onConfirm}>
            {t("kb.digest.confirm.start")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

const FailuresDialog = ({
  open,
  failures,
  onClose,
}: {
  open: boolean;
  failures: KbDigestFailure[];
  onClose: () => void;
}) => {
  const { t } = useTranslation();
  const retry = async (f: KbDigestFailure) => {
    const { started, error } = await useKbStore
      .getState()
      .runDigest({ kind: "session", source: f.source, session_id: f.session_id });
    if (error != null) toast.error(t("kb.digest.toast.failed", { message: error }));
    else if (started?.state === "busy") toast.info(t("kb.digest.toast.busy"));
    else if (started?.state === "nothing") toast.info(started.message);
    else {
      toast.success(t("kb.digest.toast.started"));
      onClose();
    }
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("kb.digest.failuresTitle")}</DialogTitle>
          <DialogDescription>{t("kb.digest.failuresHint")}</DialogDescription>
        </DialogHeader>
        <ul className="max-h-[60vh] space-y-2 overflow-y-auto">
          {failures.map((f) => (
            <li key={`${f.source}:${f.session_id}`} className="space-y-1 rounded-md border p-3 text-sm">
              <p className="break-words font-medium">{f.title ?? t("kb.sessions.untitled")}</p>
              <p className="break-words text-xs text-destructive">
                {t("kb.digest.failureLine", { count: f.attempts, message: f.error ?? "" })}
              </p>
              <Button size="sm" variant="outline" onClick={() => void retry(f)}>
                {t("kb.digest.retry")}
              </Button>
            </li>
          ))}
        </ul>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            {t("kb.settings.close")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};
