import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui";
import { isTauri } from "@/utils/platform";
import { kbApi } from "./api";
import { Field, VerdictLine } from "./fields";
import { formatBytes } from "./format";
import { pickFolder } from "./pickFolder";
import type { KbDownloadProgress, KbSemanticStatus, KbSemanticTest } from "./types";

const POLL_MS = 3000;

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/**
 * The settings group for semantic search: download the model, test it, switch
 * it on, watch the index fill up. Unlike the other groups it does not edit the
 * draft: each button is a command of its own and takes effect at once (a
 * model download is not something to "save later").
 */
export const SemanticField = () => {
  const { t } = useTranslation();
  const [status, setStatus] = useState<KbSemanticStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<KbDownloadProgress | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<KbSemanticTest | null>(null);
  const [busy, setBusy] = useState(false);
  const mounted = useRef(true);

  const reload = useCallback(async () => {
    try {
      const next = await kbApi.semanticStatus();
      if (mounted.current) setStatus(next);
    } catch (e) {
      if (mounted.current) setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void reload();
    return () => {
      mounted.current = false;
    };
  }, [reload]);

  // Follow the builder while it works.
  const active = status?.index.building === true || downloading;
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => void reload(), POLL_MS);
    return () => clearInterval(timer);
  }, [active, reload]);

  // Download progress comes as events.
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<KbDownloadProgress>("kb-semantic-download", (e) => setProgress(e.payload)),
      )
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      void reload();
    }
  };

  const download = async () => {
    setDownloading(true);
    setProgress(null);
    setError(null);
    try {
      await kbApi.semanticDownload();
      setTest(null);
    } catch (e) {
      const message = errorText(e);
      if (message !== "cancelled") setError(t("kb.semantic.downloadFailed", { message }));
    } finally {
      setDownloading(false);
      setProgress(null);
      void reload();
    }
  };

  const runTest = async () => {
    setTesting(true);
    setTest(null);
    try {
      setTest(await kbApi.semanticTest());
    } catch (e) {
      setTest({ ok: false, seconds: 0, dim: 0, message: errorText(e) });
    } finally {
      setTesting(false);
    }
  };

  const chooseFolder = async () => {
    const dir = await pickFolder(t("kb.semantic.pickFolder"));
    if (dir == null) return;
    await run(() => kbApi.semanticImportFolder(dir));
  };

  if (status == null) {
    return (
      <Field label={t("kb.semantic.title")} hint={t("kb.semantic.hint")}>
        {error == null ? (
          <p className="text-sm text-muted-foreground">{t("kb.wizard.loading")}</p>
        ) : (
          <VerdictLine verdict={{ tone: "error", text: error }} />
        )}
      </Field>
    );
  }

  const ready = status.model.state === "ready";
  const idx = status.index;
  const percent = idx.total > 0 ? Math.min(100, Math.floor((idx.indexed * 100) / idx.total)) : 0;
  const canEnable = ready && (test?.ok === true || status.enabled);
  const totalBytes = status.model_bytes;

  return (
    <Field label={t("kb.semantic.title")} hint={t("kb.semantic.hint")}>
      <p className="rounded-md border p-2 text-xs">{t("kb.semantic.offline")}</p>

      <label className="flex items-center gap-2 text-sm font-medium">
        <input
          type="checkbox"
          checked={status.enabled}
          disabled={busy || (!status.enabled && !canEnable)}
          onChange={(e) => void run(() => kbApi.semanticSet({ enabled: e.target.checked }))}
        />
        {t("kb.semantic.enable")}
      </label>
      {!status.enabled && !canEnable && (
        <p className="text-xs text-muted-foreground">{t("kb.semantic.testFirst")}</p>
      )}

      <div className="space-y-2 rounded-md border p-3">
        <p className="text-sm font-medium">
          {t("kb.semantic.model", { name: status.model_name, size: formatBytes(totalBytes) })}
        </p>

        {status.model.state === "ready" && (
          <>
            <VerdictLine verdict={{ tone: "ok", text: t("kb.semantic.modelReady") }} />
            <p className="break-all font-mono text-xs text-muted-foreground">{status.models_dir}</p>
            <Button
              variant="outline"
              size="sm"
              disabled={busy || idx.building}
              onClick={() => void run(() => kbApi.semanticDeleteModel())}
            >
              {t("kb.semantic.deleteModel")}
            </Button>
          </>
        )}

        {status.model.state === "corrupt" && (
          <VerdictLine
            verdict={{
              tone: "error",
              text: t("kb.semantic.modelCorrupt", {
                file: status.model.file,
                reason: status.model.reason,
              }),
            }}
          />
        )}

        {status.model.state !== "ready" && (
          <>
            <fieldset className="space-y-1 text-sm" disabled={downloading}>
              <legend className="text-xs text-muted-foreground">{t("kb.semantic.source")}</legend>
              {status.sources.map((src) => (
                <label key={src.id} className="flex items-center gap-2">
                  <input
                    type="radio"
                    name="semantic-source"
                    checked={status.source === src.id}
                    onChange={() => void run(() => kbApi.semanticSet({ source: src.id }))}
                  />
                  {t(`kb.semantic.sources.${src.id}`)}
                  <span className="font-mono text-xs text-muted-foreground">{src.host}</span>
                </label>
              ))}
            </fieldset>
            <p className="text-xs text-muted-foreground">
              {t("kb.semantic.downloadNote", { size: formatBytes(totalBytes) })}
            </p>
            <div className="flex flex-wrap items-center gap-2">
              <Button disabled={downloading || busy} onClick={() => void download()}>
                {downloading && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
                {downloading ? t("kb.semantic.downloading") : t("kb.semantic.download")}
              </Button>
              {downloading && (
                <Button variant="ghost" onClick={() => void kbApi.semanticDownloadCancel()}>
                  {t("kb.semantic.cancel")}
                </Button>
              )}
              <Button variant="outline" disabled={downloading || busy} onClick={() => void chooseFolder()}>
                {t("kb.semantic.fromFolder")}
              </Button>
            </div>
            {downloading && progress != null && (
              <p className="text-sm text-muted-foreground" role="status">
                {t("kb.semantic.progress", {
                  done: formatBytes(progress.done_bytes),
                  total: formatBytes(progress.total_bytes),
                })}
              </p>
            )}
          </>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" disabled={!ready || testing || idx.building} onClick={() => void runTest()}>
          {testing && <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />}
          {t("kb.semantic.test")}
        </Button>
      </div>
      {test != null && (
        <VerdictLine
          verdict={
            test.ok
              ? {
                  tone: "ok",
                  text: t("kb.semantic.testOk", {
                    seconds: test.seconds.toFixed(1),
                    dim: test.dim,
                  }),
                }
              : { tone: "error", text: t("kb.semantic.testFailed", { message: test.message }) }
          }
        />
      )}

      {ready && (
        <div className="space-y-2 rounded-md border p-3">
          <p className="text-sm font-medium">{t("kb.semantic.indexTitle")}</p>
          <p className="text-sm" role="status">
            {idx.total === 0
              ? t("kb.semantic.indexNone")
              : t("kb.semantic.indexProgress", {
                  done: idx.indexed.toLocaleString(),
                  total: idx.total.toLocaleString(),
                  percent,
                })}
          </p>
          {idx.total > 0 && (
            <div
              className="h-2 overflow-hidden rounded bg-muted"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={percent}
              aria-label={t("kb.semantic.indexTitle")}
            >
              <div className="h-full bg-primary" style={{ width: `${percent}%` }} />
            </div>
          )}
          {idx.building && (
            <p className="text-xs text-muted-foreground">{t("kb.semantic.building")}</p>
          )}
          {!idx.building && idx.status === "postponed_memory" && (
            <p className="text-xs text-warning" role="note">
              {t("kb.semantic.postponed", { count: idx.postponed_rounds })}
            </p>
          )}
          {!idx.building && idx.status === "paused_model" && idx.message !== "" && (
            <p className="text-xs text-warning" role="note">
              {t("kb.semantic.paused", { message: idx.message })}
            </p>
          )}
          {!idx.building && idx.status === "error" && (
            <VerdictLine verdict={{ tone: "error", text: idx.message }} />
          )}
          <p className="text-xs text-muted-foreground">
            {t("kb.semantic.indexHint", { size: formatBytes(status.vectors_bytes) })}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={busy || idx.building}
              onClick={() => void run(() => kbApi.semanticBuild(false))}
            >
              {t("kb.semantic.buildBatch")}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={busy || idx.building}
              onClick={() => void run(() => kbApi.semanticBuild(true))}
            >
              {t("kb.semantic.buildAll")}
            </Button>
            {idx.building && (
              <Button variant="ghost" size="sm" onClick={() => void run(() => kbApi.semanticBuildCancel())}>
                {t("kb.semantic.stopBuild")}
              </Button>
            )}
            <Button
              variant="ghost"
              size="sm"
              disabled={busy || idx.building}
              onClick={() => void run(() => kbApi.semanticRebuild())}
            >
              {t("kb.semantic.rebuild")}
            </Button>
          </div>
        </div>
      )}

      {error != null && <VerdictLine verdict={{ tone: "error", text: error }} />}
    </Field>
  );
};
