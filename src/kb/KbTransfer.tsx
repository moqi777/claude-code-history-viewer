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
import { isTauri } from "@/utils/platform";
import { kbApi } from "./api";
import { Field, VerdictLine } from "./fields";
import { formatBytes } from "./format";
import { progressLabel } from "./progressLabel";
import { splitTransferError, transferErrorText } from "./transferErrors";
import type {
  KbExportReport,
  KbImportInspect,
  KbImportPrepared,
  KbImportReport,
} from "./types";
import { useKbStore } from "./useKbStore";

const FILE_FILTER = [{ name: "AgentKB", extensions: ["agentkb"] }];

const today = (): string => new Date().toISOString().slice(0, 10);

const formatWhen = (iso: string): string => {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
};

// ---------------------------------------------------------------------------
// Settings: the "move to another computer" group
// ---------------------------------------------------------------------------

export const TransferSection = () => {
  const { t } = useTranslation();
  const progress = useKbStore((s) => s.progress);
  const [exporting, setExporting] = useState(false);
  const [report, setReport] = useState<KbExportReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [importOpen, setImportOpen] = useState(false);

  const exportNow = async () => {
    if (!isTauri()) return;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const dest = await save({
      defaultPath: `AgentKB-${today()}.agentkb`,
      filters: FILE_FILTER,
    });
    if (typeof dest !== "string") return;
    setExporting(true);
    setReport(null);
    setError(null);
    try {
      // The save dialog has already asked about replacing an existing file.
      setReport(await kbApi.exportStart(dest, true));
    } catch (e) {
      const { code } = splitTransferError(e);
      if (code !== "cancelled") setError(transferErrorText(e, t));
    } finally {
      setExporting(false);
      useKbStore.getState().setProgress(null);
    }
  };

  return (
    <Field label={t("kb.transfer.title")} hint={t("kb.transfer.hint")}>
      <div className="space-y-4">
        <div className="space-y-2">
          <p className="text-sm font-medium">{t("kb.transfer.export.title")}</p>
          <p className="text-xs text-muted-foreground">{t("kb.transfer.export.hint")}</p>
          <p className="text-xs text-warning" role="note">
            {t("kb.transfer.export.warn")}
          </p>
          <div className="flex items-center gap-2">
            <Button variant="outline" disabled={exporting} onClick={() => void exportNow()}>
              {exporting && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
              {exporting ? t("kb.transfer.export.running") : t("kb.transfer.export.button")}
            </Button>
            {exporting && (
              <Button variant="ghost" onClick={() => void kbApi.transferCancel()}>
                {t("kb.transfer.cancel")}
              </Button>
            )}
          </div>
          {exporting && progress != null && (progress.op === "export" || progress.op === "import") && (
            <p className="text-sm text-muted-foreground" role="status">
              {progressLabel(progress, t)}
            </p>
          )}
          {report != null && (
            <div className="space-y-1">
              <VerdictLine
                verdict={{
                  tone: "ok",
                  text: t("kb.transfer.export.done", {
                    sessions: report.sessions,
                    notes: report.notes,
                    size: formatBytes(report.file_bytes),
                    seconds: Math.max(1, Math.round(report.duration_ms / 1000)),
                  }),
                }}
              />
              <p className="break-all font-mono text-xs text-muted-foreground">{report.path}</p>
              {report.paused_collection && (
                <p className="text-xs text-muted-foreground">{t("kb.transfer.export.paused")}</p>
              )}
            </div>
          )}
          {error != null && <VerdictLine verdict={{ tone: "error", text: error }} />}
        </div>

        <div className="space-y-2 border-t pt-4">
          <p className="text-sm font-medium">{t("kb.transfer.import.title")}</p>
          <p className="text-xs text-muted-foreground">{t("kb.transfer.import.hint")}</p>
          <Button variant="outline" onClick={() => setImportOpen(true)}>
            {t("kb.transfer.import.button")}
          </Button>
        </div>
      </div>
      <ImportDialog
        open={importOpen}
        onClose={() => setImportOpen(false)}
        wizard={false}
        onImported={() => {
          void useKbStore.getState().refresh();
          void useKbStore.getState().loadFacets();
        }}
      />
    </Field>
  );
};

// ---------------------------------------------------------------------------
// Import: choose a file, look, unpack and check, preview, apply
// ---------------------------------------------------------------------------

type Stage =
  | { kind: "choose" }
  | { kind: "info"; path: string; info: KbImportInspect }
  | { kind: "preparing"; path: string; info: KbImportInspect }
  | { kind: "preview"; path: string; info: KbImportInspect; prepared: KbImportPrepared }
  | { kind: "committing"; path: string; prepared: KbImportPrepared }
  | { kind: "done"; report: KbImportReport }
  | { kind: "failed"; message: string };

export const ImportDialog = ({
  open,
  onClose,
  wizard,
  dataDir,
  onImported,
}: {
  open: boolean;
  onClose: () => void;
  /** In the wizard the form is filled from the result and `config.json` is left alone. */
  wizard: boolean;
  dataDir?: string;
  onImported: (report: KbImportReport) => void;
}) => {
  const { t } = useTranslation();
  const progress = useKbStore((s) => s.progress);
  const [stage, setStage] = useState<Stage>({ kind: "choose" });
  const [adopt, setAdopt] = useState(false);

  const busy = stage.kind === "preparing" || stage.kind === "committing";

  const reset = () => {
    setStage({ kind: "choose" });
    setAdopt(false);
  };

  const close = () => {
    if (busy) return;
    if (stage.kind === "preview") {
      void kbApi.importDiscard(stage.path, dataDir).catch(() => undefined);
    }
    reset();
    onClose();
  };

  const choose = async () => {
    if (!isTauri()) return;
    const { open: openDialog } = await import("@tauri-apps/plugin-dialog");
    const picked = await openDialog({ multiple: false, directory: false, filters: FILE_FILTER });
    if (typeof picked !== "string") return;
    try {
      const info = await kbApi.importInspect(picked, dataDir);
      setStage({ kind: "info", path: picked, info });
    } catch (e) {
      setStage({ kind: "failed", message: transferErrorText(e, t) });
    }
  };

  const prepare = async (path: string, info: KbImportInspect) => {
    setStage({ kind: "preparing", path, info });
    try {
      const prepared = await kbApi.importPrepare(path, adopt, dataDir);
      setStage({ kind: "preview", path, info, prepared });
    } catch (e) {
      const { code } = splitTransferError(e);
      setStage(code === "cancelled" ? { kind: "info", path, info } : { kind: "failed", message: transferErrorText(e, t) });
    } finally {
      useKbStore.getState().setProgress(null);
    }
  };

  const commit = async (path: string, prepared: KbImportPrepared) => {
    setStage({ kind: "committing", path, prepared });
    try {
      const report = await kbApi.importCommit(path, adopt, !wizard, dataDir);
      setStage({ kind: "done", report });
      onImported(report);
      if (!wizard) {
        toast.success(t("kb.transfer.import.doneToast"));
        // Recognise sources that exist here and bring the index up to date.
        void kbApi.syncNow().catch(() => undefined);
        void useKbStore.getState().loadConfigState();
      }
    } catch (e) {
      setStage({ kind: "failed", message: transferErrorText(e, t) });
    } finally {
      useKbStore.getState().setProgress(null);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && close()}>
      <DialogContent className="max-w-xl max-h-[88vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("kb.transfer.import.dialogTitle")}</DialogTitle>
          <DialogDescription>{t("kb.transfer.import.hint")}</DialogDescription>
        </DialogHeader>

        {stage.kind === "choose" && (
          <div className="space-y-3">
            <p className="text-sm text-muted-foreground">{t("kb.transfer.import.choose")}</p>
            <Button onClick={() => void choose()}>{t("kb.transfer.import.pick")}</Button>
          </div>
        )}

        {(stage.kind === "info" || stage.kind === "preparing") && (
          <div className="space-y-3">
            <FileSummary info={stage.info} />
            {stage.info.problem != null ? (
              <VerdictLine
                verdict={{ tone: "error", text: t(`kb.transfer.error.${stage.info.problem}`, { message: "" }) }}
              />
            ) : (
              <>
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={adopt}
                    disabled={busy}
                    onChange={(e) => setAdopt(e.target.checked)}
                  />
                  {t("kb.transfer.import.adopt")}
                </label>
                <p className="text-xs text-muted-foreground">{t("kb.transfer.import.digestOff")}</p>
                {stage.info.free_bytes != null && stage.info.free_bytes < stage.info.needed_bytes && (
                  <VerdictLine
                    verdict={{
                      tone: "error",
                      text: t("kb.transfer.import.needSpace", {
                        needed: formatBytes(stage.info.needed_bytes),
                        free: formatBytes(stage.info.free_bytes),
                      }),
                    }}
                  />
                )}
              </>
            )}
            {stage.kind === "preparing" && <Working progress={progress} />}
          </div>
        )}

        {stage.kind === "preview" && (
          <div className="space-y-3">
            <FileSummary info={stage.info} />
            <Preview prepared={stage.prepared} />
          </div>
        )}

        {stage.kind === "committing" && (
          <div className="space-y-3">
            <Preview prepared={stage.prepared} />
            <Working progress={progress} />
          </div>
        )}

        {stage.kind === "done" && <Done report={stage.report} wizard={wizard} />}

        {stage.kind === "failed" && (
          <VerdictLine verdict={{ tone: "error", text: stage.message }} />
        )}

        <DialogFooter>
          {stage.kind === "done" ? (
            <Button onClick={close}>{t("kb.transfer.close")}</Button>
          ) : (
            <>
              <Button variant="ghost" disabled={busy && stage.kind === "committing"} onClick={() => {
                if (busy) void kbApi.transferCancel();
                else close();
              }}>
                {t("kb.transfer.cancel")}
              </Button>
              {stage.kind === "info" && stage.info.problem == null && (
                <Button onClick={() => void prepare(stage.path, stage.info)}>
                  {t("kb.transfer.import.check")}
                </Button>
              )}
              {stage.kind === "preview" && (
                <Button onClick={() => void commit(stage.path, stage.prepared)}>
                  {t("kb.transfer.import.confirm")}
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

const Working = ({ progress }: { progress: ReturnType<typeof useKbStore.getState>["progress"] }) => {
  const { t } = useTranslation();
  return (
    <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
      <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
      {progress != null && progress.op === "import"
        ? progressLabel(progress, t)
        : t("kb.transfer.import.working")}
    </p>
  );
};

const FileSummary = ({ info }: { info: KbImportInspect }) => {
  const { t } = useTranslation();
  return (
    <div className="space-y-1 rounded-md border p-3 text-sm">
      <p>
        {t("kb.transfer.import.from", {
          device: info.device_name,
          version: info.app_version,
          when: formatWhen(info.exported_at),
        })}
      </p>
      <p className="text-muted-foreground">
        {t("kb.transfer.import.contents", {
          sessions: info.counts.sessions,
          notes: info.counts.notes,
          size: formatBytes(info.file_bytes),
        })}
      </p>
    </div>
  );
};

const Preview = ({ prepared }: { prepared: KbImportPrepared }) => {
  const { t } = useTranslation();
  const s = prepared.summary;
  return (
    <div className="space-y-2 text-sm">
      {prepared.mode === "empty" || s == null ? (
        <p>
          {t("kb.transfer.import.previewEmpty", {
            sessions: prepared.counts.sessions,
            notes: prepared.counts.notes,
          })}
        </p>
      ) : (
        <>
          <p>
            {t("kb.transfer.import.previewMerge", {
              added: s.sessions_added,
              same: s.identical,
              extended: s.extended,
              conflicts: s.notes_conflicts,
            })}
          </p>
          {s.kept_both_live + s.kept_both_diverged + s.kept_both_other > 0 && (
            <p className="text-muted-foreground">
              {t("kb.transfer.import.previewBoth", {
                count: s.kept_both_live + s.kept_both_diverged + s.kept_both_other,
              })}
            </p>
          )}
          <p className="text-xs text-muted-foreground">{t("kb.transfer.import.backupNote")}</p>
        </>
      )}
      {prepared.added_exclusions.length > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("kb.transfer.import.exclusions", {
            count: prepared.added_exclusions.length,
            list: prepared.added_exclusions.join(", "),
          })}
        </p>
      )}
    </div>
  );
};

const Done = ({ report, wizard }: { report: KbImportReport; wizard: boolean }) => {
  const { t } = useTranslation();
  const sessions = report.checks.find((c) => c.name === "sessions")?.actual ?? 0;
  const notes = report.checks.find((c) => c.name === "notes")?.actual ?? 0;
  const off = report.checks.filter((c) => c.expected !== c.actual);
  return (
    <div className="space-y-2 text-sm" role="status">
      <VerdictLine
        verdict={{
          tone: "ok",
          text: t(wizard ? "kb.transfer.import.doneWizard" : "kb.transfer.import.done", {
            sessions,
            notes,
          }),
        }}
      />
      {off.length > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("kb.transfer.import.mismatch", {
            list: off.map((c) => `${c.name} ${c.actual}/${c.expected}`).join(", "),
          })}
        </p>
      )}
      {report.warnings.length > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("kb.transfer.import.warnings", { count: report.warnings.length })}
        </p>
      )}
    </div>
  );
};
