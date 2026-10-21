import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, CheckCircle2, Loader2, XCircle } from "lucide-react";
import { Button, Input } from "@/components/ui";
import { cn } from "@/lib/utils";
import { kbApi } from "./api";
import { formatBytes } from "./format";
import {
  describeDataDir,
  describeMirror,
  describeScheduleEnv,
  describeSource,
  type Verdict,
} from "./messages";
import { pickFolder } from "./pickFolder";
import { useTest } from "./useTest";
import type {
  KbConfig,
  KbDataDirReport,
  KbMirrorConfig,
  KbMirrorReport,
  KbScheduleEnvReport,
  KbSourceProbe,
  KbVolume,
} from "./types";
import { shortVolumeId } from "./volumeLabel";

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export const VerdictLine = ({ verdict, busy }: { verdict: Verdict | null; busy?: boolean }) => {
  if (busy) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
        <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
      </p>
    );
  }
  if (verdict == null) return null;
  const Icon = verdict.tone === "ok" ? CheckCircle2 : verdict.tone === "warn" ? AlertTriangle : XCircle;
  return (
    <p
      role={verdict.tone === "error" ? "alert" : "status"}
      className={cn(
        "flex items-start gap-2 text-sm",
        verdict.tone === "ok" && "text-success",
        verdict.tone === "warn" && "text-warning",
        verdict.tone === "error" && "text-destructive",
      )}
    >
      <Icon className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
      <span className="break-words">{verdict.text}</span>
    </p>
  );
};

