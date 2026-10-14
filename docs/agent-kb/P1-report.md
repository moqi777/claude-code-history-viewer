# P1 完成报告：后台收集 + 原样备份 + 会话索引

分支 `agent-kb`（从 `main` @ `7f48907` 拉出），均已推送到 `origin/agent-kb`。设计见 `P1-design.md`（v2，已按审核意见修订）。

## 0. 结论一览

| 验收标准 | 结果 |
|---|---|
| 1 镜像与源文件逐字节一致 | ✅ 266/266 比对，唯一差异是当时正在写入的本会话文件，且镜像恰为其前缀 |
| 2 连续两次同步，第二次 0 字节、2 秒内 | ✅ 第二次 `copied_bytes=0`，release 0.02 s（debug 0.03 s）|
| 3 新开 Claude + Codex 会话，一个周期内出现 | ✅ 聊完后约 26 秒内两个会话都进入镜像和索引（见 §3.3）。**"App 里能看到"这半句未做 GUI 验证，见 §5** |
| 4 删除测试 | ✅ 后端与数据库层通过；**App 界面里的"已删除"标记未做 GUI 验证，见 §5** |
| 5 定时任务 | ✅ 安装、`RunAtLoad`、周期运行、`status`、卸载均验证。**"注销后重新登录补跑"我无法代做，需你实测一次**，见 §3.4 |
| 6 源目录没被动过 | ✅ 同步前后紧邻快照差异为空，见 §3.5 |
| 7 全部检查通过 | ✅ 含 clippy（底座原有的 2 处已按审核选 B 单独修复）|
| 8 报告与推送 | ✅ |

## 1. 改了哪些文件

**新增**（全在 `kb` 命名空间，约 5000 行 Rust + 前端）

- `src-tauri/src/kb/`：`mod.rs`、`config.rs`、`paths.rs`、`lock.rs`、`log.rs`、`db.rs`、`discover.rs`、`mirror.rs`、`index/{mod,claude,codex,codex_titles,text}.rs`、`sync.rs`、`status.rs`、`schedule.rs`、`cli.rs`、`commands.rs`（含 74 个单元测试）
- `src/kb/`：`types.ts`、`api.ts`、`useKbStore.ts`、`KbPanel.tsx`、`index.ts`、`fixtures.ts`、两个测试文件（15 个测试）
- `src/i18n/locales/{en,ko,ja,zh-CN,zh-TW}/kb.json`（新 namespace `kb`，各 43 个键；`en`、`zh-CN` 认真写，其余三种先填英文）
- `docs/agent-kb/P1-design.md`、`P1-report.md`

**修改底座文件**（合计 +65 / −10，不含 lockfile 与生成文件）

| 文件 | 改动 |
|---|---|
| `src-tauri/src/lib.rs` | +12：`pub mod kb;`、`--kb-*` 分发块、注册 5 个命令 |
| `src-tauri/src/providers/codex.rs` | +9/−1：`validate_session_path` 追加镜像根白名单；Codex 未安装时不再因 `get_sessions_dir()?` 提前报错而挡住镜像 |
| `src-tauri/src/providers/zcode.rs` | 2 行文档注释加反引号（clippy 基线，独立提交 `794ed00`，审核选 B）|
| `src-tauri/Cargo.toml` | +2：`fs4 = "1.1"`、`sha2 = "0.10"` |
| `src/layouts/Header/SettingDropdown/index.tsx` | +9：设置菜单里的"知识库"项（仅桌面端）|
| `src/layouts/AppLayout.tsx` | +2：挂载 `<KbPanel />` |
| `src/i18n/index.ts` | 注册 `kb` namespace（5 种语言）|
| `src/i18n/types.generated.ts` | 由 `pnpm run generate:i18n-types` 生成 |

没有新增 `AnalyticsView`：面板是一个自带 store 的对话框，所以不动底座 slice、类型、导航 hook 和移动端 tab bar。

