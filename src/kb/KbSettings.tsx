import { useCallback, useEffect, useMemo, useState } from "react";
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
  Input,
} from "@/components/ui";
import { kbApi } from "./api";
import { Field, IntervalField, MirrorField, SourcesField, VerdictLine } from "./fields";
import { DigestField } from "./KbDigestField";
import { digestValid, excludesValid, parseExcludes } from "./logic";
import { SkillField } from "./SkillField";
import { TransferSection } from "./KbTransfer";
import { useTest } from "./useTest";
import { describeMigrateCheck } from "./messages";
import { pickFolder } from "./pickFolder";
import { progressLabel } from "./progressLabel";
import type { KbConfig, KbMigrateCheck } from "./types";
import { useKbStore } from "./useKbStore";

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export const KbSettings = () => {
  const { t } = useTranslation();
  const open = useKbStore((s) => s.settingsOpen);
  const overview = useKbStore((s) => s.overview);
  const [saved, setSaved] = useState<KbConfig | null>(null);
  const [draft, setDraft] = useState<KbConfig | null>(null);
  const [excludeText, setExcludeText] = useState("");
  const [excludeCwdText, setExcludeCwdText] = useState("");
  const [mirrorOk, setMirrorOk] = useState(true);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoadError(null);
    kbApi
      .getConfig()
      .then((config) => {
        if (cancelled) return;
        setSaved(config);
        setDraft(structuredClone(config));
        setExcludeText(config.exclude_prefixes.join("\n"));
        setExcludeCwdText((config.exclude_cwd_prefixes ?? []).join("\n"));
        setMirrorOk(true);
      })
      .catch((e) => !cancelled && setLoadError(errorText(e)));
    return () => {
      cancelled = true;
    };
  }, [open]);

  const excludes = useMemo(() => parseExcludes(excludeText), [excludeText]);
  const excludeCwds = useMemo(() => parseExcludes(excludeCwdText), [excludeCwdText]);
  const candidate = useMemo(
    () =>
      draft == null
        ? null
        : { ...draft, exclude_prefixes: excludes, exclude_cwd_prefixes: excludeCwds },
    [draft, excludes, excludeCwds],
  );
  const dirty = candidate != null && saved != null && JSON.stringify(candidate) !== JSON.stringify(saved);
  const mirrorChanged =
    candidate != null &&
    saved != null &&
    candidate.mirror.enabled &&
    (!saved.mirror.enabled ||
      candidate.mirror.target_dir !== saved.mirror.target_dir ||
      candidate.mirror.volume_uuid !== saved.mirror.volume_uuid);
  const valid =
    candidate != null &&
    excludesValid(excludes) &&
    excludesValid(excludeCwds) &&
    candidate.interval_secs >= 60 &&
    digestValid(candidate.digest) &&
    (candidate.sources.claude.enabled || candidate.sources.codex.enabled) &&
    (!mirrorChanged || mirrorOk);

  const save = useCallback(async () => {
    if (candidate == null) return;
    setSaving(true);
    try {
      const outcome = await kbApi.saveConfig(candidate);
      setSaved(outcome.config);
      setDraft(structuredClone(outcome.config));
      if (outcome.schedule_error != null) {
        toast.error(t("kb.settings.scheduleNotUpdated", { message: outcome.schedule_error }));
      } else {
        toast.success(t("kb.settings.saved"));
      }
      await useKbStore.getState().loadConfigState();
      await useKbStore.getState().refresh();
      void useKbStore.getState().refreshPresence();
    } catch (e) {
      toast.error(t("kb.settings.saveFailed", { message: errorText(e) }));
    } finally {
      setSaving(false);
    }
  }, [candidate, t]);

  return (
    <Dialog open={open} onOpenChange={(next) => !next && useKbStore.getState().closeSettings()}>
      <DialogContent className="max-w-2xl max-h-[88vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("kb.settings.title")}</DialogTitle>
          <DialogDescription>{t("kb.settings.description")}</DialogDescription>
        </DialogHeader>

        {loadError != null && (
          <VerdictLine verdict={{ tone: "error", text: t("kb.error.loadFailed", { message: loadError }) }} />
        )}
        {draft == null || saved == null ? (
          loadError == null && <p className="text-sm text-muted-foreground">{t("kb.wizard.loading")}</p>
        ) : (
          <div className="space-y-6">
            <SourcesField draft={draft} onChange={setDraft} />
            <IntervalField draft={draft} onChange={setDraft} />

            <Field label={t("kb.field.exclude")} hint={t("kb.field.excludeHint")}>
              <textarea
                value={excludeText}
                onChange={(e) => setExcludeText(e.target.value)}
                rows={3}
                aria-label={t("kb.field.exclude")}
                aria-invalid={!excludesValid(excludes)}
                className="w-full rounded-md border bg-background p-2 font-mono text-xs"
              />
              {!excludesValid(excludes) && (
                <p className="text-xs text-destructive" role="alert">
                  {t("kb.field.excludeInvalid")}
                </p>
              )}
            </Field>

            <Field label={t("kb.field.excludeCwd")} hint={t("kb.field.excludeCwdHint")}>
              <textarea
                value={excludeCwdText}
                onChange={(e) => setExcludeCwdText(e.target.value)}
                rows={3}
                aria-label={t("kb.field.excludeCwd")}
                aria-invalid={!excludesValid(excludeCwds)}
                className="w-full rounded-md border bg-background p-2 font-mono text-xs"
              />
              {!excludesValid(excludeCwds) && (
                <p className="text-xs text-destructive" role="alert">
                  {t("kb.field.excludeInvalid")}
                </p>
              )}
            </Field>

            <MirrorField
              mirror={draft.mirror}
              onChange={(mirror) => setDraft({ ...draft, mirror })}
              onTested={setMirrorOk}
            />

            <SkillField />

            <DigestField draft={draft} onChange={setDraft} />

            <MoveDataDir current={saved.data_dir} />

            <TransferSection />

            <div className="flex flex-wrap items-center gap-2 border-t pt-4">
              <Button
                variant="outline"
                onClick={() => {
                  useKbStore.getState().closeSettings();
                  useKbStore.getState().openWizard("rerun");
                }}
              >
                {t("kb.settings.runWizard")}
              </Button>
              {overview != null && (
                <span className="text-xs text-muted-foreground">
                  {t("kb.version", {
                    version: overview.agentkb_version,
                    base: overview.base_version,
                  })}
                </span>
              )}
            </div>
          </div>
        )}

        <DialogFooter>
          <Button variant="ghost" onClick={() => useKbStore.getState().closeSettings()}>
            {t("kb.settings.close")}
          </Button>
          <Button disabled={!dirty || !valid || saving} onClick={() => void save()}>
            {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
            {t("kb.settings.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

/** Moving the data directory is a verified copy, not a field edit. */
const MoveDataDir = ({ current }: { current: string }) => {
  const { t } = useTranslation();
  const progress = useKbStore((s) => s.progress);
  const [target, setTarget] = useState("");
  const [moving, setMoving] = useState(false);
  const { verdict, result, busy, test } = useTest(
    () => kbApi.changeDataDirCheck(target),
    (check: KbMigrateCheck) => describeMigrateCheck(check, t),
  );

  const choose = async () => {
    const picked = await pickFolder(t("kb.field.chooseFolder"));
    if (picked != null) setTarget(picked);
  };

  const run = async () => {
    if (result == null) return;
    setMoving(true);
    try {
      const useExisting = result.target_state === "existing_data";
      const outcome = await kbApi.changeDataDirRun(target, useExisting);
      if (outcome.schedule_error != null) {
        toast.warning(t("kb.move.scheduleFailed", { message: outcome.schedule_error }));
      }
      toast.success(
        useExisting
          ? t("kb.move.adopted", { path: target })
          : t("kb.move.done", { path: current }),
      );
      setTarget("");
      await useKbStore.getState().loadConfigState();
      await useKbStore.getState().refresh();
      useKbStore.getState().closeSettings();
    } catch (e) {
      toast.error(t("kb.move.failed", { message: errorText(e) }));
    } finally {
      setMoving(false);
      useKbStore.getState().setProgress(null);
    }
  };

  const canMove =
    result != null &&
    result.problem == null &&
    result.target_state !== "not_empty" &&
    !moving;

  return (
    <Field label={t("kb.move.title")} hint={t("kb.move.hint")}>
      <p className="break-all font-mono text-xs">{current}</p>
      <div className="flex gap-2">
        <Input
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          placeholder={t("kb.move.placeholder")}
          aria-label={t("kb.move.newDir")}
          className="font-mono text-xs"
        />
        <Button variant="outline" onClick={() => void choose()}>
          {t("kb.field.choose")}
        </Button>
        <Button variant="outline" disabled={target.trim() === "" || busy} onClick={() => void test()}>
          {t("kb.field.test")}
        </Button>
      </div>
      <VerdictLine verdict={verdict} busy={busy && verdict == null} />
      {canMove && (
        <Button onClick={() => void run()}>
          {result.target_state === "existing_data" ? t("kb.move.useExisting") : t("kb.move.start")}
        </Button>
      )}
      {moving && (
        <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
          <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
          {progress != null ? progressLabel(progress, t) : t("kb.move.working")}
        </p>
      )}
    </Field>
  );
};

