// Headless render of the P6 screens against a mocked Tauri IPC (no real data).
// usage: node shot-p6.mjs <outdir>
import { createRequire } from "node:module";
const require = createRequire("/Users/mo7/.nvm/versions/node/v20.13.1/lib/node_modules/@playwright/cli/node_modules/playwright-core/package.json");
const { chromium } = require("./index.js");
const outDir = process.argv[2];
const chrome = `${process.env.HOME}/Library/Caches/ms-playwright/chromium-1228/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`;

const config = {
  version: 1, data_dir: "/Users/me/AgentKB", interval_secs: 600, schedule_enabled: true,
  sources: { claude: { enabled: true, root: null }, codex: { enabled: true, root: null } },
  exclude_prefixes: [], exclude_cwd_prefixes: [], mirror: { enabled: false, target_dir: null, volume_uuid: null, volume_name: null },
  semantic: { enabled: false, source: "modelscope" },
  digest: { enabled: false, engine: "claude", model: "", cli_path: null, time: "03:00", max_sessions_per_round: 30, recent_days: 7, quiet_minutes: 30, min_messages: 4, max_chunks_per_session: 3, codex_reasoning: "low" },
};
const session = (i, over = {}) => ({
  session_id: `/AgentKB/raw/claude/p/${i}.jsonl`, actual_session_id: `a${i}`, file_path: `/AgentKB/raw/claude/p/${i}.jsonl`,
  project_name: "my-project", message_count: 12 + i, first_message_time: "2026-10-14T09:00:00Z",
  last_message_time: "2026-10-14T09:30:00Z", last_modified: "2026-10-14T09:30:00Z", has_tool_use: false, has_errors: false,
  summary: `Fix the login bug #${i}`, provider: "claude", storage_type: "jsonl", entrypoint: "cli", cwd: "/Users/me/proj",
  source_status: "present", source_path: "/x", parent_session_id: null, is_subagent: false, ...over,
});
const sessions = [
  session(1, { source_status: "elsewhere", origin: "MacBook-A", summary: "Deadlock in the order pool" }),
  session(2, { source_status: "elsewhere", origin: "MacBook-A", provider: "codex", summary: "Retry for the uploader", entrypoint: "exec" }),
  session(3), session(4, { source_status: "deleted", summary: "Gone from disk" }), session(5),
];
const overview = {
  data_dir: "/Users/me/AgentKB", db_exists: true, sessions_by_source: { claude: 40, codex: 228 }, deleted_sources: 1,
  last_run: { started_at: "2026-10-14T10:00:00Z", finished_at: "2026-10-14T10:00:05Z", trigger: "schedule", ok: true, summary: { status: "ok", changed_files: 3, errors: [] } },
  running: false, activity: "idle", backup: { last_at: null, last_result: null, last_skipped_at: null, last_bytes: null, progress: null },
  schedule_notice: null, data_bytes: 2_900_000_000,
  schedule: { supported: true, label: "x", plist_path: "/p", installed: true, loaded: true, interval_secs: 600, program: "/Applications/AgentKB.app/Contents/MacOS/AgentKB", plist_program: "/Applications/AgentKB.app/Contents/MacOS/AgentKB", program_exists: true, state: "not running", last_exit_code: 0, runs: 4 },
  interval_secs: 600, schedule_enabled: true, exe_stability: "stable", agentkb_version: "0.6.0", base_version: "1.32.0",
  mirror: { enabled: false, target_dir: null, volume_name: null },
};
const counts = { sessions: 313, notes: 116, sessions_with_notes: 19, raw_files: 527, note_files: 19 };
const inspect = (empty) => ({ export_id: "id1", exported_at: "2026-10-16T08:40:12Z", app_version: "0.6.0", schema_version: 5, device_name: "MacBook-A", device_os: "macos", counts, uncompressed_bytes: 3.9e9, file_bytes: 2.14e9,
  local: empty ? { empty: true, sessions: 0, notes: 0, raw_files: 0, note_files: 0 } : { empty: false, sessions: 40, notes: 5, raw_files: 40, note_files: 3 }, needed_bytes: 4.3e9, free_bytes: 80e9, problem: null });
const mergeSummary = { sessions_added: 23, files_added: 25, identical: 290, extended: 1, kept_both_live: 1, kept_both_diverged: 1, kept_both_other: 0, row_collisions: 0, notes_added: 3, notes_identical: 10, notes_conflicts: 5, expected_sessions: 336, expected_notes: 121, expected_raw_files: 552, expected_note_files: 22 };

