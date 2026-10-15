import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import { Button, Input } from "@/components/ui";
import { kbApi } from "./api";
import { describeDigestTest, formatTokens } from "./digestFormat";
import { Field, VerdictLine } from "./fields";
import { digestValid, timeValid } from "./logic";
import type { KbConfig, KbDigestConfig, KbDigestEngine } from "./types";
import { useTest } from "./useTest";

const ENGINES: KbDigestEngine[] = ["claude", "codex"];

/**
 * The daily digest's settings, shared by the settings page and the wizard.
 * Turning it on needs a passing test first: that proves the CLI is found,
 * logged in, and answers the way the digest needs, and stores its absolute
 * path (background tasks have no `PATH`).
 */
export const DigestField = ({
  draft,
  onChange,
  showEstimate = true,
}: {
  draft: KbConfig;
  onChange: (next: KbConfig) => void;
  showEstimate?: boolean;
}) => {
  const { t } = useTranslation();
  const d = draft.digest;
  const [passed, setPassed] = useState(false);
  const [daily, setDaily] = useState<number | null>(null);
  const { verdict, busy, test, reset } = useTest(
    () => kbApi.digestTest(d),
    (report) => describeDigestTest(report, t),
  );

  useEffect(() => {
    if (!showEstimate) return;
    let cancelled = false;
    Promise.resolve(kbApi.digestDailyTokens())
      .then((tokens) => !cancelled && setDaily(tokens ?? null))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [showEstimate]);

  const set = (patch: Partial<KbDigestConfig>, invalidatesTest = false) => {
    onChange({ ...draft, digest: { ...d, ...patch } });
    if (invalidatesTest) {
      setPassed(false);
      reset();
    }
  };

  const runTest = async () => {
    const report = await test();
    if (report?.ok === true) {
      setPassed(true);
      if (report.cli != null && report.cli !== d.cli_path) set({ cli_path: report.cli });
    } else {
      setPassed(false);
    }
  };

  const canEnable = passed || d.enabled;
  const timeOk = timeValid(d.time);
  const capOk = digestValid(d);

  return (
    <Field label={t("kb.digest.field.title")} hint={t("kb.digest.field.hint")}>
      <p className="rounded-md border border-warning/40 p-2 text-xs">{t("kb.digest.sharedQuota")}</p>

      <label className="flex items-center gap-2 text-sm font-medium">
        <input
          type="checkbox"
          checked={d.enabled}
          disabled={!canEnable}
          onChange={(e) => set({ enabled: e.target.checked })}
        />
        {t("kb.digest.field.enable")}
      </label>
      {!canEnable && <p className="text-xs text-muted-foreground">{t("kb.digest.field.testFirst")}</p>}

      <div role="radiogroup" aria-label={t("kb.digest.field.engine")} className="flex flex-wrap gap-4 text-sm">
        {ENGINES.map((engine) => (
          <label key={engine} className="flex items-center gap-2">
            <input
              type="radio"
              name="digest-engine"
              checked={d.engine === engine}
              onChange={() => set({ engine, cli_path: null }, true)}
            />
            {t(`kb.digest.engine.${engine}`)}
          </label>
        ))}
      </div>

      <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2 text-sm">
        <label htmlFor="digest-model" className="text-muted-foreground">
          {t("kb.digest.field.model")}
        </label>
        <Input
          id="digest-model"
          value={d.model}
          onChange={(e) => set({ model: e.target.value.trim() }, true)}
          placeholder={t("kb.digest.field.modelPlaceholder")}
          className="font-mono text-xs"
        />
        <label htmlFor="digest-time" className="text-muted-foreground">
          {t("kb.digest.field.time")}
        </label>
        <Input
          id="digest-time"
          type="time"
          value={d.time}
          aria-invalid={!timeOk}
          onChange={(e) => set({ time: e.target.value })}
          className="w-32"
        />
        <label htmlFor="digest-cap" className="text-muted-foreground">
          {t("kb.digest.field.cap")}
        </label>
        <Input
          id="digest-cap"
          type="number"
          min={1}
          max={500}
          value={d.max_sessions_per_round}
          aria-invalid={!capOk}
          onChange={(e) => set({ max_sessions_per_round: Number(e.target.value) })}
          className="w-24"
        />
      </div>
      <p className="text-xs text-muted-foreground">{t("kb.digest.field.capHint")}</p>
      {showEstimate && daily != null && (
        <p className="text-xs text-muted-foreground">
          {t("kb.digest.field.daily", { tokens: formatTokens(daily, t) })}
        </p>
      )}
      {(!timeOk || !capOk) && (
        <p role="alert" className="text-xs text-destructive">
          {t("kb.digest.field.invalid")}
        </p>
      )}

      <div className="flex flex-wrap items-center gap-3">
        <Button size="sm" variant="outline" onClick={() => void runTest()} disabled={busy}>
          {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
          {t("kb.digest.field.test")}
        </Button>
        <VerdictLine verdict={verdict} busy={busy && verdict == null} />
      </div>
      {busy && <p className="text-xs text-muted-foreground">{t("kb.digest.field.testing")}</p>}
    </Field>
  );
};
