import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button, Input } from "@/components/ui";
import { kbApi } from "./api";
import { formatBytes } from "./format";
import { packOf, packValid } from "./logic";
import { KB_PACK_MAX_DAYS, type KbConfig, type KbPackConfig, type KbShrinkStats } from "./types";

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/**
 * Old backups stored compressed: on/off and after how many days (saved with
 * the other settings), what is compressed so far, and "compress now".
 */
export const ShrinkField = ({
  draft,
  onChange,
}: {
  draft: KbConfig;
  onChange: (next: KbConfig) => void;
}) => {
  const { t } = useTranslation();
  const pack = packOf(draft);
  const [daysText, setDaysText] = useState(String(pack.after_days));
  const [stats, setStats] = useState<KbShrinkStats | null>(null);
  const [running, setRunning] = useState(false);

  const loadStats = useCallback(() => {
    kbApi
      .shrinkStats()
      .then(setStats)
      .catch((e) => toast.error(t("kb.shrink.statsFailed", { message: errorText(e) })));
  }, [t]);

  useEffect(() => {
    loadStats();
  }, [loadStats]);

  const set = (patch: Partial<KbPackConfig>) => onChange({ ...draft, pack: { ...pack, ...patch } });

  const runNow = async () => {
    setRunning(true);
    try {
      const done = await kbApi.shrinkNow();
      const saved = formatBytes(Math.max(0, done.plain_bytes - done.packed_bytes));
      if (done.status === "skipped") {
        toast.info(t("kb.shrink.nowSkipped"));
      } else if (done.failed_files > 0) {
        toast.warning(
          t("kb.shrink.nowPartial", { files: done.packed_files, failed: done.failed_files, saved }),
        );
      } else {
        toast.success(t("kb.shrink.nowDone", { files: done.packed_files, saved }));
      }
    } catch (e) {
      toast.error(t("kb.shrink.nowFailed", { message: errorText(e) }));
    } finally {
      setRunning(false);
      loadStats();
    }
  };

  const daysOk = packValid(pack);
  const daysId = "kb-shrink-days";

  return (
    <section className="space-y-2">
      <h3 className="text-sm font-medium">{t("kb.shrink.title")}</h3>
      <p className="text-xs text-muted-foreground">{t("kb.shrink.hint")}</p>

      <label className="flex items-center gap-2 text-sm font-medium">
        <input
          type="checkbox"
          checked={pack.enabled}
          onChange={(e) => set({ enabled: e.target.checked })}
        />
        {t("kb.shrink.enable")}
      </label>

      <div className="flex items-center gap-2 text-sm">
        <label htmlFor={daysId} className="text-muted-foreground">
          {t("kb.shrink.afterDays")}
        </label>
        <Input
          id={daysId}
          type="number"
          inputMode="numeric"
          min={1}
          max={KB_PACK_MAX_DAYS}
          value={daysText}
          aria-invalid={!daysOk}
          onChange={(e) => {
            setDaysText(e.target.value);
            set({ after_days: Number(e.target.value) });
          }}
          className="w-24"
        />
        <span className="text-muted-foreground">{t("kb.shrink.daysUnit")}</span>
      </div>
      {!daysOk && (
        <p className="text-xs text-destructive" role="alert">
          {t("kb.shrink.daysInvalid", { max: KB_PACK_MAX_DAYS })}
        </p>
      )}

      {stats != null && (
        <div className="space-y-1 text-xs text-muted-foreground">
          <p>
            {stats.files === 0
              ? t("kb.shrink.none")
              : t("kb.shrink.stats", {
                  files: stats.files,
                  plain: formatBytes(stats.plain_bytes),
                  packed: formatBytes(stats.packed_bytes),
                  saved: formatBytes(Math.max(0, stats.plain_bytes - stats.packed_bytes)),
                })}
          </p>
          {stats.last_at != null && (
            <p>
              {t("kb.shrink.last", {
                at: new Date(stats.last_at).toLocaleString(),
                seconds: ((stats.last_ms ?? 0) / 1000).toFixed(1),
                files: stats.last_files ?? 0,
              })}
            </p>
          )}
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" disabled={running} onClick={() => void runNow()}>
          {running && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
          {t("kb.shrink.now")}
        </Button>
        <span className="text-xs text-muted-foreground">{t("kb.shrink.nowHint")}</span>
      </div>
    </section>
  );
};