const mode = process.env.IMPORT_MODE ?? "merge";
const mock = `
const S = ${JSON.stringify({ config, sessions, overview, counts, inspectEmpty: inspect(true), inspectMerge: inspect(false), mergeSummary })};
let cbId = 1;
window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
window.__TAURI_INTERNALS__ = {
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
  transformCallback: () => cbId++, unregisterCallback() {}, convertFileSrc: (p) => p,
  invoke: async (cmd, args) => {
    switch (cmd) {
      case "kb_semantic_status": return window.__SEM();
      case "kb_semantic_test": await new Promise(r => setTimeout(r, 400)); return { ok: true, seconds: 1.8, dim: 1024, message: "" };
      case "kb_semantic_download": await new Promise(r => setTimeout(r, 60000)); return null;
      case "kb_semantic_set": window.__SET = args; return window.__SEM();
      case "kb_search": return window.__SEARCH;
      case "kb_get_status": return S.overview;
      case "kb_get_backup_presence": return { state: "disabled" };
      case "kb_list_sessions": return S.sessions;
      case "kb_digest_status": return { enabled: false, engine: "claude", model: "", time: "03:00", running: null, paused: null, next_run_at: null, last_round: null, failures: [], pending_auto: 0, pending_older: 0, first_round_tokens: null };
      case "kb_notes_list": return [];
      case "kb_session_facets": return { clients: ["cli", "exec"], projects: [["claude", "my-project"]] };
      case "kb_get_config_state": return { state: window.__CFG ?? "valid", config: S.config, config_path: "/x/config.json" };
      case "kb_get_config": return S.config;
      case "kb_skill_status": return [];
      case "kb_probe_sources": return [{ source: "claude", root: "/Users/me/.claude", exists: true, sessions: 40, bytes: 342e6 }, { source: "codex", root: "/Users/me/.codex", exists: true, sessions: 228, bytes: 2.5e9 }];
      case "kb_test_data_dir": return { path: "/Users/me/AgentKB", exists: false, will_create: true, writable: true, free_bytes: 120e9, existing: null, db_too_new: false, overlaps_source: null, error: null };
      case "kb_digest_find_cli": return null;
      case "plugin:dialog|save": return "/Volumes/Disk/AgentKB-2026-10-16.agentkb";
      case "plugin:dialog|open": return "/Volumes/Disk/AgentKB-2026-10-16.agentkb";
      case "kb_export_start": await new Promise(r => setTimeout(r, window.__SLOW ? 60000 : 300)); return { path: args.dest, export_id: "x", file_bytes: 2.14e9, uncompressed_bytes: 4.0e9, files: 548, sessions: 313, notes: 116, sessions_with_notes: 19, raw_files: 527, note_files: 19, duration_ms: 8200, paused_collection: false };
      case "kb_import_inspect": return window.__EMPTY ? S.inspectEmpty : S.inspectMerge;
      case "kb_import_prepare":
        if (window.__CORRUPT) throw new Error("corrupt: the checksum of raw/claude/projects/p/x.jsonl is wrong: the file is damaged");
        return { export_id: "id1", mode: window.__EMPTY ? "empty" : "merge", device_name: "MacBook-A", counts: S.counts, summary: window.__EMPTY ? null : S.mergeSummary, added_exclusions: window.__EMPTY ? [] : ["/Users/me/Dev/secret"], local: window.__EMPTY ? S.inspectEmpty.local : S.inspectMerge.local };
      case "kb_import_commit": await new Promise(r => setTimeout(r, 400)); return { mode: window.__EMPTY ? "empty" : "merge", export_id: "id1", device_name: "MacBook-A", exported_at: "x", summary: window.__EMPTY ? null : S.mergeSummary, checks: [{ name: "sessions", expected: 313, actual: 313 }, { name: "notes", expected: 116, actual: 116 }], all_match: true, backup_db: null, backup_config: null, added_exclusions: [], config: S.config, config_written: true, warnings: [], duration_ms: 9000 };
      case "kb_import_discard": case "kb_transfer_cancel": case "kb_sync_now": return null;
      case "plugin:event|listen": return 1;
      case "plugin:event|unlisten": return null;
      case "plugin:store|load": return 1;
      case "plugin:store|get": return [null, false];
      case "plugin:store|set": case "plugin:store|save": return null;
      case "plugin:os|locale": return "zh-Hans-CN";
      case "get_startup_session_hint": return null;
      case "get_claude_folder_path": return "/Users/me/.claude";
      case "load_user_metadata": return { version: 1, sessions: {}, projects: {}, settings: {} };
      case "detect_providers": case "scan_all_projects": case "scan_projects": return [];
      default: return null;
    }
  },
};
`;


