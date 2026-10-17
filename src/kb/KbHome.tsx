import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  AlertTriangle,
  ChevronRight,
  DatabaseBackup,
  HardDriveDownload,
  Loader2,
  RefreshCw,
  Settings,
  SlidersHorizontal,
  X,
} from "lucide-react";
import { Badge, Button } from "@/components/ui";
import { cn } from "@/lib/utils";
import { formatTime } from "@/utils/time";
import { formatBytes } from "./format";
import { describeBackup, scheduleHealth } from "./homeStatus";
import { VerdictLine } from "./fields";
import { openKbSession } from "./openSession";
import { digestSummary, formatShortTime } from "./digestFormat";
import { KbDigestCard } from "./KbDigestCard";
import { KbNotes } from "./KbNotes";
import { SearchBox, SearchResults } from "./KbSearch";
import { percentOf, progressLabel } from "./progressLabel";
import type { KbFilters, KbOverview, KbSession, KbTimeRange } from "./types";
import { defaultFilters, useKbStore } from "./useKbStore";

const POLL_BUSY_MS = 2000;
const POLL_IDLE_MS = 5000;
const PRESENCE_MS = 15000;
const POLL_DIGEST_IDLE_MS = 10000;
const RANGES: KbTimeRange[] = ["all", "7d", "30d"];

