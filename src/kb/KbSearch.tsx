import { Fragment, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { AlertTriangle, Loader2, Search, X } from "lucide-react";
import { Badge, Input } from "@/components/ui";
import { formatTime } from "@/utils/time";
import { NoteCard } from "./KbNotes";
import { openKbSession } from "./openSession";
import { focusOf, sessionFromHit } from "./searchHit";
import type { KbHit, KbSessionHit } from "./types";
import { useKbStore } from "./useKbStore";

const DEBOUNCE_MS = 300;

/** `text` with `<mark>` around the given `[start, end)` character ranges. */
export const Highlighted = ({
  text,
  ranges,
}: {
  text: string;
  ranges: [number, number][];
}) => {
  const chars = Array.from(text);
  const parts: React.ReactNode[] = [];
  let cursor = 0;
  ranges.forEach(([start, end], i) => {
    if (start < cursor || end > chars.length || end <= start) return;
    if (start > cursor) parts.push(<Fragment key={`t${i}`}>{chars.slice(cursor, start).join("")}</Fragment>);
    parts.push(
      <mark key={`m${i}`} className="rounded bg-accent/30 px-0.5 text-foreground">
        {chars.slice(start, end).join("")}
      </mark>,
    );
    cursor = end;
  });
  if (cursor < chars.length) parts.push(<Fragment key="rest">{chars.slice(cursor).join("")}</Fragment>);
  return <>{parts}</>;
};

export const SearchBox = () => {
  const { t } = useTranslation();
  const query = useKbStore((s) => s.search.query);
  const status = useKbStore((s) => s.search.status);
  const focusPending = useKbStore((s) => s.searchFocusPending);
  const input = useRef<HTMLInputElement>(null);

  // ⌘K asked for the search box: take the focus once, with the text selected.
  useEffect(() => {
    if (!focusPending) return;
    input.current?.focus();
    input.current?.select();
    useKbStore.getState().clearSearchFocus();
  }, [focusPending]);

  // Search as the user types, once they pause.
  useEffect(() => {
    if (query.trim() === "") return;
    const timer = window.setTimeout(() => void useKbStore.getState().runSearch(), DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [query]);

  return (
    <div className="relative">
      <Search
        className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
        aria-hidden="true"
      />
      <Input
        ref={input}
        value={query}
        onChange={(e) => useKbStore.getState().setSearchQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void useKbStore.getState().runSearch();
          if (e.key === "Escape") useKbStore.getState().setSearchQuery("");
        }}
        placeholder={t("kb.search.placeholder")}
        aria-label={t("kb.search.placeholder")}
        className="h-10 pl-9 pr-9"
      />
      {status === "loading" ? (
        <Loader2
          className="absolute right-3 top-1/2 h-4 w-4 -translate-y-1/2 animate-spin text-muted-foreground"
          aria-hidden="true"
        />
      ) : (
        query !== "" && (
          <button
            type="button"
            aria-label={t("kb.search.clear")}
            onClick={() => useKbStore.getState().setSearchQuery("")}
            className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 hover:bg-muted"
          >
            <X className="h-4 w-4" aria-hidden="true" />
          </button>
        )
      )}
    </div>
  );
};

const open = async (
  hit: KbSessionHit,
  focusHit: KbHit | undefined,
  t: (key: string, options?: Record<string, unknown>) => string,
) => {
  try {
    const found = await openKbSession(
      sessionFromHit(hit),
      // A hit inside a sub-agent session has no message to jump to in the
      // parent's own transcript.
      focusHit == null || focusHit.subagent_session_id != null ? undefined : focusOf(focusHit),
    );
    if (!found) toast.info(t("kb.search.focusFailed"));
  } catch (e) {
    toast.error(t("kb.toast.openFailed", { message: e instanceof Error ? e.message : String(e) }));
  }
};

export const SearchResults = () => {
  const { t } = useTranslation();
  const { status, response, message } = useKbStore((s) => s.search);

  if (status === "loading" && response == null) {
    return (
      <p role="status" className="flex items-center justify-center gap-2 p-6 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
        {t("kb.search.searching")}
      </p>
    );
  }
  if (status === "not_ready") {
    return (
      <p role="status" className="p-6 text-center text-sm text-muted-foreground">
        {t("kb.search.notReady")}
      </p>
    );
  }
  if (status === "no_data") {
    return (
      <p role="status" className="p-6 text-center text-sm text-muted-foreground">
        {t("kb.search.noData")}
      </p>
    );
  }
  if (status === "error") {
    return (
      <div role="alert" className="flex items-center justify-center gap-2 p-6 text-sm text-destructive">
        <AlertTriangle className="h-4 w-4" aria-hidden="true" />
        {t("kb.search.error", { message: message ?? "" })}
      </div>
    );
  }
  if (response == null) return null;
  const notes = response.notes ?? [];
  if (response.results.length === 0 && notes.length === 0) {
    return (
      <div className="p-6 text-center text-sm text-muted-foreground">
        <p>{t("kb.search.none")}</p>
        <p className="mt-1">{t("kb.search.noneHint")}</p>
      </div>
    );
  }

  return (
    <div aria-busy={status === "loading"}>
      <p className="border-b px-4 py-2 text-xs text-muted-foreground">
        {t("kb.search.found", {
          count: response.matched_sessions,
          time: response.data_as_of == null ? "—" : formatTime(response.data_as_of),
        })}
        {response.mode_used === "any" && <> · {t("kb.search.relaxed")}</>}
      </p>
      {notes.length > 0 && (
        <section aria-label={t("kb.search.notesTitle")} className="border-b bg-muted/30">
          <h3 className="px-4 pt-2 text-xs font-medium text-muted-foreground">
            {t("kb.search.notesTitle")}
          </h3>
          <ul className="divide-y">
            {notes.map((note) => (
              <NoteCard
                key={`${note.session.source}:${note.session.session_id}:${note.entry_id}`}
                note={note}
              />
            ))}
          </ul>
        </section>
      )}
      <ul className="divide-y">
        {response.results.map((hit) => (
          <ResultCard key={`${hit.session.source}:${hit.session.session_id}`} hit={hit} />
        ))}
      </ul>
    </div>
  );
};

const ResultCard = ({ hit }: { hit: KbSessionHit }) => {
  const { t } = useTranslation();
  const s = hit.session;
  const title = s.title ?? t("kb.sessions.untitled");
  const deleted = s.source_status === "deleted";
  return (
    <li className="px-4 py-3">
      <button
        type="button"
        onClick={() => void open(hit, hit.hits[0], t)}
        aria-label={t("kb.sessions.open", { title })}
        className="flex w-full flex-col gap-1 text-left hover:text-accent-foreground focus-visible:outline-none focus-visible:underline"
      >
        <span className="flex items-center gap-2">
          <span className="truncate font-medium">{title}</span>
          {deleted && (
            <Badge variant="destructive" title={t("kb.sessions.deletedHint")}>
              {t("kb.sessions.deletedBadge")}
            </Badge>
          )}
          {s.is_subagent && <Badge variant="muted">{t("kb.sessions.subagentBadge")}</Badge>}
          {hit.has_note === true && <Badge variant="outline">{t("kb.search.hasNote")}</Badge>}
        </span>
        <span className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
          <Badge variant="outline">{s.source}</Badge>
          {s.client != null && <span>{s.client}</span>}
          <span>{s.project ?? "—"}</span>
          <span>{formatTime(s.last_activity_at ?? s.started_at ?? "")}</span>
        </span>
      </button>
      <ul className="mt-2 space-y-1">
        {hit.hits.map((h) => (
          <li key={`${h.subagent_session_id ?? ""}:${h.seq}`}>
            <button
              type="button"
              onClick={() => void open(hit, h, t)}
              className="flex w-full items-start gap-2 rounded px-2 py-1 text-left text-sm hover:bg-muted focus-visible:bg-muted focus-visible:outline-none"
            >
              <span className="mt-0.5 shrink-0 text-xs text-muted-foreground">
                {h.subagent_session_id != null
                  ? t("kb.sessions.subagentBadge")
                  : t(`kb.search.kind.${h.kind}`)}
              </span>
              <span className="min-w-0 break-words">
                <Highlighted text={h.snippet} ranges={h.highlights} />
              </span>
            </button>
          </li>
        ))}
      </ul>
      {(hit.subagent_hits ?? 0) > 0 && (
        <p className="mt-1 px-2 text-xs text-muted-foreground">
          {t("kb.search.subagentHits", { count: hit.subagent_hits })}
        </p>
      )}
      {hit.hit_messages > hit.hits.length && (
        <p className="mt-1 px-2 text-xs text-muted-foreground">
          {t("kb.search.moreHits", { count: hit.hit_messages - hit.hits.length })}
        </p>
      )}
    </li>
  );
};