const semStatus = (over = {}) => ({
  enabled: false, source: "modelscope",
  sources: [{ id: "modelscope", host: "modelscope.cn" }, { id: "huggingface", host: "huggingface.co" }],
  model_name: "bge-m3 (int8)", model_bytes: 585562194, model: { state: "missing" }, downloading: false,
  index: { status: "idle", total: 0, done: 0, updated_at: "", message: "", postponed_rounds: 0, indexed: 0, building: false },
  vectors_bytes: 0, models_dir: "/Users/me/AgentKB/models/bge-m3-int8", ...over,
});
const withSem = (s) => `window.__SEM = () => (${JSON.stringify(s)});`;
const states = {
  "semantic-missing": semStatus(),
  "semantic-building": semStatus({ enabled: true, model: { state: "ready" }, vectors_bytes: 19_000_000,
    index: { status: "running", total: 24385, done: 18400, updated_at: "", message: "", postponed_rounds: 0, indexed: 18400, building: true } }),
  "semantic-ready": semStatus({ model: { state: "ready" } }),
  "semantic-postponed": semStatus({ enabled: true, model: { state: "ready" }, vectors_bytes: 6_000_000,
    index: { status: "postponed_memory", total: 24385, done: 6000, updated_at: "", message: "1500 MB available", postponed_rounds: 3, indexed: 6000, building: false } }),
  "semantic-corrupt": semStatus({ model: { state: "corrupt", file: "model.onnx", reason: "the SHA-256 does not match" } }),
};
const browser = await chromium.launch({ executablePath: chrome, headless: true });
const open = async (flags = "", height = 900) => {
  const page = await browser.newPage({ viewport: { width: 1280, height }, deviceScaleFactor: 1 });
  page.on("pageerror", (e) => console.log(`pageerror: ${e.message}`));
  await page.addInitScript(mock + flags);
  await page.goto("http://localhost:5174/", { waitUntil: "networkidle" });
  await page.waitForTimeout(1200);
  return page;
};
const toSettings = async (page) => {
  const b = page.getByRole("button", { name: /Knowledge Base|知识库/ }).first();
  if (await b.count()) await b.click();
  await page.waitForTimeout(1000);
  await page.locator("#main-content").getByRole("button", { name: /^(Settings|设置)$/ }).click();
  await page.waitForTimeout(1000);
};
for (const [name, status] of Object.entries(states)) {
  const page = await open(withSem(status), 1500);
  await toSettings(page);
  await page.getByText("语义搜索").first().scrollIntoViewIfNeeded();
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${outDir}/${name}.png` });
  if (name === "semantic-missing") {
    await page.getByRole("button", { name: "下载模型" }).click();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${outDir}/semantic-downloading.png` });
  }
  if (name === "semantic-ready") {
    await page.locator("button:not([disabled])", { hasText: /^测试$/ }).last().click();
    await page.waitForTimeout(900);
    await page.screenshot({ path: `${outDir}/semantic-tested.png` });
  }
  await page.close();
}
// the search page: marks and the one-line notice
const hit = (id, title, match, extra = {}) => ({
  rank: 1, score: 12.5, session: { source: "codex", session_id: id, title, client: "vscode", project: "demo", cwd: "/Users/me/demo",
    started_at: "2026-09-09T15:15:47Z", last_activity_at: "2026-09-09T16:02:10Z", source_status: "present", is_subagent: false, parent_session_id: null, message_count: 30 },
  resume: { available: true, command: `codex resume ${id}`, note: null }, paths: { mirror: "/m", original: "/o" },
  hit_messages: match === "semantic" ? 0 : 2, match, ...(match !== "keyword" ? { semantic_score: 0.72 } : {}),
  hits: match === "semantic" ? [] : [{ seq: 3, kind: "assistant", at: "2026-09-09T15:40:12Z", msg_key: null, snippet: "…休眠时仍有唤醒源，用 pmset -g log 查看…", highlights: [], show: "x" }], ...extra });
const search = (state, extra = {}) => ({ state: "ok", response: { query: "续航变短", tokens: ["续航", "变短"], mode_used: "hybrid", read_mode: "normal", data_as_of: "2026-10-16T09:00:00Z",
  matched_sessions: 3, returned: 3, notes: [], hint: null,
  results: [hit("019e0001", "笔记本电池掉电过快", "both"), hit("019e0002", "Mac 休眠耗电排查", "semantic"), hit("019e0003", "WindowServer 占用过高", "keyword")],
  semantic: { state, indexed: 18400, total: 24385, note: "" }, ...extra } });
for (const [name, state] of [["search-semantic", "used"], ["search-building", "index_building"], ["search-fallback", "model_missing"]]) {
  const page = await open(withSem(states["semantic-building"]) + `window.__SEARCH = ${JSON.stringify(search(state, state === "model_missing" ? { mode_used: "any", results: [hit("019e0003", "WindowServer 占用过高", "keyword")] } : {}))};`);
  const b = page.getByRole("button", { name: /Knowledge Base|知识库/ }).first();
  if (await b.count()) await b.click();
  await page.waitForTimeout(800);
  await page.getByPlaceholder(/搜索所有会话内容/).fill("续航变短");
  await page.waitForTimeout(1800);
  await page.screenshot({ path: `${outDir}/${name}.png` });
  await page.close();
}
await browser.close();