**依赖**：`fs4` 最终版本 **1.1.0**（`rust-version = 1.75`，满足 MSRV 1.77.2）；`sha2 0.10`（已在依赖树里）。

**前端入口位置**：右上角"设置"（齿轮）下拉菜单 → "知识库 / Knowledge Base"（在 Claude 工具分组之后、"设置"标题之前）。

## 2. 审核意见逐条落实

| 审核项 | 怎么落实的 |
|---|---|
| clippy 选 B | 提交 `794ed00`，仅 `zcode.rs` 2 处注释。`dist/` 存在时 clippy 全绿（含此前被挡住的目标）|
| `.jsonl.zst` | 整文件复制（临时文件 + rename 原子落盘），元数据通过 `zstd` 流式解压后索引；有明文孪生文件时跳过 zst（沿用底座 `is_discoverable_rollout`）。测试 `compressed_files_are_copied_whole_and_indexed_decompressed`、`codex_sessions_archived_and_compressed_twins` |
| 归档后镜像留在原位 | `codex_moved_to_archive_is_one_session_not_deleted_and_not_duplicated` |
| 2.1 标题库打开 | **与审核写的顺序有一处偏离，见 §4.1**。最终策略：`-wal` 不存在 → `immutable=1`；存在 → `mode=ro`；打不开 → 回退 `session_index.jsonl` 并在摘要 `errors` 记 `stage="title_db"`，整次同步不失败。测试：`wal_database_without_wal_and_shm_files_is_still_readable`、`live_wal_database_is_read_through_its_wal`、`unreadable_database_falls_back_to_session_index_with_a_warning`、`unreadable_titles_db_degrades_to_a_warning_not_a_failed_sync`。底座 `load_native_title_index` 未改，见 §6 |
| 2.2 白名单两个根 | `codex_mirror_roots_canonical()` 返回 `raw/codex/sessions` 和 `raw/codex/archived_sessions`。测试 `mirrored_codex_sessions_open_from_both_mirror_roots`（含归档根，且 Codex 本身未安装）、`codex_session_paths_outside_the_mirror_and_codex_roots_are_still_rejected` |
| 2.3 先移动后删除 | `sync::reconcile()` 是纯函数：先配对移动，配不上的才算删除，整批在一个事务里。测试 `move_is_resolved_before_deletion_so_no_intermediate_deleted_state` |
| 2.4 锁重试 | 方案 a：每 100 ms 重试，最多 3 秒（`LOCK_WAIT`）。状态查询（`kb_get_status`）完全不碰锁。测试 `waiting_acquire_succeeds_once_the_holder_releases`、`concurrent_second_run_is_blocked_by_the_lock`（断言确实等满 ≥2 秒才跳过，且被跳过的运行不碰数据库）|
| 2.5 兜底标题 | `kb/index/text.rs::{is_injected_user_text, fallback_title}`，Claude、Codex 共用，第 3 期可直接复用。Codex 计数排除注入的 user 消息，Claude 排除 `isMeta`。测试在 `text.rs`、`codex.rs`、`claude.rs` |
| 2.6 launchd Label | `schedule::LAUNCHD_LABEL = "io.github.moqi777.agentkb.sync"`，plist 同名，与 `tauri.conf.json` 无关 |
| 3 Codex 父会话 | 先读 `payload.parent_thread_id`，再读 `source.subagent.thread_spawn.parent_thread_id`，`source` 为字符串时按客户端处理。测试 `subagent_parent_is_read_from_top_level_then_nested_source` |
| 4 注意事项 | 最长单行 27.9 MB（<100 MB，不用停下）；入口位置见 §1；`pnpm build` 写进了 §7 的验证命令 |

## 3. 验证与证据

环境：macOS，用户本机真实数据（Claude 39 个文件、Codex 227 个会话）。

### 3.1 首次全量同步（性能）

debug 构建，真实 `~/AgentKB`：

