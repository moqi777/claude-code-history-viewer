import type { ClaudeProject, ProviderId } from "@/types";
import { useAppStore } from "@/store/useAppStore";
import { kbApi } from "./api";
import type { KbSession } from "./types";

export const KB_PROJECT_PREFIX = "kb://";

/** Projects built here never exist on disk; the base store must not try to load them. */
export const isKbProject = (project: { path: string } | null | undefined): boolean =>
  project?.path.startsWith(KB_PROJECT_PREFIX) === true;

const SIBLINGS_LIMIT = 500;

const leaf = (path: string): string => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

/**
 * A stand-in project for the backed-up sessions that share one working
 * directory, so the viewer's three-column layout (sessions list, header) shows
 * the right source and project exactly as for a live project.
 */
export const syntheticProject = (session: KbSession, siblings: KbSession[]): ClaudeProject => {
  const cwd = session.cwd;
  const newest = siblings.reduce(
    (latest, s) => (s.last_modified > latest ? s.last_modified : latest),
    session.last_modified,
  );
  return {
    name: session.project_name || (cwd != null ? leaf(cwd) : session.provider ?? "project"),
    path: `${KB_PROJECT_PREFIX}${session.provider}/${cwd ?? session.project_name}`,
    actual_path: cwd ?? "",
    session_count: siblings.length,
    message_count: siblings.reduce((sum, s) => sum + s.message_count, 0),
    last_modified: newest,
    // Without a recorded working directory a resume command would be wrong.
    ...(cwd == null ? { path_status: "unavailable" as const } : {}),
    provider: (session.provider ?? "claude") as ProviderId,
    storage_type: "jsonl",
  };
};

/**
 * Open a backed-up session in the viewer. The middle column lists the other
 * backed-up sessions of the same project (including ones whose source file is
 * gone) and the header shows the session's source and project.
 */
export async function openKbSession(session: KbSession): Promise<void> {
  const siblings =
    session.cwd != null
      ? await kbApi.listSessions({
          cwd: session.cwd,
          source: session.provider as "claude" | "codex",
          include_subagents: true,
          include_empty: true,
          limit: SIBLINGS_LIMIT,
        })
      : [session];
  const list = siblings.some((s) => s.file_path === session.file_path)
    ? siblings
    : [session, ...siblings];

  const app = useAppStore.getState();
  app.exitSessionSelectionMode();
  useAppStore.setState({
    selectedProject: syntheticProject(session, list),
    sessions: list,
    sessionsTotal: list.length,
    sessionsOffset: list.length,
    hasMoreSessions: false,
    isLoadingSessions: false,
    isLoadingMoreSessions: false,
    selectedSession: null,
  });
  app.setViewingGlobalStats(false);
  app.setAnalyticsCurrentView("messages");
  await useAppStore.getState().selectSession(session);
}
