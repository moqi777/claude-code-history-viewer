import { describe, expect, it } from "vitest";
import type { ClaudeMessage } from "@/types";
import { resolveFocusUuid, sessionFromHit } from "./searchHit";
import type { KbSessionHit } from "./types";

const message = (uuid: string, type: string, timestamp: string) =>
  ({ uuid, type, timestamp }) as unknown as ClaudeMessage;

const all = [
  message("codex-1", "user", "2026-09-09T15:40:12.100Z"),
  message("codex-2", "assistant", "2026-09-09T15:40:12.100Z"),
  message("codex-9", "assistant", "2026-09-09T15:41:00.000Z"),
  message("claude-uuid", "assistant", "2026-09-09T16:00:00.000Z"),
];

describe("finding the viewer message a search hit points at", () => {
  it("uses the Claude uuid when the viewer has it", () => {
    expect(
      resolveFocusUuid(all, { msgKey: "claude-uuid", at: null, kind: "assistant" }),
    ).toBe("claude-uuid");
  });

  it("matches a Codex message by timestamp and role", () => {
    expect(
      resolveFocusUuid(all, { msgKey: null, at: "2026-09-09T15:40:12.100Z", kind: "assistant" }),
    ).toBe("codex-2");
    expect(
      resolveFocusUuid(all, { msgKey: null, at: "2026-09-09T15:40:12.100Z", kind: "user" }),
    ).toBe("codex-1");
  });

  it("falls back to the closest message of that kind, and to nothing without a time", () => {
    expect(
      resolveFocusUuid(all, { msgKey: null, at: "2026-09-09T15:40:50.000Z", kind: "assistant" }),
    ).toBe("codex-9");
    expect(resolveFocusUuid(all, { msgKey: null, at: null, kind: "assistant" })).toBeNull();
    expect(resolveFocusUuid([], { msgKey: "x", at: "2026-09-09T15:40:12Z", kind: "user" })).toBeNull();
  });

  it("a tool hit matches on time alone", () => {
    expect(
      resolveFocusUuid(all, { msgKey: null, at: "2026-09-09T15:41:00.000Z", kind: "tool_use" }),
    ).toBe("codex-9");
  });
});

describe("opening a hit's session", () => {
  it("builds the session the viewer opens from the mirror copy", () => {
    const hit = {
      session: {
        source: "codex",
        session_id: "abc",
        title: "T",
        client: "vscode",
        project: "p",
        cwd: "/x/p",
        started_at: "2026-09-09T15:00:00Z",
        last_activity_at: "2026-09-09T16:00:00Z",
        source_status: "deleted",
        is_subagent: false,
        parent_session_id: null,
        message_count: 7,
      },
      paths: { mirror: "/AgentKB/raw/codex/sessions/r.jsonl", original: "/home/.codex/r.jsonl" },
    } as unknown as KbSessionHit;
    expect(sessionFromHit(hit)).toMatchObject({
      session_id: "/AgentKB/raw/codex/sessions/r.jsonl",
      file_path: "/AgentKB/raw/codex/sessions/r.jsonl",
      actual_session_id: "abc",
      provider: "codex",
      cwd: "/x/p",
      summary: "T",
      source_status: "deleted",
      source_path: "/home/.codex/r.jsonl",
      last_modified: "2026-09-09T16:00:00Z",
    });
  });
});