export const KbHome = () => {
  const { t } = useTranslation();
  const overview = useKbStore((s) => s.overview);
  const sessions = useKbStore((s) => s.sessions);
  const facets = useKbStore((s) => s.facets);
  const presence = useKbStore((s) => s.presence);
  const configState = useKbStore((s) => s.configState);
  const filters = useKbStore((s) => s.filters);
  const progress = useKbStore((s) => s.progress);
  const isLoading = useKbStore((s) => s.isLoading);
  const isSyncing = useKbStore((s) => s.isSyncing);
  const isBackingUp = useKbStore((s) => s.isBackingUp);
  const isRepairing = useKbStore((s) => s.isRepairing);
  const loadError = useKbStore((s) => s.loadError);
  const searching = useKbStore((s) => s.search.status !== "idle");
  const homeTab = useKbStore((s) => s.homeTab);
  const digestRunning = useKbStore((s) => s.digest?.running != null);

  const activity = overview?.activity ?? "idle";
  const busy = isSyncing || isBackingUp || overview?.running === true;

  useEffect(() => {
    const store = useKbStore.getState();
    void store.loadConfigState();
    void store.refresh();
    void store.loadFacets();
    void store.refreshPresence();
    void store.loadSkills();
    void store.refreshDigest();
  }, []);

  // Collections also run from launchd, outside this window: keep the page fresh.
  useEffect(() => {
    const timer = window.setInterval(
      () => void useKbStore.getState().refresh(),
      busy ? POLL_BUSY_MS : POLL_IDLE_MS,
    );
    return () => window.clearInterval(timer);
  }, [busy]);

  // The digest runs in its own process (launchd, or a button): follow it closely while it works.
  useEffect(() => {
    const timer = window.setInterval(
      () => void useKbStore.getState().refreshDigest(),
      digestRunning ? POLL_BUSY_MS : POLL_DIGEST_IDLE_MS,
    );
    return () => window.clearInterval(timer);
  }, [digestRunning]);

  useEffect(() => {
    const timer = window.setInterval(() => void useKbStore.getState().refreshPresence(), PRESENCE_MS);
    return () => window.clearInterval(timer);
  }, []);

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

  const handleBackup = useCallback(async () => {
    const { summary, error } = await useKbStore.getState().backupNow();
    if (error != null) {
      if (error !== "busy") toast.error(t("kb.toast.backupFailed", { message: error }));
      return;
    }
    if (summary == null) return;
    if (summary.status === "skipped") {
      toast.info(summary.reason === "locked" ? t("kb.toast.syncBusy") : t("kb.toast.backupSkipped"));
    } else if (summary.status === "error") {
      toast.error(t("kb.toast.backupFailed", { message: summary.errors[0]?.message ?? "" }));
    } else if (summary.copied_files + summary.appended_files > 0) {
      toast.success(
        t("kb.toast.backupDone", {
          files: summary.copied_files + summary.appended_files,
          bytes: formatBytes(summary.copied_bytes),
        }),
      );
    } else {
      toast.success(t("kb.toast.backupUpToDate"));
    }
  }, [t]);

  const handleRepair = useCallback(async () => {
    const { error } = await useKbStore.getState().repairSchedule();
    if (error != null) toast.error(t("kb.toast.repairFailed", { message: error }));
    else toast.success(t("kb.toast.repaired"));
  }, [t]);

  const handleOpenSession = useCallback(
    async (session: KbSession) => {
      try {
        await openKbSession(session);
      } catch (e) {
        toast.error(
          t("kb.toast.openFailed", { message: e instanceof Error ? e.message : String(e) }),
        );
      }
    },
    [t],
  );

  const mirrorOn = overview?.mirror.enabled === true;
  const diskUp = presence?.state === "connected";
  const percent =
    overview?.backup.progress != null
      ? percentOf(overview.backup.progress[0], overview.backup.progress[1])
      : null;

  let syncTitle: string | undefined;
  if (activity === "backing_up") syncTitle = t("kb.action.syncDisabledDuringBackup");
  let backupTitle: string | undefined;
  if (!diskUp) backupTitle = t("kb.action.backupNeedsDisk");
  else if (busy) backupTitle = t("kb.action.backupBusy");

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto px-4 pb-4 md:px-6 md:pb-6" aria-busy={isLoading}>
      <header className="flex flex-wrap items-start gap-3 pt-4 md:pt-6">
        <div className="min-w-0 flex-1">
          <h2 className="flex flex-wrap items-baseline gap-x-3 text-lg font-semibold">
            <span className="flex items-center gap-2">
              <DatabaseBackup className="h-5 w-5 self-center" aria-hidden="true" />
              {t("kb.title")}
            </span>
            {overview != null && (
              <span className="text-xs font-normal text-muted-foreground">
                {t("kb.version", { version: overview.agentkb_version, base: overview.base_version })}
              </span>
            )}
          </h2>
          <p className="text-sm text-muted-foreground [text-wrap:balance]">{t("kb.description")}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button onClick={() => void handleSync()} disabled={busy} title={syncTitle}>
            {activity === "collecting" || isSyncing ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />
            ) : (
              <RefreshCw className="mr-2 h-4 w-4" aria-hidden="true" />
            )}
            {activity === "collecting" || isSyncing ? t("kb.action.syncing") : t("kb.action.syncNow")}
          </Button>
          {mirrorOn && (
            <Button
              variant="outline"
              onClick={() => void handleBackup()}
              disabled={busy || !diskUp}
              title={backupTitle}
            >
              {activity === "backing_up" || isBackingUp ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" aria-hidden="true" />
              ) : (
                <HardDriveDownload className="mr-2 h-4 w-4" aria-hidden="true" />
              )}
              {activity === "backing_up" || isBackingUp
                ? t("kb.action.backingUp")
                : t("kb.action.backupNow")}
            </Button>
          )}
          <Button variant="outline" onClick={() => useKbStore.getState().openSettings()}>
            <Settings className="mr-2 h-4 w-4" aria-hidden="true" />
            {t("kb.action.settings")}
          </Button>
        </div>
      </header>

      {loadError != null && (
        <div role="alert" className="flex items-center gap-2 text-sm text-destructive">
          <AlertTriangle className="h-4 w-4" aria-hidden="true" />
          {t("kb.error.loadFailed", { message: loadError })}
        </div>
      )}

      <Banners />

      {busy && (
        <div role="status" aria-live="polite" className="rounded-md border p-3 text-sm">
          <p className="flex items-center gap-2">
            <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
            {activity === "backing_up"
              ? t("kb.status.activity.backingUp", { percent: percent ?? 0 })
              : t("kb.status.activity.collecting")}
          </p>
          {progress != null && (
            <p className="mt-1 text-xs text-muted-foreground">{progressLabel(progress, t)}</p>
          )}
          {activity === "backing_up" && percent != null && (
            <div
              className="mt-2 h-1.5 overflow-hidden rounded bg-muted"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={percent}
            >
              <div className="h-full bg-accent" style={{ width: `${percent}%` }} />
            </div>
          )}
        </div>
      )}

      {overview != null && (
        <Summary
          overview={overview}
          onRepair={() => void handleRepair()}
          repairing={isRepairing}
        />
      )}

      {/* Stays at the top while the list scrolls under it. */}
      <div className="sticky top-0 z-10 -mx-4 space-y-2 bg-background px-4 pt-1 md:-mx-6 md:px-6">
      <div role="tablist" aria-label={t("kb.tabs.label")} className="flex gap-1 border-b">
        {(["sessions", "notes"] as const).map((tab) => (
          <button
            key={tab}
            type="button"
            role="tab"
            aria-selected={homeTab === tab}
            onClick={() => useKbStore.getState().setHomeTab(tab)}
            className={cn(
              "-mb-px border-b-2 px-3 py-1.5 text-sm",
              homeTab === tab
                ? "border-accent font-medium"
                : "border-transparent text-muted-foreground hover:text-foreground",
            )}
          >
            {t(`kb.tabs.${tab}`)}
          </button>
        ))}
      </div>

      {homeTab === "sessions" && (
        <div className="space-y-2 pb-2">
          <SearchBox />
          <Filters filters={filters} facets={facets} />
        </div>
      )}
      </div>

      {homeTab === "notes" ? (
        <KbNotes />
      ) : (
        <>
      <div className="rounded-md border" aria-busy={isLoading}>
        {searching ? (
          <SearchResults />
        ) : sessions.length === 0 ? (
          <p className="p-6 text-center text-sm text-muted-foreground">
            {configState?.state === "missing"
              ? t("kb.sessions.emptyNoSetup")
              : t("kb.sessions.empty")}
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
      {!searching && sessions.length >= 500 && (
        <p className="text-xs text-muted-foreground">{t("kb.sessions.limitNote", { count: 500 })}</p>
      )}
        </>
      )}
    </div>
  );
};