```
$ /usr/bin/time -l target/debug/claude-code-history-viewer --kb-sync
{"status":"ok","trigger":"cli","scanned_files":266,"changed_files":266,"copied_bytes":2946637454,
 "new_sessions":266,"errors":[],"bad_lines":0,"longest_line_bytes":27942607,"duration_ms":20104,...}
   25.58 real   144457728  maximum resident set size
```

release 构建，隔离 HOME（不碰真实库，冷启动）：

```
首次  : 9.08 s real, 峰值内存 111,001,600 B (≈106 MB), copied_bytes=2947005860, 镜像 2.7G
第二次: 0.02 s real, changed_files=0, copied_bytes=0, 峰值内存 ≈15 MB
第三次: 0.01 s real, changed_files=0, copied_bytes=0
```

内存峰值 <300 MB ✅；无变化 <2 秒 ✅。`unknown_types` 记录了 Claude/Codex 里 9 种未在已知列表中的记录类型（`atis-latch`、`token_usage_record` 等），只计数并写日志，没有报错。

### 3.2 逐字节比对

脚本把每个源文件与镜像 `cmp`（`scratch/cmpall.sh`，未入库）：

```
$ scratch/cmpall.sh
DIFF .../claude-code-history-viewer/ef228e77-....jsonl (2311887 vs 2301788)
compared=266 mismatched=1
```

唯一差异是执行本任务的这个会话自己的文件（比对时正在写入）。验证它是前缀关系：

```
$ head -c 1937904 <源> | cmp - <镜像>      → 无输出（一致）；镜像末尾字节 0a
```

### 3.3 新会话

在 `scratch/kb-newsession/` 里实际运行 `claude -p` 和 `codex exec` 各一次，聊完（12:53:54）后，定时任务在 12:54:20 收集：

```
claude|sdk-cli|kb-newsession|2|Reply with exactly the word: pong-claude|present
codex |exec   |kb-newsession|2|Reply with exactly the word: pong-codex-|present
```

### 3.4 定时任务

用临时 `config.json`（`interval_secs=60`，已删除还原）安装：

```
$ --kb-schedule install   → loaded:true, interval_secs:60, runs:1
日志: 12:51:20  12:52:20  12:53:20  12:54:20   各一条 "sync start trigger=schedule"
$ --kb-schedule status    → state:"not running", last_exit_code:0, runs:4
launchd.err.log 0 字节
$ --kb-schedule uninstall → installed:false, loaded:false；plist 已删除
```

`RunAtLoad`：安装当下立即有一次 `trigger=schedule` 的运行。

**未能验证**：注销后重新登录是否补跑。`RunAtLoad=true` 的语义已由安装时的立即运行证明，但真正的"重新登录"我无法代做，请你实测一次：`--kb-schedule install` → 注销登录 → 看 `~/AgentKB/logs/sync-*.log` 登录后是否有新的 `trigger=schedule` 记录。
测试结束后我已卸载，**没有给你留下常驻任务**。注意：plist 里的可执行文件路径是开发期路径（`scratch/release-target/release/...` 或 `src-tauri/target/debug/...`），正式打包后的路径与 `.app` 移动后失效问题按任务留给第 2 期。

### 3.5 源目录没被动过

整树快照对比里有大量 Agent 自己的正常写入（Codex 缓存、`.tmp`、正在运行的会话），区分度不够，所以改用"紧邻同步前后各取一次"：

```
snap > pre.txt;  --kb-sync;  snap > post.txt      （1445 个条目，排除 Codex 的 .tmp/plugins/cache/computer-use/logs）
diff pre.txt post.txt  →  空
~/.codex 下没有 state_5.sqlite-wal / -shm（现存的 -wal/-shm 只属于 Codex 自己的 logs_2、queue_1、thread_history_1）
```

### 3.6 删除测试

