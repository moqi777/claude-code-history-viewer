import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui";
import { kbApi } from "./api";
import { Field, VerdictLine } from "./fields";
import { formatBytes } from "./format";
import { pickFolder } from "./pickFolder";
import { progressLabel } from "./progressLabel";
import { restoreErrorText } from "./restoreErrors";
import { splitTransferError } from "./transferErrors";
import type { KbBackupEntry, KbRestoreInspect, KbRestoreReport } from "./types";
import { useKbStore } from "./useKbStore";

type Stage =
  | { kind: "choose" }
  | { kind: "checking"; folder: string }
  | { kind: "pick"; entries: KbBackupEntry[] }
  | { kind: "ready"; folder: string; info: KbRestoreInspect }
  | { kind: "restoring"; folder: string; info: KbRestoreInspect }
  | { kind: "done"; report: KbRestoreReport }
  | { kind: "failed"; message: string };

const formatWhen = (iso: string): string => {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
};

/** The button in the backup-disk group; everything else happens in the dialog. */
export const RestoreField = ({ configuredFolder }: { configuredFolder: string | null }) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <Field label={t("kb.restore.title")} hint={t("kb.restore.hint")}>
      <div>
        <Button variant="outline" onClick={() => setOpen(true)}>
          {t("kb.restore.button")}
        </Button>
      </div>
      <RestoreDialog open={open} onClose={() => setOpen(false)} configuredFolder={configuredFolder} />
    </Field>
  );
};