const Field = ({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) => (
  <section className="space-y-2">
    <h3 className="text-sm font-medium">{label}</h3>
    {hint != null && <p className="text-xs text-muted-foreground">{hint}</p>}
    {children}
  </section>
);

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

export const SourcesField = ({
  draft,
  onChange,
}: {
  draft: KbConfig;
  onChange: (next: KbConfig) => void;
}) => {
  const { t } = useTranslation();
  const [probes, setProbes] = useState<KbSourceProbe[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const detect = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setProbes(await kbApi.probeSources(draft));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }, [draft]);

  // Detect once on first show.
  const detectedOnce = useRef(false);
  useEffect(() => {
    if (detectedOnce.current) return;
    detectedOnce.current = true;
    void detect();
  }, [detect]);

  const toggle = (source: "claude" | "codex", enabled: boolean) =>
    onChange({
      ...draft,
      sources: { ...draft.sources, [source]: { ...draft.sources[source], enabled } },
    });

  return (
    <Field label={t("kb.field.sources")} hint={t("kb.field.sourcesHint")}>
      <ul className="space-y-2">
        {(["claude", "codex"] as const).map((source) => {
          const probe = probes?.find((p) => p.source === source);
          return (
            <li key={source} className="rounded-md border p-3">
              <label className="flex items-center gap-2 text-sm font-medium">
                <input
                  type="checkbox"
                  checked={draft.sources[source].enabled}
                  onChange={(e) => toggle(source, e.target.checked)}
                />
                {t(`kb.sessions.filter.${source}`)}
              </label>
              <div className="mt-1 pl-6">
                {probe != null && <VerdictLine verdict={describeSource(probe, t)} />}
                {probe?.root != null && (
                  <p className="break-all text-xs text-muted-foreground">{probe.root}</p>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {error != null && <VerdictLine verdict={{ tone: "error", text: t("kb.test.failed", { message: error }) }} />}
      <Button size="sm" variant="outline" onClick={() => void detect()} disabled={busy}>
        {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
        {t("kb.field.redetect")}
      </Button>
    </Field>
  );
};

// ---------------------------------------------------------------------------
// Data directory
// ---------------------------------------------------------------------------

export const DataDirField = ({
  value,
  onChange,
  locked,
  onReport,
  children,
}: {
  value: string;
  onChange: (value: string) => void;
  /** Changing the directory of a configured app goes through "Move data directory". */
  locked?: boolean;
  onReport?: (report: KbDataDirReport | null) => void;
  children?: React.ReactNode;
}) => {
  const { t } = useTranslation();
  const { verdict, result, busy, test, reset } = useTest(
    () => kbApi.testDataDir(value),
    (r: KbDataDirReport) => describeDataDir(r, t),
  );

  useEffect(() => {
    onReport?.(result);
  }, [result, onReport]);

  // Test straight away (and again when the path changes): the verdict line
  // already announces existing data without the user having to ask.
  const lastTested = useRef<string | null>(null);
  useEffect(() => {
    if (lastTested.current === value) return;
    lastTested.current = value;
    reset();
    const timer = window.setTimeout(() => void test(), 300);
    return () => {
      window.clearTimeout(timer);
      // A cancelled timer must not count as "already tested" (StrictMode remount).
      lastTested.current = null;
    };
  }, [value, test, reset]);

  const choose = async () => {
    const picked = await pickFolder(t("kb.field.chooseFolder"));
    if (picked != null) onChange(picked);
  };

  return (
    <Field label={t("kb.field.dataDir")} hint={t("kb.field.dataDirHint")}>
      <div className="flex gap-2">
        <Input
          value={value}
          readOnly={locked}
          onChange={(e) => onChange(e.target.value)}
          aria-label={t("kb.field.dataDir")}
          className="font-mono text-xs"
        />
        {!locked && (
          <Button variant="outline" onClick={() => void choose()}>
            {t("kb.field.choose")}
          </Button>
        )}
        <Button variant="outline" onClick={() => void test()} disabled={busy}>
          {t("kb.field.test")}
        </Button>
      </div>
      <VerdictLine verdict={verdict} busy={busy && verdict == null} />
      {children}
    </Field>
  );
};

// ---------------------------------------------------------------------------
// Interval + schedule
// ---------------------------------------------------------------------------

const PRESET_MINUTES = [5, 10, 15] as const;

export const IntervalField = ({
  draft,
  onChange,
  showTest = true,
}: {
  draft: KbConfig;
  onChange: (next: KbConfig) => void;
  showTest?: boolean;
}) => {
  const { t } = useTranslation();
  const minutes = draft.interval_secs / 60;
  const isPreset = (PRESET_MINUTES as readonly number[]).includes(minutes);
  // Derived, not copied from the first render: the draft may still be loading
  // when this mounts, and 5/10/15 must always select their own button.
  const [customChosen, setCustom] = useState(false);
  const custom = customChosen || !isPreset;
  const [customText, setCustomText] = useState(String(Math.round(minutes)));
  const { verdict, busy, test } = useTest(
    () => kbApi.testScheduleEnv(),
    (r: KbScheduleEnvReport) => describeScheduleEnv(r, t),
  );

  const setMinutes = (m: number) => onChange({ ...draft, interval_secs: Math.round(m * 60) });
  const customValid = /^\d+$/.test(customText) && Number(customText) >= 1 && Number(customText) <= 1440;

  return (
    <Field label={t("kb.field.interval")} hint={t("kb.field.intervalHint")}>
      <div className="flex flex-wrap items-center gap-2" role="group" aria-label={t("kb.field.interval")}>
        {PRESET_MINUTES.map((m) => (
          <Button
            key={m}
            size="sm"
            variant={!custom && minutes === m ? "default" : "outline"}
            aria-pressed={!custom && minutes === m}
            onClick={() => {
              setCustom(false);
              setMinutes(m);
            }}
          >
            {t("kb.field.minutes", { count: m })}
          </Button>
        ))}
        <Button
          size="sm"
          variant={custom ? "default" : "outline"}
          aria-pressed={custom}
          onClick={() => setCustom(true)}
        >
          {t("kb.field.custom")}
        </Button>
        {custom && (
          <label className="flex items-center gap-2 text-sm">
            <Input
              value={customText}
              inputMode="numeric"
              className="h-8 w-20"
              aria-label={t("kb.field.customMinutes")}
              aria-invalid={!customValid}
              onChange={(e) => {
                setCustomText(e.target.value);
                if (/^\d+$/.test(e.target.value) && Number(e.target.value) >= 1) {
                  setMinutes(Math.min(Number(e.target.value), 1440));
                }
              }}
            />
            {t("kb.field.minutesUnit")}
          </label>
        )}
      </div>
      {custom && !customValid && (
        <p className="text-xs text-destructive" role="alert">
          {t("kb.field.intervalInvalid")}
        </p>
      )}
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={draft.schedule_enabled}
          onChange={(e) => onChange({ ...draft, schedule_enabled: e.target.checked })}
        />
        {t("kb.field.scheduleEnabled")}
      </label>
      {showTest && (
        <>
          <Button size="sm" variant="outline" onClick={() => void test()} disabled={busy}>
            {t("kb.field.testSchedule")}
          </Button>
          <VerdictLine verdict={verdict} busy={busy && verdict == null} />
        </>
      )}
    </Field>
  );
};

// ---------------------------------------------------------------------------
// External disk
// ---------------------------------------------------------------------------

const DEFAULT_BACKUP_FOLDER = "AgentKB-backup";

export const MirrorField = ({
  mirror,
  onChange,
  onTested,
}: {
  mirror: KbMirrorConfig;
  onChange: (next: KbMirrorConfig) => void;
  /** `true` when the current choice has just passed its test. */
  onTested?: (ok: boolean) => void;
}) => {
  const { t } = useTranslation();
  const [volumes, setVolumes] = useState<KbVolume[]>([]);
  const [listError, setListError] = useState<string | null>(null);

  const target = mirror.target_dir ?? "";
  const { verdict, result, busy, test, reset } = useTest(
    () => kbApi.testMirror(target, mirror.volume_uuid),
    (r: KbMirrorReport) => describeMirror(r, t),
  );

  const loadVolumes = useCallback(async () => {
    try {
      setVolumes(await kbApi.listExternalVolumes());
      setListError(null);
    } catch (e) {
      setListError(errorText(e));
    }
  }, []);

  useEffect(() => {
    if (mirror.enabled) void loadVolumes();
  }, [mirror.enabled, loadVolumes]);

  useEffect(() => {
    onTested?.(result?.state === "connected");
  }, [result, onTested]);

  const chooseVolume = (volume: KbVolume) => {
    reset();
    onChange({
      ...mirror,
      target_dir: `${volume.mount_point}/${DEFAULT_BACKUP_FOLDER}`,
      volume_uuid: volume.uuid,
      volume_name: volume.name,
    });
  };

  const chooseFolder = async () => {
    const picked = await pickFolder(t("kb.field.chooseFolder"));
    if (picked == null) return;
    reset();
    // The UUID comes from the disk that really holds the folder.
    try {
      const report = await kbApi.testMirror(picked, null);
      if (report.state === "connected" || report.state === "not_writable") {
        onChange({
          ...mirror,
          target_dir: picked,
          volume_uuid: report.volume.uuid,
          volume_name: report.volume.name,
        });
      } else {
        onChange({ ...mirror, target_dir: picked, volume_uuid: null, volume_name: null });
      }
    } catch (e) {
      onChange({ ...mirror, target_dir: picked, volume_uuid: null, volume_name: null });
      setListError(errorText(e));
    }
  };

  return (
    <Field label={t("kb.field.mirror")} hint={t("kb.field.mirrorHint")}>
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={mirror.enabled}
          onChange={(e) => {
            reset();
            onChange({ ...mirror, enabled: e.target.checked });
          }}
        />
        {t("kb.field.mirrorEnabled")}
      </label>
      {mirror.enabled && (
        <div className="space-y-2 rounded-md border p-3">
          {volumes.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("kb.field.noVolumes")}</p>
          ) : (
            <ul className="space-y-1" aria-label={t("kb.field.volumes")}>
              {volumes.map((v) => (
                <li key={v.uuid}>
                  <Button
                    size="sm"
                    variant={mirror.volume_uuid === v.uuid ? "default" : "outline"}
                    aria-pressed={mirror.volume_uuid === v.uuid}
                    onClick={() => chooseVolume(v)}
                  >
                    {v.name ?? v.mount_point} · {shortVolumeId(v.uuid)}
                  </Button>
                </li>
              ))}
            </ul>
          )}
          {listError != null && <VerdictLine verdict={{ tone: "error", text: listError }} />}
          <div className="flex gap-2">
            <Button size="sm" variant="outline" onClick={() => void loadVolumes()}>
              {t("kb.field.refreshVolumes")}
            </Button>
            <Button size="sm" variant="outline" onClick={() => void chooseFolder()}>
              {t("kb.field.chooseFolder")}
            </Button>
          </div>
          <Input
            value={target}
            readOnly
            aria-label={t("kb.field.mirrorFolder")}
            placeholder={t("kb.field.mirrorFolderPlaceholder")}
            className="font-mono text-xs"
          />
          <Button
            size="sm"
            variant="outline"
            onClick={() => void test()}
            disabled={busy || target === ""}
          >
            {t("kb.field.test")}
          </Button>
          <VerdictLine verdict={verdict} busy={busy && verdict == null} />
          {result?.state === "connected" && result.existing_bytes > 0 && (
            <p className="text-xs text-muted-foreground">
              {t("kb.field.mirrorExisting", { size: formatBytes(result.existing_bytes) })}
            </p>
          )}
        </div>
      )}
    </Field>
  );
};

export { Field };