const SKILL_OFFER_KEY = "agentkb.skillOffer.dismissed";

const offerDismissed = (): boolean => {
  try {
    return window.localStorage.getItem(SKILL_OFFER_KEY) === "1";
  } catch {
    return false;
  }
};

const Banners = () => {
  const { t } = useTranslation();
  const overview = useKbStore((s) => s.overview);
  const configState = useKbStore((s) => s.configState);
  const skills = useKbStore((s) => s.skills);
  const [offerHidden, setOfferHidden] = useState(offerDismissed);

  const stale = (skills ?? []).filter((s) => s.state === "stale");
  const anyInstalled = (skills ?? []).some((s) => s.state === "current" || s.state === "stale");
  const canOffer =
    skills != null &&
    configState?.state === "valid" &&
    !anyInstalled &&
    skills.some((s) => s.tool_found && s.state === "not_installed") &&
    !offerHidden;

  const hideOffer = () => {
    try {
      window.localStorage.setItem(SKILL_OFFER_KEY, "1");
    } catch {
      /* the offer simply comes back next time */
    }
    setOfferHidden(true);
  };

  const updateSkills = async () => {
    for (const s of stale) {
      const { error } = await useKbStore.getState().installSkill(s.target);
      if (error != null) {
        toast.error(t("kb.skill.actionFailed", { message: error }));
        return;
      }
    }
    toast.success(t("kb.skill.installed"));
  };

  const restore = async () => {
    const { error } = await useKbStore.getState().restoreDefaultConfig();
    if (error != null) toast.error(error);
    else useKbStore.getState().openWizard("first");
  };

  return (
    <div className="space-y-2">
      {configState?.state === "invalid" && (
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-md border border-destructive/50 p-3 text-sm">
          <AlertTriangle className="h-4 w-4 text-destructive" aria-hidden="true" />
          <span className="min-w-0 flex-1 break-words">
            {t("kb.banner.configInvalid", { message: configState.message })}
          </span>
          <Button size="sm" variant="outline" onClick={() => void restore()}>
            {t("kb.banner.restoreDefaults")}
          </Button>
        </div>
      )}
      {configState?.state === "newer" && (
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-md border border-destructive/50 p-3 text-sm">
          <AlertTriangle className="h-4 w-4 text-destructive" aria-hidden="true" />
          <span className="min-w-0 flex-1">
            {t("kb.banner.configNewer", { version: configState.file_version })}
          </span>
        </div>
      )}
      {configState?.state === "missing" && (
        <div className="flex flex-wrap items-center gap-3 rounded-md border p-3 text-sm">
          <span className="min-w-0 flex-1">{t("kb.banner.notSetUp")}</span>
          <Button size="sm" onClick={() => useKbStore.getState().openWizard("first")}>
            {t("kb.banner.startWizard")}
          </Button>
        </div>
      )}
      {stale.length > 0 && (
        <div role="status" className="flex flex-wrap items-center gap-3 rounded-md border border-warning/50 p-3 text-sm">
          <span className="min-w-0 flex-1">{t("kb.skillPrompt.stale")}</span>
          <Button size="sm" onClick={() => void updateSkills()}>
            {t("kb.skillPrompt.update")}
          </Button>
        </div>
      )}
      {canOffer && (
        <div role="status" className="flex flex-wrap items-center gap-3 rounded-md border p-3 text-sm">
          <span className="min-w-0 flex-1">{t("kb.skillPrompt.offer")}</span>
          <Button
            size="sm"
            onClick={() => {
              hideOffer();
              useKbStore.getState().openSettings();
            }}
          >
            {t("kb.skillPrompt.open")}
          </Button>
          <Button size="sm" variant="ghost" onClick={hideOffer}>
            {t("kb.skillPrompt.dismiss")}
          </Button>
        </div>
      )}
      {overview?.schedule_notice != null && (
        <div role="status" className="flex items-center gap-3 rounded-md border border-accent/40 p-3 text-sm">
          <span className="min-w-0 flex-1 break-all">
            {t("kb.notice.scheduleRepaired", { to: overview.schedule_notice.to })}
          </span>
          <button
            type="button"
            aria-label={t("kb.notice.dismiss")}
            onClick={() => void useKbStore.getState().dismissScheduleNotice()}
            className="rounded p-1 hover:bg-muted"
          >
            <X className="h-4 w-4" aria-hidden="true" />
          </button>
        </div>
      )}
      {overview != null &&
        overview.exe_stability !== "stable" &&
        overview.exe_stability !== "not_in_app" && (
          <div role="status" className="rounded-md border border-warning/50 p-3 text-sm">
            {t("kb.notice.moveToApplications")}
          </div>
        )}
    </div>
  );
};

