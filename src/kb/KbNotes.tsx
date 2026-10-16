import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, ChevronDown, ChevronRight, Loader2, Search, X } from "lucide-react";
import { Badge, Button, Input } from "@/components/ui";
import { formatTime } from "@/utils/time";
import { kbApi } from "./api";
import { openNote } from "./noteActions";
import type { KbNote, KbNotesPage, KbSource } from "./types";

const PAGE = 50;
const DEBOUNCE_MS = 300;

const LABELS = ["problem", "cause", "solution"] as const;

/** One note: the conclusion up front, the rest on demand. */
export const NoteCard = ({ note, startOpen = false }: { note: KbNote; startOpen?: boolean }) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(startOpen);
  const sessionTitle = note.session.title ?? t("kb.sessions.untitled");
  const from = note.evidence.seq_from;
  const to = note.evidence.seq_to;
  return (
    <li className="space-y-1 px-4 py-3">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="flex w-full items-start gap-2 text-left focus-visible:outline-none focus-visible:underline"
      >
        {open ? (
          <ChevronDown className="mt-1 h-4 w-4 shrink-0" aria-hidden="true" />
        ) : (
          <ChevronRight className="mt-1 h-4 w-4 shrink-0" aria-hidden="true" />
        )}
        <span className="min-w-0 flex-1">
          <span className="block break-words font-medium">{note.title}</span>
          <span className="block break-words text-sm text-muted-foreground">
            {t("kb.notes.conclusion")}: {note.conclusion}
          </span>
        </span>
        <span className="shrink-0 text-xs text-muted-foreground">
          {formatTime(note.digested_at)}
        </span>
      </button>
      {open && (
        <dl className="ml-6 space-y-1 text-sm">
          {LABELS.filter((key) => note[key] !== "").map((key) => (
            <div key={key}>
              <dt className="text-xs text-muted-foreground">{t(`kb.notes.${key}`)}</dt>
              <dd className="whitespace-pre-wrap break-words">{note[key]}</dd>
            </div>
          ))}
        </dl>
      )}
      <div className="ml-6 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <Badge variant="outline">{note.session.source}</Badge>
        <span className="min-w-0 truncate" title={sessionTitle}>
          {sessionTitle}
        </span>
        {note.keywords.slice(0, 6).map((k) => (
          <span key={k}>#{k}</span>
        ))}
        <Button
          size="sm"
          variant="outline"
          onClick={() => void openNote(note, t)}
          aria-label={t("kb.notes.openAria", { title: note.title })}
        >
          {t("kb.notes.open", { from, to })}
        </Button>
      </div>
    </li>
  );
};

type Loaded =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "ok"; page: KbNotesPage };

/** The notes tab: newest first, with a search box over the notes only. */
export const KbNotes = () => {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [source, setSource] = useState<"all" | KbSource>("all");
  const [state, setState] = useState<Loaded>({ status: "loading" });
  const [moreBusy, setMoreBusy] = useState(false);
  const epoch = useRef(0);

  const load = useCallback(
    async (offset: number) => {
      const mine = ++epoch.current;
      try {
        const page = await kbApi.notesList({
          ...(query.trim() !== "" ? { query: query.trim() } : {}),
          ...(source !== "all" ? { source } : {}),
          limit: PAGE,
          offset,
        });
        if (mine !== epoch.current) return;
        setState((prev) =>
          offset > 0 && prev.status === "ok"
            ? { status: "ok", page: { total: page.total, notes: [...prev.page.notes, ...page.notes] } }
            : { status: "ok", page },
        );
      } catch (e) {
        if (mine !== epoch.current) return;
        setState({ status: "error", message: e instanceof Error ? e.message : String(e) });
      }
    },
    [query, source],
  );

  useEffect(() => {
    setState({ status: "loading" });
    const timer = window.setTimeout(() => void load(0), query === "" ? 0 : DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [load, query]);

  const showMore = async () => {
    if (state.status !== "ok") return;
    setMoreBusy(true);
    await load(state.page.notes.length);
    setMoreBusy(false);
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="sticky top-[2.35rem] z-10 -mx-4 flex flex-wrap items-center gap-2 bg-background px-4 py-1 md:-mx-6 md:px-6">
        <div className="relative min-w-[12rem] flex-1">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQuery("")}
            placeholder={t("kb.notes.searchPlaceholder")}
            aria-label={t("kb.notes.searchPlaceholder")}
            className="h-9 pl-9 pr-9"
          />
          {query !== "" && (
            <button
              type="button"
              aria-label={t("kb.search.clear")}
              onClick={() => setQuery("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 hover:bg-muted"
            >
              <X className="h-4 w-4" aria-hidden="true" />
            </button>
          )}
        </div>
        <label className="flex items-center gap-1 text-sm text-muted-foreground">
          {t("kb.filters.source")}
          <select
            className="h-8 rounded-md border bg-background px-2 text-sm"
            value={source}
            onChange={(e) => setSource(e.target.value as "all" | KbSource)}
          >
            <option value="all">{t("kb.sessions.filter.all")}</option>
            <option value="claude">{t("kb.sessions.filter.claude")}</option>
            <option value="codex">{t("kb.sessions.filter.codex")}</option>
          </select>
        </label>
      </div>

      <div className="rounded-md border">
        {state.status === "loading" && (
          <p role="status" className="flex items-center justify-center gap-2 p-6 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />
            {t("kb.search.searching")}
          </p>
        )}
        {state.status === "error" && (
          <div role="alert" className="flex items-center justify-center gap-2 p-6 text-sm text-destructive">
            <AlertTriangle className="h-4 w-4" aria-hidden="true" />
            {t("kb.search.error", { message: state.message })}
          </div>
        )}
        {state.status === "ok" &&
          (state.page.notes.length === 0 ? (
            <p className="p-6 text-center text-sm text-muted-foreground">
              {query.trim() === "" ? t("kb.notes.empty") : t("kb.notes.noMatch")}
            </p>
          ) : (
            <>
              <p className="border-b px-4 py-2 text-xs text-muted-foreground">
                {query.trim() === ""
                  ? t("kb.notes.total", { count: state.page.total })
                  : t("kb.notes.found", { count: state.page.notes.length })}
              </p>
              <ul className="divide-y">
                {state.page.notes.map((note) => (
                  <NoteCard
                    key={`${note.session.source}:${note.session.session_id}:${note.entry_id}`}
                    note={note}
                  />
                ))}
              </ul>
              {query.trim() === "" && state.page.notes.length < state.page.total && (
                <div className="p-3 text-center">
                  <Button size="sm" variant="outline" disabled={moreBusy} onClick={() => void showMore()}>
                    {t("kb.notes.more")}
                  </Button>
                </div>
              )}
            </>
          ))}
      </div>
    </div>
  );
};