export const RestoreDialog = ({
  open,
  onClose,
  configuredFolder,
}: {
  open: boolean;
  onClose: () => void;
  /** The `AgentKB-backup` folder of the disk set up for backups, if any. */
  configuredFolder: string | null;
}) => {
  const { t } = useTranslation();
  const progress = useKbStore((s) => s.progress);
  const [stage, setStage] = useState<Stage>({ kind: "choose" });

  const reset = () => setStage({ kind: "choose" });
  const close = () => {
    if (stage.kind === "restoring") return;
    reset();
    onClose();
  };

  const inspect = async (folder: string) => {
    setStage({ kind: "checking", folder });
    try {
      setStage({ kind: "ready", folder, info: await kbApi.restoreInspect(folder) });
    } catch (e) {
      setStage({ kind: "failed", message: restoreErrorText(e, t) });
    }
  };

  /** A folder is either one backup or holds one per computer. */
  const look = async (folder: string) => {
    setStage({ kind: "checking", folder });
    try {
      const entries = await kbApi.restoreList(folder);
      if (entries.length === 0) {
        setStage({ kind: "failed", message: restoreErrorText(new Error("backup_incomplete: no backup in this folder"), t) });
      } else if (entries.length === 1 && entries[0] != null) {
        await inspect(entries[0].folder);
      } else {
        setStage({ kind: "pick", entries });
      }
    } catch (e) {
      setStage({ kind: "failed", message: restoreErrorText(e, t) });
    }
  };

  const pick = async () => {
    const picked = await pickFolder(t("kb.restore.pickTitle"), configuredFolder ?? undefined);
    if (picked != null) await look(picked);
  };

  const start = async (folder: string, info: KbRestoreInspect) => {
    setStage({ kind: "restoring", folder, info });
    try {
      const report = await kbApi.restoreStart(folder);
      setStage({ kind: "done", report });
      toast.success(t("kb.restore.doneToast"));
      void useKbStore.getState().refresh();
      void useKbStore.getState().loadFacets();
    } catch (e) {
      const { code } = splitTransferError(e);
      // A cancel before the switch leaves everything as it was: back to the summary.
      setStage(
        code === "cancelled" ? { kind: "ready", folder, info } : { kind: "failed", message: restoreErrorText(e, t) },
      );
    } finally {
      useKbStore.getState().setProgress(null);
    }
  };

  const goSemantic = () => {
    close();
    // The semantic group is in the same settings dialog.
    setTimeout(() => document.getElementById("kb-semantic-field")?.scrollIntoView({ behavior: "smooth" }), 50);
  };

  const collectNow = () => {
    void useKbStore.getState().syncNow();
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && close()}>
      <DialogContent className="max-w-xl max-h-[88vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("kb.restore.dialogTitle")}</DialogTitle>
          <DialogDescription>{t("kb.restore.hint")}</DialogDescription>
        </DialogHeader>

        {stage.kind === "choose" && (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">{t("kb.restore.choose")}</p>
            {configuredFolder != null && (
              <Button onClick={() => void look(configuredFolder)} className="max-w-full">
                <span className="truncate">{t("kb.restore.useConfigured", { path: configuredFolder })}</span>
              </Button>
            )}
            <div>
              <Button variant="outline" onClick={() => void pick()}>
                {t("kb.restore.pick")}
              </Button>
            </div>
          </div>
        )}

        {stage.kind === "pick" && (
          <div className="space-y-2">
            <p className="text-sm text-muted-foreground">{t("kb.restore.pickComputer")}</p>
            <ul className="space-y-2" aria-label={t("kb.restore.pickComputer")}>
              {stage.entries.map((entry) => (
                <li key={entry.folder}>
                  <Button
                    variant="outline"
                    className="h-auto w-full justify-start whitespace-normal py-2 text-left"
                    onClick={() => void inspect(entry.folder)}
                  >
                    <span className="block space-y-0.5">
                      <span className="block font-medium">
                        {entry.old_layout ? t("kb.restore.oldLayout") : entry.name}
                        {entry.this_computer && ` · ${t("kb.restore.thisComputer")}`}
                      </span>
                      {!entry.old_layout && (
                        <span className="block text-xs text-muted-foreground">
                          {t("kb.restore.entry", {
                            os: entry.os,
                            when: entry.last_backup_at === "" ? "?" : formatWhen(entry.last_backup_at),
                            sessions: entry.sessions,
                          })}
                        </span>
                      )}
                    </span>
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}

        {stage.kind === "checking" && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
            <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
            {t("kb.restore.checking")}
          </p>
        )}

        {(stage.kind === "ready" || stage.kind === "restoring") && (
          <div className="space-y-3">
            <p className="break-all font-mono text-xs text-muted-foreground">{stage.folder}</p>
            <div className="space-y-1 rounded-md border p-3 text-sm">
              <p>
                {t("kb.restore.summary", {
                  sessions: stage.info.sessions,
                  notes: stage.info.notes,
                  files: stage.info.raw_files,
                  size: formatBytes(stage.info.raw_bytes),
                })}
              </p>
              <p className="text-muted-foreground">
                {t("kb.restore.snapshotAt", { when: formatWhen(stage.info.snapshot_at) })}
              </p>
              {stage.info.needs_upgrade && (
                <p className="text-muted-foreground">{t("kb.restore.upgrade")}</p>
              )}
            </div>
            <p className="text-sm">
              {stage.info.current_empty
                ? t("kb.restore.willSetAsideNothing")
                : t("kb.restore.willSetAside", {
                    sessions: stage.info.current_sessions,
                    notes: stage.info.current_notes,
                  })}
            </p>
            <p className="text-xs text-muted-foreground">{t("kb.restore.afterNote")}</p>
            {stage.info.free_bytes != null && stage.info.free_bytes < stage.info.needed_bytes && (
              <VerdictLine
                verdict={{
                  tone: "error",
                  text: t("kb.restore.needSpace", {
                    needed: formatBytes(stage.info.needed_bytes),
                    free: formatBytes(stage.info.free_bytes),
                  }),
                }}
              />
            )}
            {stage.kind === "restoring" && (
              <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
                <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
                {progress != null && progress.op === "restore"
                  ? progressLabel(progress, t)
                  : t("kb.restore.working")}
              </p>
            )}
          </div>
        )}

        {stage.kind === "done" && <Done report={stage.report} onSemantic={goSemantic} onCollect={collectNow} />}

        {stage.kind === "failed" && <VerdictLine verdict={{ tone: "error", text: stage.message }} />}

        <DialogFooter>
          {stage.kind === "done" ? (
            <Button onClick={close}>{t("kb.transfer.close")}</Button>
          ) : (
            <>
              <Button
                variant="ghost"
                onClick={() => {
                  if (stage.kind === "restoring") void kbApi.transferCancel();
                  else close();
                }}
              >
                {t("kb.transfer.cancel")}
              </Button>
              {stage.kind === "ready" && (
                <Button
                  disabled={stage.info.free_bytes != null && stage.info.free_bytes < stage.info.needed_bytes}
                  onClick={() => void start(stage.folder, stage.info)}
                >
                  {t("kb.restore.confirm")}
                </Button>
              )}
              {stage.kind === "failed" && (
                <Button variant="outline" onClick={reset}>
                  {t("kb.transfer.import.again")}
                </Button>
              )}
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

const Done = ({
  report,
  onSemantic,
  onCollect,
}: {
  report: KbRestoreReport;
  onSemantic: () => void;
  onCollect: () => void;
}) => {
  const { t } = useTranslation();
  return (
    <div className="space-y-3 text-sm">
      <VerdictLine
        verdict={{
          tone: "ok",
          text: t("kb.restore.done", { sessions: report.sessions, notes: report.notes }),
        }}
      />
      {report.set_aside != null && (
        <p className="break-all text-xs text-muted-foreground">
          {t("kb.restore.doneAside", { path: report.set_aside })}
        </p>
      )}
      {report.caught_up_files > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("kb.restore.doneCaughtUp", { files: report.caught_up_files })}
        </p>
      )}
      {report.missing_files > 0 && (
        <p className="text-xs text-warning">{t("kb.restore.doneMissing", { count: report.missing_files })}</p>
      )}
      <div className="space-y-2 rounded-md border p-3">
        <p className="font-medium">{t("kb.restore.semanticTitle")}</p>
        <p className="text-xs text-muted-foreground">{t("kb.restore.semanticBody")}</p>
        <Button variant="outline" size="sm" onClick={onSemantic}>
          {t("kb.restore.goSemantic")}
        </Button>
      </div>
      <div className="space-y-2 rounded-md border p-3">
        <p className="text-xs text-muted-foreground">{t("kb.restore.collectBody")}</p>
        <Button variant="outline" size="sm" onClick={onCollect}>
          {t("kb.restore.collect")}
        </Button>
      </div>
    </div>
  );
};

/** Which folder of the disk is this computer's, and a note about an older backup. */
export const BackupFolderLine = () => {
  const { t } = useTranslation();
  const backup = useKbStore((s) => s.overview?.backup);
  const enabled = useKbStore((s) => s.overview?.mirror.enabled === true);
  if (!enabled || backup == null) return null;
  return (
    <div className="space-y-1 text-xs text-muted-foreground">
      <p className="break-all">
        {backup.folder != null
          ? t("kb.backupFolder.line", { path: backup.folder })
          : t("kb.backupFolder.notYet")}
      </p>
      {backup.old_layout && <p role="note">{t("kb.backupFolder.oldLayout")}</p>}
    </div>
  );
};