const SUMMARY_KEY = "agentkb.home.summaryOpen";

const readSummaryOpen = (): boolean => {
  try {
    return window.localStorage.getItem(SUMMARY_KEY) === "1";
  } catch {
    return false;
  }
};

/**
 * The collection status and the daily digest as one line; the details open
 * on a click and the choice is remembered. Anything that needs attention
 * (a dead schedule, a paused or failing digest) is marked in the line itself.
 */
const Summary = ({
  overview,
  onRepair,
  repairing,
}: {
  overview: KbOverview;
  onRepair: () => void;
  repairing: boolean;
}) => {
  const { t } = useTranslation();
  const digest = useKbStore((s) => s.digest);
  const configValid = useKbStore((s) => s.configState?.state === "valid");
  const [open, setOpen] = useState(readSummaryOpen);

  const toggle = () => {
    const next = !open;
    setOpen(next);
    try {
      window.localStorage.setItem(SUMMARY_KEY, next ? "1" : "0");
    } catch {
      /* the choice is only not remembered */
    }
  };

  const { last_run: lastRun } = overview;
  let lastRunText = t("kb.summary.lastRun.never");
  if (lastRun != null) {
    const time = formatShortTime(lastRun.finished_at ?? lastRun.started_at);
    const problems = lastRun.summary?.errors.length ?? 0;
    lastRunText =
      lastRun.finished_at == null
        ? t("kb.summary.lastRun.running", { time })
        : problems > 0
          ? t("kb.summary.lastRun.problems", { time, count: problems })
          : t("kb.summary.lastRun.ok", { time });
  }
  const scheduleDead = scheduleHealth(overview) === "dead";
  const digestLine = configValid && digest != null ? digestSummary(digest, t) : null;
  const attention = scheduleDead || digestLine?.attention === true;

  return (
    <section aria-label={t("kb.summary.label")} className="rounded-md border text-sm">
      <button
        type="button"
        aria-expanded={open}
        aria-controls="kb-summary-details"
        onClick={toggle}
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-muted/50"
      >
        <ChevronRight
          className={cn("h-4 w-4 shrink-0 transition-transform", open && "rotate-90")}
          aria-hidden="true"
        />
        <span className="min-w-0 flex-1 truncate">
          {lastRunText}
          {scheduleDead && <> · <span className="text-destructive">{t("kb.summary.scheduleDead")}</span></>}
          {digestLine != null && (
            <>
              {" · "}
              <span className={cn(digestLine.attention && "text-warning")}>{digestLine.text}</span>
            </>
          )}
        </span>
        {attention && <AlertTriangle className="h-4 w-4 shrink-0 text-warning" aria-hidden="true" />}
        <span className="shrink-0 text-xs text-muted-foreground">
          {open ? t("kb.summary.hide") : t("kb.summary.show")}
        </span>
      </button>
      {open && (
        <div id="kb-summary-details" className="space-y-3 border-t p-3">
          <StatusSection overview={overview} onRepair={onRepair} repairing={repairing} />
          <KbDigestCard />
        </div>
      )}
    </section>
  );
};

