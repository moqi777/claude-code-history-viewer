import { useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { AlertTriangle, DatabaseBackup, Loader2, RefreshCw } from "lucide-react";
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui";
import { useAppStore } from "@/store/useAppStore";
import { formatTime } from "@/utils/time";
import type { KbOverview, KbSession, KbSourceFilter } from "./types";
import { useKbStore } from "./useKbStore";

const POLL_MS = 5000;
const FILTERS: KbSourceFilter[] = ["all", "claude", "codex", "deleted"];

const formatBytes = (bytes: number): string => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
};

/** Hosted once near the app root; opened through `useKbStore.getState().open()`. */
export const KbPanel = () => {
  const { t } = useTranslation();
  const isOpen = useKbStore((s) => s.isOpen);
  const close = useKbStore((s) => s.close);
  const overview = useKbStore((s) => s.overview);
  const sessions = useKbStore((s) => s.sessions);
  const filter = useKbStore((s) => s.filter);
  const includeSubagents = useKbStore((s) => s.includeSubagents);
  const isLoading = useKbStore((s) => s.isLoading);
  const isSyncing = useKbStore((s) => s.isSyncing);
  const isSchedulePending = useKbStore((s) => s.isSchedulePending);
  const loadError = useKbStore((s) => s.loadError);

  // Keep the status fresh while the panel is open: collections also run from
  // launchd, outside this window.
  useEffect(() => {
    if (!isOpen) return;
    const timer = window.setInterval(() => {
      void useKbStore.getState().refresh();
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [isOpen]);

  const handleSync = useCallback(async () => {
    const { summary, error } = await useKbStore.getState().syncNow();
    if (error != null) {
      if (error !== "busy") toast.error(t("kb.toast.syncFailed", { message: error }));
      return;
    }
    if (summary?.status === "skipped") {
      toast.info(t("kb.toast.syncBusy"));
    } else if (summary != null && summary.errors.length > 0) {
      toast.error(t("kb.toast.syncProblems", { count: summary.errors.length }));
    } else if (summary != null && summary.changed_files > 0) {
      toast.success(
        t("kb.toast.syncDone", {
          files: summary.changed_files,
          bytes: formatBytes(summary.copied_bytes),
        }),
      );
    } else {
      toast.success(t("kb.toast.syncUpToDate"));
    }
  }, [t]);

  const handleSchedule = useCallback(async () => {
    const installed = useKbStore.getState().overview?.schedule.installed === true;
    const { error } = await useKbStore.getState().toggleSchedule();
    if (error != null) {
      toast.error(t("kb.toast.scheduleFailed", { message: error }));
    } else {
      toast.success(installed ? t("kb.toast.scheduleRemoved") : t("kb.toast.scheduleInstalled"));
    }
  }, [t]);

  const handleOpenSession = useCallback(
    async (session: KbSession) => {
      try {
        const app = useAppStore.getState();
        app.setAnalyticsCurrentView("messages");
        await app.selectSession(session);
        useKbStore.getState().close();
      } catch (e) {
        toast.error(
          t("kb.toast.openFailed", { message: e instanceof Error ? e.message : String(e) }),
        );
      }
    },
    [t],
  );

  const collecting = isSyncing || overview?.running === true;

  return (
    <Dialog open={isOpen} onOpenChange={(open) => !open && close()}>
      <DialogContent className="max-w-3xl max-h-[85vh] flex flex-col gap-4">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <DatabaseBackup className="h-5 w-5" aria-hidden="true" />
            {t("kb.title")}
          </DialogTitle>
          <DialogDescription>{t("kb.description")}</DialogDescription>
        </DialogHeader>

        {loadError != null && (
          <div role="alert" className="flex items-center gap-2 text-sm text-destructive">
            <AlertTriangle className="h-4 w-4" aria-hidden="true" />
            {t("kb.error.loadFailed", { message: loadError })}
          </div>
        )}

        {overview != null && <StatusSection overview={overview} />}

        <div className="flex flex-wrap items-center gap-2">
          <Button onClick={() => void handleSync()} disabled={collecting}>
            {collecting ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />
            ) : (
              <RefreshCw className="mr-2 h-4 w-4" aria-hidden="true" />
            )}
            {collecting ? t("kb.action.syncing") : t("kb.action.syncNow")}
          </Button>
          {overview?.schedule.supported === true && (
            <Button
              variant="outline"
              onClick={() => void handleSchedule()}
              disabled={isSchedulePending}
            >
              {overview.schedule.installed
                ? t("kb.action.removeSchedule")
                : t("kb.action.installSchedule")}
            </Button>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2" role="group" aria-label={t("kb.sessions.filterLabel")}>
          {FILTERS.map((value) => (
            <Button
              key={value}
              size="sm"
              variant={filter === value ? "default" : "outline"}
              aria-pressed={filter === value}
              onClick={() => void useKbStore.getState().setFilter(value)}
            >
              {t(`kb.sessions.filter.${value}`)}
            </Button>
          ))}
          <label className="ml-auto flex items-center gap-2 text-sm text-muted-foreground">
            <input
              type="checkbox"
              checked={includeSubagents}
              onChange={(e) => void useKbStore.getState().setIncludeSubagents(e.target.checked)}
            />
            {t("kb.sessions.includeSubagents")}
          </label>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto rounded-md border" aria-busy={isLoading}>
          {sessions.length === 0 ? (
            <p className="p-6 text-center text-sm text-muted-foreground">
              {t("kb.sessions.empty")}
            </p>
          ) : (
            <ul className="divide-y">
              {sessions.map((session) => (
                <SessionRow
                  key={session.file_path}
                  session={session}
                  onOpen={() => void handleOpenSession(session)}
                />
              ))}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
};

const StatusSection = ({ overview }: { overview: KbOverview }) => {
  const { t } = useTranslation();
  const { schedule, last_run: lastRun } = overview;

  let scheduleText: string;
  if (!schedule.supported) scheduleText = t("kb.status.schedule.unsupported");
  else if (schedule.loaded)
    scheduleText = t("kb.status.schedule.active", {
      minutes: Math.max(1, Math.round((schedule.interval_secs ?? overview.interval_secs) / 60)),
    });
  else if (schedule.installed) scheduleText = t("kb.status.schedule.installedNotLoaded");
  else scheduleText = t("kb.status.schedule.notInstalled");

  let lastRunText = t("kb.status.lastRun.never");
  if (lastRun != null) {
    const time = formatTime(lastRun.finished_at ?? lastRun.started_at);
    const problems = lastRun.summary?.errors.length ?? 0;
    lastRunText =
      lastRun.finished_at == null
        ? t("kb.status.lastRun.running", { time })
        : problems > 0
          ? t("kb.status.lastRun.problems", { time, count: problems })
          : t("kb.status.lastRun.ok", { time, files: lastRun.summary?.changed_files ?? 0 });
  }

  const rows: [string, string][] = [
    [t("kb.status.dataDir"), overview.data_dir],
    [t("kb.status.schedule"), scheduleText],
    [t("kb.status.lastRun"), lastRunText],
    [
      t("kb.status.sessions"),
      t("kb.status.sessionsValue", {
        claude: overview.sessions_by_source.claude ?? 0,
        codex: overview.sessions_by_source.codex ?? 0,
      }),
    ],
    [t("kb.status.deleted"), String(overview.deleted_sources)],
  ];

  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
      {rows.map(([label, value]) => (
        <div key={label} className="contents">
          <dt className="text-muted-foreground">{label}</dt>
          <dd className="break-all">{value}</dd>
        </div>
      ))}
    </dl>
  );
};

const SessionRow = ({ session, onOpen }: { session: KbSession; onOpen: () => void }) => {
  const { t } = useTranslation();
  const title = session.summary ?? t("kb.sessions.untitled");
  const deleted = session.source_status === "deleted";
  return (
    <li>
      <button
        type="button"
        onClick={onOpen}
        aria-label={t("kb.sessions.open", { title })}
        className="flex w-full flex-col gap-1 px-4 py-3 text-left hover:bg-muted focus-visible:bg-muted focus-visible:outline-none"
      >
        <span className="flex items-center gap-2">
          <span className="truncate font-medium">{title}</span>
          {deleted && (
            <Badge variant="destructive" title={t("kb.sessions.deletedHint")}>
              {t("kb.sessions.deletedBadge")}
            </Badge>
          )}
        </span>
        <span className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
          <Badge variant="outline">{session.provider}</Badge>
          {session.entrypoint != null && <span>{session.entrypoint}</span>}
          <span>{session.project_name || "—"}</span>
          <span>{t("kb.sessions.messages", { count: session.message_count })}</span>
          <span>{formatTime(session.last_modified)}</span>
        </span>
      </button>
    </li>
  );
};