按任务要求：复制一个真实会话到 `~/.claude/projects/kb-test-26626/<uuid>.jsonl`，同步，删除这份**我自己复制出来的**文件，再同步：

```
同步1: new_sessions=1 → present|60571023-...|2
删除后同步2: deleted_sources=1 → deleted|deleted_at=2026-10-14T04:51:12Z
镜像仍在，cmp 与原模板逐字节一致；再同步一次：deleted_sources=0（不反复折腾）
```

测试数据（数据库行与镜像文件）事后已从真实 KB 清掉。没有碰任何真实会话文件。

### 3.7 全部检查

```
pnpm tsc --build .                                  exit 0
pnpm vitest run                                     130 files, 1407 tests passed   （基线 128 / 1392；+2 文件 +15 测试）
pnpm lint                                           exit 0
pnpm run i18n:validate                              ✅（仅有底座既有的 35 条"疑似未翻译"警告，kb 无新增）
cargo test -- --test-threads=1                      1185 passed / 0 failed / 1 ignored   （基线 1111；+74）
cargo clippy --all-targets --all-features -- -D warnings   通过
cargo fmt --all -- --check                          exit 0
```

任务要求的 10 个 Rust 场景与对应测试：新文件 `new_files_are_mirrored_and_indexed`；追加 `appends_and_half_lines_…`、`new_then_unchanged_then_append_…`；半行 `a_half_written_line_waits_for_the_next_run`；没有变化 `nothing_changed_copies_nothing_…`；重写 `rewritten_source_keeps_the_old_mirror_and_reindexes`；源删除 `deleted_source_keeps_its_mirror_…`；Codex 移归档 `codex_moved_to_archive_…`；崩溃恢复 `crash_after_mirror_append_before_commit_…`、`crash_between_mirror_append_and_cursor_update_…`；同时运行两次 `concurrent_second_run_is_blocked_by_the_lock`；三次同步字节一致 `three_syncs_end_with_every_mirror_identical_to_its_source`。

## 4. 偏离任务/审核的地方

### 4.1 标题库：`immutable` 与 `mode=ro` 的顺序反过来了（需要你知道）

审核 2.1 要求"先 `mode=ro`，CANTOPEN 且无 `-wal` 再 `immutable=1`"。实现时发现：在**随 rusqlite 打包的 SQLite** 里，对"无 `-wal`/`-shm` 的 WAL 库"用 `mode=ro` 打开**会成功，并且在库所在目录新建 `-shm` 和 `-wal` 文件**（我在 `state_5.sqlite` 的副本上复现了）。对真实的 `~/.codex` 这就是写源目录，违反"源目录只读"和验收 6。你复现的 CANTOPEN 只出现在系统自带的 `sqlite3` 命令行里，打包版不会失败，而是悄悄写文件。

所以我改成：**先看 `-wal` 是否存在**——不存在直接 `immutable=1`（零副作用）；存在（Codex 正在运行）才用 `mode=ro`（`-shm` 也在，不新建）。这仍遵守审核的约束"只有没有 `-wal` 才用 immutable"。测试断言读取后目录里没有新增 `-wal`/`-shm`，防止回归。

### 4.2 其他小偏离

- **触发来源**：plist 里不改 `ProgramArguments`（仍是 `exe --kb-sync`），改用环境变量 `AGENTKB_TRIGGER=schedule` 区分定时与手动，写进 `sync_runs.trigger`。
- **镜像残留处理**：某文件第一次复制中途崩溃、留下没有游标的镜像时，若它恰是源文件的前缀则原地重来，否则移成 `.prev-*`，保证绝不因为"没游标"而删掉任何可能独有的数据。
- **删除检测的安全阀**：只有"整个目录被完整列出"的来源才允许判定删除。源根不存在/不可读（比如盘没挂载）时，一个会话也不会被标成已删除。测试 `an_unreadable_source_root_never_declares_history_deleted`。
- **排除前缀**：被排除后已收集的会话不会因为"扫不到"而被标成已删除。
- **plain↔zst 互转**：同一会话从 `.jsonl.zst` 变成明文（或反之）时按"新文件"处理，旧的按删除处理，不做移动配对（移动只在压缩状态相同的情况下配对）。
- **Windows**：只做了 macOS 验证；`schedule` 在非 macOS 返回 `supported:false`。