const StatusSection = ({
  overview,
  onRepair,
  repairing,
}: {
  overview: KbOverview;
  onRepair: () => void;
  repairing: boolean;
}) => {
  const { t } = useTranslation();
  const presence = useKbStore((s) => s.presence);
  const { schedule, last_run: lastRun } = overview;
  const health = scheduleHealth(overview);

  let scheduleText: string;
  switch (health) {
    case "unsupported":
      scheduleText = t("kb.status.schedule.unsupported");
      break;
    case "dead":
      scheduleText = t("kb.status.schedule.dead");
      break;
    case "active":
      scheduleText = t("kb.status.schedule.active", {
        minutes: Math.max(1, Math.round((schedule.interval_secs ?? overview.interval_secs) / 60)),
      });
      break;
    case "installedNotLoaded":
      scheduleText = t("kb.status.schedule.installedNotLoaded");
      break;
    default:
      scheduleText = t("kb.status.schedule.notInstalled");
  }

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

  const backup = describeBackup(overview, presence, t);

  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
      <Row label={t("kb.status.lastRun")}>{lastRunText}</Row>
      <Row label={t("kb.status.schedule")}>
        <span className={cn(health === "dead" && "text-destructive")}>{scheduleText}</span>
        {health === "dead" && (
          <>
            {" "}
            <Button size="sm" variant="outline" onClick={onRepair} disabled={repairing}>
              {t("kb.action.repairSchedule")}
            </Button>
            {overview.exe_stability === "not_in_app" && (
              <span className="ml-2 text-xs text-muted-foreground">
                {t("kb.status.schedule.devNote")}
              </span>
            )}
          </>
        )}
      </Row>
      <Row label={t("kb.status.sessions")}>
        {t("kb.status.sessionsValue", {
          claude: overview.sessions_by_source.claude ?? 0,
          codex: overview.sessions_by_source.codex ?? 0,
        })}
      </Row>
      <Row label={t("kb.status.deleted")}>{String(overview.deleted_sources)}</Row>
      <Row label={t("kb.status.backup")}>
        <VerdictLine verdict={backup} />
      </Row>
      <Row label={t("kb.status.dataDir")}>
        <span className="break-all">{overview.data_dir}</span>{" "}
        <span className="text-muted-foreground">· {formatBytes(overview.data_bytes)}</span>
      </Row>
    </dl>
  );
};

const Row = ({ label, children }: { label: string; children: React.ReactNode }) => (
  <div className="contents">
    <dt className="text-muted-foreground">{label}</dt>
    <dd className="min-w-0">{children}</dd>
  </div>
);

