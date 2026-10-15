import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
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
import { supportedLanguages, type SupportedLanguage } from "@/i18n";
import { useAppStore } from "@/store/useAppStore";
import { useLanguageStore } from "@/store/useLanguageStore";
import { kbApi } from "./api";
import { DataDirField, IntervalField, MirrorField, SourcesField, VerdictLine } from "./fields";
import { DigestField } from "./KbDigestField";
import { formatBytes } from "./format";
import { SkillField } from "./SkillField";
import { dataDirAcceptable, digestValid } from "./logic";
import { progressLabel } from "./progressLabel";
import type { KbConfig, KbDataDirReport, KbWizardResult } from "./types";
import { useKbStore } from "./useKbStore";

const STEPS = ["sources", "dataDir", "interval", "mirror", "digest", "confirm"] as const;
type Step = (typeof STEPS)[number];

type Phase =
  | { kind: "editing" }
  | { kind: "running" }
  | { kind: "done"; result: WizardOutcome }
  | { kind: "failed"; message: string };

type WizardOutcome = KbWizardResult;

/** Language names are shown in their own language, so the picker needs no translation. */
const LanguagePicker = () => {
  const { t } = useTranslation();
  const language = useLanguageStore((s) => s.language);
  const setLanguage = useLanguageStore((s) => s.setLanguage);
  return (
    <div
      className="flex flex-wrap items-center gap-2"
      role="group"
      aria-label={t("common.settings.language.title")}
    >
      <span className="text-sm text-muted-foreground">{t("common.settings.language.title")}</span>
      {Object.entries(supportedLanguages).map(([code, name]) => (
        <Button
          key={code}
          size="sm"
          variant={language === code ? "default" : "outline"}
          aria-pressed={language === code}
          onClick={() => void setLanguage(code as SupportedLanguage)}
        >
          {name}
        </Button>
      ))}
    </div>
  );
};

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export const KbWizard = () => {
  const { t } = useTranslation();
  const wizard = useKbStore((s) => s.wizard);
  const progress = useKbStore((s) => s.progress);

  const [step, setStep] = useState<Step>("sources");
  const [draft, setDraft] = useState<KbConfig | null>(null);
  const [dirReport, setDirReport] = useState<KbDataDirReport | null>(null);
  const [mirrorOk, setMirrorOk] = useState(false);
  const [phase, setPhase] = useState<Phase>({ kind: "editing" });

  // Every opening starts from the saved configuration (or the defaults): the
  // draft lives only in memory, so closing or quitting leaves nothing behind.
  useEffect(() => {
    if (!wizard.open) return;
    setStep("sources");
    setPhase({ kind: "editing" });
    setDirReport(null);
    setMirrorOk(false);
    const saved = useKbStore.getState().configState;
    setDraft(saved?.config != null ? structuredClone(saved.config) : null);
  }, [wizard.open]);

  const locked = wizard.mode === "rerun";
  const index = STEPS.indexOf(step);

  const canAdvance = useMemo(() => {
    if (draft == null) return false;
    switch (step) {
      case "sources":
        return draft.sources.claude.enabled || draft.sources.codex.enabled;
      case "dataDir":
        return locked || dataDirAcceptable(dirReport);
      case "interval":
        return draft.interval_secs >= 60 && draft.interval_secs <= 86_400;
      case "mirror":
        return !draft.mirror.enabled || mirrorOk;
      case "digest":
        return digestValid(draft.digest);
      default:
        return true;
    }
  }, [draft, step, dirReport, mirrorOk, locked]);

  const commit = useCallback(async () => {
    if (draft == null) return;
    setPhase({ kind: "running" });
    try {
      const result = await kbApi.wizardCommit(draft);
      setPhase({ kind: "done", result });
      await useKbStore.getState().loadConfigState();
    } catch (e) {
      setPhase({ kind: "failed", message: errorText(e) });
    } finally {
      useKbStore.getState().setProgress(null);
    }
  }, [draft]);

  const finish = useCallback(() => {
    useKbStore.getState().closeWizard();
    useAppStore.getState().setAnalyticsCurrentView("kb");
    void useKbStore.getState().refresh();
    void useKbStore.getState().loadFacets();
    void useKbStore.getState().refreshPresence();
  }, []);

  const running = phase.kind === "running";

  return (
    <Dialog
      open={wizard.open}
      onOpenChange={(open) => {
        // Not while the first collection is writing the configuration.
        if (!open && !running) useKbStore.getState().closeWizard();
      }}
    >
      <DialogContent className="max-w-2xl max-h-[88vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("kb.wizard.title")}</DialogTitle>
          <DialogDescription>{t("kb.wizard.description")}</DialogDescription>
        </DialogHeader>

        <ol className="flex flex-wrap gap-2 text-xs" aria-label={t("kb.wizard.steps")}>
          {STEPS.map((s, i) => (
            <li
              key={s}
              aria-current={s === step ? "step" : undefined}
              className={
                s === step
                  ? "rounded-full bg-accent px-3 py-1 font-medium text-accent-foreground"
                  : "rounded-full border px-3 py-1 text-muted-foreground"
              }
            >
              {i + 1}. {t(`kb.wizard.step.${s}`)}
            </li>
          ))}
        </ol>

        {draft == null ? (
          <p className="text-sm text-muted-foreground">{t("kb.wizard.loading")}</p>
        ) : phase.kind === "editing" || phase.kind === "failed" ? (
          <div className="space-y-5">
            {step === "sources" && (
              <>
                <p className="text-sm">{t("kb.wizard.welcome")}</p>
                <LanguagePicker />
                <SourcesField draft={draft} onChange={setDraft} />
              </>
            )}
            {step === "dataDir" && (
              <DataDirField
                value={draft.data_dir}
                locked={locked}
                onChange={(value) => setDraft({ ...draft, data_dir: value })}
                onReport={setDirReport}
              >
                {locked && (
                  <p className="text-xs text-muted-foreground">{t("kb.wizard.dataDirLocked")}</p>
                )}
              </DataDirField>
            )}
            {step === "interval" && <IntervalField draft={draft} onChange={setDraft} />}
            {step === "mirror" && (
              <>
                <MirrorField
                  mirror={draft.mirror}
                  onChange={(mirror) => setDraft({ ...draft, mirror })}
                  onTested={setMirrorOk}
                />
                <p className="text-xs text-muted-foreground">{t("kb.wizard.mirrorSkip")}</p>
              </>
            )}
            {step === "digest" && (
              <>
                <DigestField draft={draft} onChange={setDraft} showEstimate={false} />
                <p className="text-xs text-muted-foreground">{t("kb.digest.wizardSkip")}</p>
              </>
            )}
            {step === "confirm" && <Summary draft={draft} existing={dirReport} />}
            {phase.kind === "failed" && (
              <VerdictLine
                verdict={{
                  tone: "error",
                  text: t("kb.wizard.failed", { message: phase.message }),
                }}
              />
            )}
          </div>
        ) : phase.kind === "running" ? (
          <div className="space-y-3" role="status" aria-live="polite">
            <p className="flex items-center gap-2 text-sm">
              <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
              {t("kb.wizard.running")}
            </p>
            {progress != null && (
              <p className="text-sm text-muted-foreground">{progressLabel(progress, t)}</p>
            )}
          </div>
        ) : (
          <Done result={phase.result} />
        )}

        <DialogFooter className="gap-2 sm:justify-between">
          {phase.kind === "done" ? (
            <Button onClick={finish} className="ml-auto">
              {t("kb.wizard.enter")}
            </Button>
          ) : (
            <>
              <Button
                variant="ghost"
                disabled={running}
                onClick={() => useKbStore.getState().closeWizard()}
              >
                {t("kb.wizard.later")}
              </Button>
              <div className="flex gap-2">
                <Button
                  variant="outline"
                  disabled={running || index === 0}
                  onClick={() => setStep(STEPS[index - 1] ?? step)}
                >
                  {t("kb.wizard.back")}
                </Button>
                {step === "confirm" ? (
                  <Button disabled={running || draft == null} onClick={() => void commit()}>
                    {phase.kind === "failed" ? t("kb.wizard.retry") : t("kb.wizard.start")}
                  </Button>
                ) : (
                  <Button
                    disabled={!canAdvance || running}
                    onClick={() => setStep(STEPS[index + 1] ?? step)}
                  >
                    {t("kb.wizard.next")}
                  </Button>
                )}
              </div>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

const Summary = ({ draft, existing }: { draft: KbConfig; existing: KbDataDirReport | null }) => {
  const { t } = useTranslation();
  const sources = (["claude", "codex"] as const)
    .filter((s) => draft.sources[s].enabled)
    .map((s) => t(`kb.sessions.filter.${s}`))
    .join(" · ");
  const rows: [string, string][] = [
    [t("kb.field.sources"), sources],
    [t("kb.field.dataDir"), draft.data_dir],
    [
      t("kb.field.interval"),
      draft.schedule_enabled
        ? t("kb.field.everyMinutes", { count: Math.round(draft.interval_secs / 60) })
        : t("kb.status.schedule.notInstalled"),
    ],
    [
      t("kb.field.mirror"),
      draft.mirror.enabled
        ? `${draft.mirror.volume_name ?? ""} ${draft.mirror.target_dir ?? ""}`.trim()
        : t("kb.wizard.mirrorOff"),
    ],
    [
      t("kb.digest.field.title"),
      draft.digest.enabled
        ? t("kb.digest.summaryOn", {
            engine: t(`kb.digest.engine.${draft.digest.engine}`),
            time: draft.digest.time,
          })
        : t("kb.digest.summaryOff"),
    ],
  ];
  return (
    <div className="space-y-3">
      <p className="text-sm">{t("kb.wizard.confirmIntro")}</p>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        {rows.map(([label, value]) => (
          <div key={label} className="contents">
            <dt className="text-muted-foreground">{label}</dt>
            <dd className="break-all">{value}</dd>
          </div>
        ))}
      </dl>
      {existing?.existing != null && (
        <p className="text-sm text-success">
          {t("kb.wizard.reuseExisting", {
            sessions: existing.existing.sessions,
            size: formatBytes(existing.existing.bytes),
          })}
        </p>
      )}
    </div>
  );
};

const Done = ({ result }: { result: WizardOutcome }) => {
  const { t } = useTranslation();
  const { sync } = result;
  return (
    <div className="space-y-2 text-sm" role="status">
      <p className="font-medium text-success">{t("kb.wizard.done")}</p>
      <p>
        {t("kb.wizard.doneCollected", {
          files: sync.changed_files,
          bytes: formatBytes(sync.copied_bytes),
        })}
      </p>
      {result.schedule != null && <p>{t("kb.wizard.doneSchedule")}</p>}
      {result.schedule_error != null && (
        <VerdictLine
          verdict={{
            tone: "warn",
            text: t("kb.wizard.doneScheduleFailed", { message: result.schedule_error }),
          }}
        />
      )}
      {result.backup_started && <p>{t("kb.wizard.doneBackup")}</p>}
      {sync.errors.length > 0 && (
        <VerdictLine
          verdict={{
            tone: "warn",
            text: t("kb.toast.syncProblems", { count: sync.errors.length }),
          }}
        />
      )}
      <div className="border-t pt-4">
        <p className="mb-1 font-medium">{t("kb.wizard.agentsTitle")}</p>
        <p className="mb-3 text-muted-foreground">{t("kb.wizard.agentsIntro")}</p>
        <SkillField />
      </div>
    </div>
  );
};