## 5. 没能做到/没验证到的

1. **桌面界面的真机截图验证没有完成。** 我启动了 debug 版 App（需要手动起 vite：`just` 没装，`tauri dev` 的 `beforeDevCommand` 会失败），截图时 macOS 弹出了"Ghostty 请求屏幕录制权限"的系统弹窗。这是你的系统安全设置，我没有点"允许"，随后停止了 GUI 验证，并只按 PID 结束了我自己启动的 App 和 vite。所以：
   - 验收 3、4 里"App 里能看到 / 标记已删除"，目前证据是：后端 `kb_list_sessions` 的 SQL（数据库层已验证 `source_status` 和 `deleted_at`）+ 15 个前端测试（面板渲染"源文件已删除"徽章、点击经现有 `selectSession` 打开、失败时 toast、"收集中"按钮状态、筛选）。**没有真实渲染的截图。**
   - 需要你（或授权后我）手动看一眼：设置齿轮 → "知识库"。
   - 同样没做真实窗口里"点开一个 Codex 镜像会话能显示消息"的端到端点击；该链路由 `mirrored_codex_sessions_open_from_both_mirror_roots` 在后端层面覆盖。
2. 注销重登补跑，见 §3.4。
3. WebUI（`--serve`）模式：`kb_*` 命令没有加进 Axum 服务端，菜单项用 `DesktopOnly` 隐藏了入口。这是有意缩小范围，但 `tauri-axum-parity-checker` 若被运行，会把这 5 个命令报为"桌面有、WebUI 无"。

## 6. 遗留问题

- **底座 `providers/codex.rs::load_native_title_index` / `open_state_db`**：同样用 `SQLITE_OPEN_READ_ONLY` 非 URI 方式打开，按 §4.1 的发现，它在 Codex 没运行时会在 `~/.codex` 里创建 `-shm`/`-wal` 副文件（写源目录）。我按要求没改，建议单独开一个 issue/小改动。
- 单行最长 27.9 MB 已被流式读取容纳，但缓冲按"最长行"增长，没有上限保护（>100 MB 的行会吃那么多内存）。
- `launchd.out/err.log` 不轮转。
- plist 写死 `current_exe()`：移动 `.app` 或升级后失效（第 2 期）。
- `--kb-reindex`、全文搜索、整理、迁移均不在本期。
- 前端 `KbPanel` 每 5 秒轮询一次状态（只读，不碰锁），面板关闭即停。
- `KbSession.session_id` 用镜像绝对路径（与底座"session_id 基于文件路径"的约定一致）。
- Claude 子代理会话作为独立会话入库（`is_subagent=1`，`parent_session_id` 指向父会话），列表默认隐藏，可勾选"包含子代理会话"。

## 7. 复现验证用的命令

```bash
pnpm install
pnpm build                       # 先生成 dist/：cargo clippy --all-features 会在编译期嵌入 ../dist
pnpm tsc --build . && pnpm vitest run && pnpm lint && pnpm run i18n:validate
cd src-tauri
cargo test -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo build --release                                  # 可执行文件：target/release/claude-code-history-viewer

# 无界面：
target/release/claude-code-history-viewer --kb-sync                      # 一行 JSON 摘要
target/release/claude-code-history-viewer --kb-schedule install|status|uninstall
# 与真实数据隔离地测冷启动：
HOME=$(mktemp -d) CLAUDE_CONFIG_DIR=$HOME_REAL/.claude CODEX_HOME=$HOME_REAL/.codex \
  /usr/bin/time -l target/release/claude-code-history-viewer --kb-sync
```