const Filters = ({
  filters,
  facets,
}: {
  filters: KbFilters;
  facets: { clients: string[]; projects: [string, string][] };
}) => {
  const { t } = useTranslation();
  const [more, setMore] = useState(false);
  const set = (patch: Partial<KbFilters>) => void useKbStore.getState().setFilters(patch);
  const selectClass = "h-8 rounded-md border bg-background px-2 text-sm";
  const projects = [...new Set(facets.projects.map(([, name]) => name))];
  const changed = JSON.stringify(filters) !== JSON.stringify(defaultFilters);
  // What the "more" row holds, counted so a hidden filter is never a surprise.
  const hiddenActive = [
    filters.client != null,
    filters.project != null,
    filters.onlyDeleted,
    filters.onlyElsewhere,
    filters.includeSubagents,
    filters.includeEmpty,
  ].filter(Boolean).length;

  return (
    <div className="space-y-2" role="group" aria-label={t("kb.sessions.filterLabel")}>
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-1 text-sm text-muted-foreground">
          {t("kb.filters.source")}
          <select
            className={selectClass}
            value={filters.source}
            onChange={(e) => set({ source: e.target.value as KbFilters["source"] })}
          >
            <option value="all">{t("kb.sessions.filter.all")}</option>
            <option value="claude">{t("kb.sessions.filter.claude")}</option>
            <option value="codex">{t("kb.sessions.filter.codex")}</option>
          </select>
        </label>
        <label className="flex items-center gap-1 text-sm text-muted-foreground">
          {t("kb.filters.range")}
          <select
            className={selectClass}
            value={filters.range}
            onChange={(e) => set({ range: e.target.value as KbTimeRange })}
          >
            {RANGES.map((r) => (
              <option key={r} value={r}>
                {t(`kb.filters.range.${r}`)}
              </option>
            ))}
          </select>
        </label>
        <Button
          size="sm"
          variant="outline"
          aria-expanded={more}
          onClick={() => setMore(!more)}
          className="h-8"
        >
          <SlidersHorizontal className="mr-1.5 h-3.5 w-3.5" aria-hidden="true" />
          {t("kb.filters.more")}
          {hiddenActive > 0 && (
            <Badge variant="secondary" className="ml-1.5">
              {hiddenActive}
            </Badge>
          )}
        </Button>
        {changed && (
          <Button size="sm" variant="ghost" onClick={() => set({ ...defaultFilters })}>
            {t("kb.filters.reset")}
          </Button>
        )}
      </div>
      {more && (
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-md border p-2">
          <label className="flex items-center gap-1 text-sm text-muted-foreground">
            {t("kb.filters.client")}
            <select
              className={selectClass}
              value={filters.client ?? ""}
              onChange={(e) => set({ client: e.target.value === "" ? null : e.target.value })}
            >
              <option value="">{t("kb.sessions.filter.all")}</option>
              {facets.clients.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-1 text-sm text-muted-foreground">
            {t("kb.filters.project")}
            <select
              className={cn(selectClass, "max-w-[12rem]")}
              value={filters.project ?? ""}
              onChange={(e) => set({ project: e.target.value === "" ? null : e.target.value })}
            >
              <option value="">{t("kb.sessions.filter.all")}</option>
              {projects.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            <input
              type="checkbox"
              checked={filters.onlyDeleted}
              onChange={(e) => set({ onlyDeleted: e.target.checked })}
            />
            {t("kb.filters.onlyDeleted")}
          </label>
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            <input
              type="checkbox"
              checked={filters.onlyElsewhere}
              onChange={(e) => set({ onlyElsewhere: e.target.checked })}
            />
            {t("kb.filters.onlyElsewhere")}
          </label>
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            <input
              type="checkbox"
              checked={filters.includeSubagents}
              onChange={(e) => set({ includeSubagents: e.target.checked })}
            />
            {t("kb.sessions.includeSubagents")}
          </label>
          <label className="flex items-center gap-2 text-sm text-muted-foreground">
            <input
              type="checkbox"
              checked={filters.includeEmpty}
              onChange={(e) => set({ includeEmpty: e.target.checked })}
            />
            {t("kb.sessions.includeEmpty")}
          </label>
        </div>
      )}
    </div>
  );
};

const SessionRow = ({ session, onOpen }: { session: KbSession; onOpen: () => void }) => {
  const { t } = useTranslation();
  const title = session.summary ?? t("kb.sessions.untitled");
  const deleted = session.source_status === "deleted";
  const elsewhere = session.source_status === "elsewhere";
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
          {elsewhere && (
            <Badge
              variant="muted"
              title={t("kb.sessions.elsewhereHint", { device: session.origin ?? "?" })}
            >
              {t("kb.sessions.elsewhereBadge")}
            </Badge>
          )}
        </span>
        <span className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
          <Badge variant="outline">{session.provider}</Badge>
          {session.entrypoint != null && <span>{session.entrypoint}</span>}
          <span>{session.project_name || "—"}</span>
          <span title={t("kb.sessions.messagesHint")}>
            {t("kb.sessions.messages", { count: session.message_count })}
          </span>
          {session.is_subagent && <Badge variant="muted">{t("kb.sessions.subagentBadge")}</Badge>}
          <span>{formatTime(session.last_modified)}</span>
        </span>
      </button>
    </li>
  );
};
