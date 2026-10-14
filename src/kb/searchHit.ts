import type { ClaudeMessage } from "@/types";
import type { KbHit, KbSessionHit, KbSession } from "./types";

/** A search hit's session in the shape the viewer opens. */
export const sessionFromHit = (hit: KbSessionHit): KbSession => {
  const s = hit.session;
  const last = s.last_activity_at ?? s.started_at ?? "";
  return {
    session_id: hit.paths.mirror,
    actual_session_id: s.session_id,
    file_path: hit.paths.mirror,
    project_name: s.project ?? "",
    message_count: s.message_count,
    first_message_time: s.started_at ?? "",
    last_message_time: s.last_activity_at ?? "",
    last_modified: last,
    has_tool_use: false,
    has_errors: false,
    summary: s.title ?? undefined,
    provider: s.source,
    storage_type: "jsonl",
    entrypoint: s.client ?? undefined,
    cwd: s.cwd,
    source_status: s.source_status,
    source_path: hit.paths.original,
    parent_session_id: s.parent_session_id,
    is_subagent: s.is_subagent,
  } as KbSession;
};

/** Which part of a session a hit points at. */
export interface KbFocus {
  msgKey: string | null;
  at: string | null;
  kind: KbHit["kind"];
}

export const focusOf = (hit: KbHit): KbFocus => ({
  msgKey: hit.msg_key,
  at: hit.at,
  kind: hit.kind,
});

const timeOf = (iso: string | null | undefined): number => {
  const t = iso == null ? Number.NaN : Date.parse(iso);
  return t;
};

/**
 * The viewer's message id for a hit. Claude hits carry the line's `uuid`.
 * Codex messages get their ids from a counter inside the viewer's loader, so
 * they are found by timestamp and role among the complete message list.
 */
export const resolveFocusUuid = (
  all: ClaudeMessage[],
  focus: KbFocus,
): string | null => {
  if (focus.msgKey != null && all.some((m) => m.uuid === focus.msgKey)) {
    return focus.msgKey;
  }
  const target = timeOf(focus.at);
  if (Number.isNaN(target)) return null;
  const wanted =
    focus.kind === "user" ? "user" : focus.kind === "assistant" ? "assistant" : null;
  const sameKind = (m: ClaudeMessage) => wanted == null || m.type === wanted;
  const exact = all.filter((m) => timeOf(m.timestamp) === target && sameKind(m));
  if (exact.length > 0) return exact[0]?.uuid ?? null;
  // Otherwise the closest message of that kind.
  let best: ClaudeMessage | null = null;
  let bestGap = Number.POSITIVE_INFINITY;
  for (const m of all) {
    if (!sameKind(m)) continue;
    const gap = Math.abs(timeOf(m.timestamp) - target);
    if (gap < bestGap) {
      best = m;
      bestGap = gap;
    }
  }
  return best?.uuid ?? null;
};
