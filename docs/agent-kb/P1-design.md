# P1 设计说明：后台收集 + 原样备份 + 会话索引

> 状态：**待 leader 审核**（步骤 1 检查点）。审核通过前不写任何实现代码。
> 依据：`PLAN.md` 第 3、6 节；`P1-collect-and-backup.md` 步骤 1 的 7 个问题。
> 文中的"依据"都是我在本仓库（分支 `agent-kb`，基于 `main` @ `7f48907`，v1.32.0）实际读过的文件和函数。

## 0. 摘要与需要 leader 拍板的点

| # | 结论 |
|---|---|
| 1 展示备份 | **不让底座 provider 直接换根目录。** 会话列表由 `kb.db` 驱动（只有数据库知道"源文件已删除"）；打开会话时复用底座现有的消息加载链路，Claude 零改动，Codex 在路径校验处加约 10 行白名单 |
| 2 复用解析 | 底座的元数据提取函数都与底座自己的缓存、私有结构绑在一起，**整体不能复用**；只复用少量 `pub(crate)` 小函数。增量解析自己写，以"已消费字节偏移"为游标 |
| 3 无界面入口 | 在 `lib.rs::run` 里 `--export` 分发块之后、`--serve` 之前加一个 `--kb-*` 分发块，执行完 `process::exit`。single-instance 插件只在 `run_tauri()` 里注册，所以天然不会被拦截 |
| 4 并发锁 | 对 `<数据目录>/kb.lock` 加**内核文件锁**（flock 语义），进程死亡内核自动释放，因此不存在"残留锁"问题 |
| 5 数据库 | 4 张表（`meta`、`files`、`sessions`、`sync_runs`），用 `PRAGMA user_version` + 顺序迁移数组升级 |
| 6 改动清单 | 新增 `src-tauri/src/kb/` 与 `src/kb/`；底座文件只改 `lib.rs`、`codex.rs`、`multi_provider.rs`、`Cargo.toml` 等，合计约 40 行（前端另约 30 行接入） |
| 7 新依赖 | 仅 `fs4`（跨平台文件锁）和 `sha2`（`sha2` 已在 Cargo.lock 里，作为直接依赖不增加新的编译量）。**不用 `libc::flock`，因为 `Cargo.toml` 有 `unsafe_code = "deny"`** |

**需要你决定的 4 件事**（详见各节，我给了默认建议，不回复就按建议做）：

1. **Codex 的 `.jsonl.zst`**（第 2.4 节）：底座代码明确支持 Codex 把旧会话压缩成 `rollout-*.jsonl.zst`，任务文件的扫描范围只写了 `.jsonl`。你本机目前 0 个 `.zst`，但别的用户会有。建议 P1 就按"整文件复制、变了就整份覆盖"支持。
2. **Codex 移入归档目录后的镜像位置**（第 1.3 节）：镜像路径不变（留在 `raw/codex/sessions/...`），不会再出现 `raw/codex/archived_sessions/` 里的副本。这与"镜像保持和源目录结构一致"略有出入，请确认接受。
3. **`dirs::config_dir()` 换成 `crate::utils::config_dir()`**（第 3.3 节）：底座测试环境下直接调 `dirs::*` 会读到真实 HOME，所以底座封装了一层。生产环境两者等价，只是测试能沙箱化。
4. **Codex 标题数据库文件名**（第 2.3 节）：底座把 `state_5.sqlite` 写死。任务文件提醒版本号会变。我建议 P1 在 `kb` 里自己做"取 `~/.codex` 下版本号最大的 `state_*.sqlite`"，不去动底座那个常量。

---

## 1. 展示备份

### 1.1 底座怎么确定根目录

| 来源 | 依据 | 行为 |
|---|---|---|
| Claude 默认根 | `providers/claude.rs::get_base_path` | 固定 `home_dir()/.claude`；**不**读 `CLAUDE_CONFIG_DIR` |
| Claude 自定义根 | `commands/multi_provider.rs::scan_all_projects` 的 `custom_claude_paths` 参数；`utils.rs::validate_custom_claude_path` | 前端用户设置里的 `customClaudePaths`，要求绝对路径、非符号链接、含 `projects/` 子目录。`CLAUDE_CONFIG_DIR` 由前端 `projectSlice.ts::autoRegisterConfigDir` 通过 `detect_claude_config_dir` 自动登记为自定义根 |
| Claude 读消息 | `commands/session/load.rs::load_session_messages` | 只用 `chain::resolve_session_chain` + `load_all_messages_from_file`，**对路径没有根目录限制** |
| Codex 默认根 | `providers/codex.rs::get_base_path` | 先读环境变量 `CODEX_HOME`（目录存在才用），否则 `~/.codex` |
| Codex 扫描 | `codex.rs::scan_projects_from_path(base)` | 可传 base；但 `load_sessions` 内部调 `get_existing_session_dirs()` → `get_base_path()`，**不可传 base** |
| Codex 读消息 | `codex.rs::load_messages` → `validate_session_path` | 路径必须在 `<base>/sessions` 或 `<base>/archived_sessions` 内，否则报 `Session path is outside Codex session directories` |
| 前端打开会话 | `src/store/slices/messageSlice.ts::selectSession` → `load_provider_messages_paginated(provider, session_path)` | 完全由后端按 provider + 文件路径加载 |

### 1.2 为什么不直接让 provider 读镜像

方案 B（被否决）："把 `~/AgentKB/raw/claude` 登记为 `customClaudePaths`，Codex 设 `CODEX_HOME`"。

- 源文件还在时，同一会话在源和镜像里各出现一次，列表里重复；去重要改底座的扫描逻辑
- `CODEX_HOME` 是进程全局，改了就看不到真实的 Codex 了，且无法同时看源和镜像
- 底座的扫描只看文件系统，**天然不知道"源文件已被删除"**——验收标准 4 要求明显标记，这个信息只能来自 `kb.db`
- 镜像里没有 `state_5.sqlite`，Codex 标题会丢（底座从那里取标题）

### 1.3 方案 A（采用）：列表由 `kb.db` 驱动，打开时复用底座

1. **新增 Tauri 命令 `kb_list_sessions`**（在 `src-tauri/src/kb/commands.rs`）：从 `sessions` 表读，返回字段与底座 `ClaudeSession` 对齐，外加 `source_status`（`present` / `deleted`）。前端得到的对象可以直接喂给 `selectSession`。
   - `file_path` = 镜像文件的绝对路径；`provider` = `claude` / `codex`
2. **打开会话完全复用底座链路**，需要的改动：
   - Claude：**零改动**（`load_session_messages` 不限制根目录）。注意 `chain::resolve_session_chain` 会在同目录里找续接文件，镜像目录结构与源一致，所以行为也一致。
   - Codex：`codex.rs::validate_session_path` 现在只允许两个根。加一个来自 `kb` 的额外白名单，约 10 行：
     ```rust
     // validate_session_path 内，canonical_session_dirs 组装完后：
     canonical_session_dirs.extend(crate::kb::paths::codex_mirror_roots_canonical());
     // 并且把 "No Codex session directories found" 的判断放到 extend 之后
     ```
     `codex_mirror_roots_canonical()` 返回 `<数据目录>/raw/codex/sessions` 的规范化路径（目录不存在则空 Vec）。
   - `commands/multi_provider.rs::merge_tool_execution_messages` 是私有函数，只在底座入口里用；因为我们走底座入口，**不需要改它**。
3. **Codex 移入归档目录的镜像位置**：镜像文件始终留在最初被镜像的位置（`raw/codex/sessions/YYYY/MM/DD/...`），只更新 `sessions.source_path`。不会出现 `raw/codex/archived_sessions/` 的第二份副本。代价：镜像不是源目录的"精确镜像"，而是"按首次发现位置"。会话 id 才是身份。（决策点 2）
4. **前端入口**：`AnalyticsViewType` 里加一个视图 `"kb"`（`src/types/analytics.ts`），点击会话时构造一个合成的 `ClaudeSession` 调 `selectSession`（`messageSlice.ts` 已有合成会话的先例，见 1072–1079 行）。

**风险**：`selectSession` 会触发 `load_provider_messages_paginated`；Claude 路径会走底座的元数据缓存（`load.rs` 里 `IncrementalParseState` 相关缓存），镜像路径会各自产生缓存条目。这是无害的（缓存键是路径），但步骤 4 实现时我会实测一次确认不报错。

---

## 2. 复用解析

### 2.1 所需元数据

会话 id、cwd、标题、开始/结束时间、消息数（仅 user+assistant）、父会话 id、客户端。

### 2.2 Claude

| 能力 | 底座现成的 | 能否复用 |
|---|---|---|
| 整文件元数据提取 | `load.rs::extract_session_metadata_internal` + `IncrementalParseState` | **不能**。私有，返回 `SessionExtractionResult`（含 `ClaudeSession`、sidechain 计数、首末用户文本等 UI 缓存字段），签名和缓存耦合 |
| 标题取值 | `load.rs::try_extract_ai_title`（`pub(crate)`） | **能**，一行 trim 非空判断，复用它保证规则一致 |
| 项目名 | `load.rs::project_display_name_from_path`（私有） | 不能，自己写 3 行（取 cwd 最后一级） |
| 系统消息类型判断 | `is_system_message_type`（私有） | 不用，我们只数 `type == "user" \| "assistant"` |
| 行切分 | `utils.rs::find_line_ranges`（`pub`，基于 `memchr`） | **能**，但它会把无结尾换行的最后一行也返回；我们要"只处理到最后一个换行符"，所以只在**已确认完整的字节区间**上调用它 |

**需要新写**（`kb/index/claude.rs`，约 120 行）：对"新追加的完整行"做轻量解析，只反序列化需要的字段（`type`、`sessionId`、`timestamp`、`cwd`、`entrypoint`、`aiTitle`、`isSidechain`、`message.content` 用于首条用户消息兜底标题）。

- 会话 id：文件名（`<uuid>.jsonl`）为准；子代理文件 `<sessionId>/subagents/*.jsonl` 的父会话 id = 路径里 `<sessionId>` 那一级，其自身 id = 文件名
- 消息数：`type` 为 `user`/`assistant` 且**非 `isMeta`** 的行数。不过滤 sidechain，子代理文件本来就是独立会话
- 标题：最新一条 `ai-title`（`aiTitle` 非空）> 首条用户消息前 50 字
- 未知 `type`：计数进 `unknown_types: HashMap<String, u64>`，同步结束写日志
- 编码健壮性：单行 JSON 解析失败 → 跳过该行并计入 `bad_lines`，**不报错**，镜像照常追加

### 2.3 Codex

| 能力 | 底座现成的 | 能否复用 |
|---|---|---|
| 文件判断 | `codex.rs::is_rollout_jsonl`、`is_discoverable_rollout`（`pub(crate)`） | **能**，直接用，保证和底座对"什么算 rollout"一致（含 `.zst` 去重规则） |
| 文件名取 id | `session_id_from_rollout_filename`（`pub(crate)`） | **能** |
| 整文件元数据 | `extract_session_info`（`pub(crate)`）：返回 id、cwd、消息数、首末时间、摘要 | **能，但只能用于"整文件重算"**。它会 mmap 整个文件，适合新文件/被重写的文件；追加场景不能用它做增量（见 2.4） |
| 首个 `session_meta` 规则 | 注释里的 issue #451（fork 会回放源会话的 session_meta，**必须取第一个**） | **必须遵守**，增量解析里同样只取首个 |
| 标题 | `load_native_title_index(base)`（私有）读 `state_5.sqlite` 的 `threads`，并丢掉 `title == first_user_message` 的行 | 不能直接调。我自己写同样的只读查询（`SQLITE_OPEN_READ_ONLY`），但**不丢** `title == first_user_message` 的行（任务规定标题取 `threads.title`，取不到再回退） |
| 标题库文件名 | `STATE_DB_FILENAME = "state_5.sqlite"` 写死 | 不复用。自己在 `~/.codex` 下找版本号最大的 `state_*.sqlite`（决策点 4） |
| 父会话 | 底座没有 | 新写：`session_meta.payload.source` 若是 JSON 对象且含 `parent_thread_id` / `subagent.thread_spawn.parent_thread_id` 则为父会话 id。**我会先在你本机真实数据上确认确切字段路径**（任务文件只说"一段 JSON，带 `parent_thread_id`"）|
| 客户端 | 底座没有 | 新写：`originator`（`codex-tui` → cli，`Codex Desktop` → desktop）与 `source`（`cli` / `vscode`）组合；未识别的原样存字符串 |
| 标题兜底 | `session_index.jsonl`（`{id, thread_name, updated_at}`） | 新写，约 15 行；同一 id 多行取 `updated_at` 最大的 |

**消息数口径**：任务要求"只统计 user 和 assistant"。底座 `extract_session_info` 的 `message_count` 还把 function_call 等也数进去了，**口径不同**，所以不直接用它的 `message_count`。我自己数：`response_item` 且 `payload.type == "message"` 且 `role ∈ {user, assistant}`。其余字段（cwd、id、首末时间）按底座规则。

### 2.4 增量解析怎么做

核心：**每个文件在 `files` 表里存 `mirror_offset`（已镜像并已解析的字节数）**，游标语义 = "已消费的、以换行符结尾的完整字节数"。

1. 读 `[offset, 最后一个 \n)` 这一段到内存缓冲（流式分块，每块上限 8 MiB，块边界也对齐到换行符，所以单块内存有上限；单行超 8 MiB 时扩大缓冲，单行最长实测需确认，见下"风险"）
2. 对这一段：①追加到镜像并 fsync ②用 `find_line_ranges` 切行，逐行喂给对应的 `ClaudeIndexer` / `CodexIndexer`（它们是带状态的累加器：`message_count`、`first_ts`、`last_ts`、`title`、`cwd`、`session_id`、`meta_seen`…）
3. 累加器的状态**序列化存进 `sessions` 表**（`indexer_state` JSON 列，只有少量标量，不存消息内容），下次从偏移处恢复，不必重读文件头
4. 事务：同一个 SQLite 事务里更新 `files.mirror_offset` 和 `sessions.*`

**被重写 / 变小**：丢弃累加器状态，从 0 重新解析整个新文件（镜像整份重拷，见 3.2 的规则）。
**`.jsonl.zst`**：无法追加，也无法增量解析：变了就整文件复制，元数据用 `zstd::decode_all` 流式解压后走和 `.jsonl` 同样的累加器。对 zst 文件跳过"尾部半行"逻辑（压缩文件一定是完整的）。

**风险（会在实现时实测）**：Claude 会话里单行可能很大（带图片 base64 的行可达数十 MB）。解决：缓冲按需增长而不是固定上限，但单文件峰值内存会 = 最长一行。首次全量同步时我会记录最长行并报告；如果超过 100 MB 再提方案。

---

## 3. 无界面入口

### 3.1 分发位置

依据：`lib.rs::run`（第 85–110 行）当前结构：

```
run():
  ① --export 块  → std::process::exit(export::run_export(&args))
  ② --serve 块   → run_server(&args); return          (feature = "webui-server")
  ③ run_tauri()
```

新增块放在 ① 和 ② 之间：

```rust
// Headless knowledge-base commands (`--kb-sync`, `--kb-schedule …`).
// Like `--export`, handled before any GUI/webview so it works from launchd.
if let Some(code) = kb::cli::try_run(&std::env::args().collect::<Vec<_>>()) {
    std::process::exit(code);
}
```

`kb::cli::try_run` 返回 `Option<i32>`：第一个参数（`args[1]`）不是 `--kb-` 开头就返回 `None`，让后面的流程照常走。**只看 `args[1]`，不像 `--export` 那样 `any()` 扫全部参数**，避免用户 cwd 或 `--session` 的值里碰巧包含 `--kb-` 字样被误触发。

### 3.2 为什么不会启动窗口、不会被 single-instance 拦截

- `tauri_plugin_single_instance::init` 只在 `run_tauri()` 里（`lib.rs` 第 155 行）`.plugin(...)` 注册。`std::process::exit` 在 `run_tauri()` 之前，**该插件根本没有被初始化**，所以无界面进程既不会被拦，也不会拦别人
- 反过来：GUI 在运行时，launchd 拉起的 `--kb-sync` 是另一个独立进程，不触碰 GUI 的 single-instance 套接字
- `main.rs` 只是 `claude_code_history_viewer_lib::run()`，没有别的初始化
- 日志：**不能用 `tauri-plugin-log`**（那是 builder 里的插件）。无界面模式自己写文件日志（`kb::log`），`log::` 宏在无界面里不输出（没有 logger），我不依赖它

### 3.3 本期的命令集

| 命令 | 作用 | 退出码 |
|---|---|---|
| `--kb-sync` | 一次增量收集，stdout 输出一行 JSON 摘要 | 0 成功（含"被锁挡住、跳过"）；1 有错误项；2 参数错误 |
| `--kb-schedule install\|uninstall\|status` | 管理 launchd 任务，stdout 输出 JSON | 同上 |

被锁挡住时输出 `{"status":"skipped","reason":"locked",...}` 且**退出码 0**——否则 launchd 会把它当失败记录。

### 3.4 配置读取

`config.json` 在 `crate::utils::config_dir()/agent-kb/`（生产等同 `dirs::config_dir()`，测试沙箱化，见决策点 3）。macOS 上就是 `~/Library/Application Support/agent-kb/config.json`。P1 只**读**（缺失就用默认值：数据目录 `~/AgentKB`、间隔 600 秒、排除前缀空）；`install` 子命令会把生效配置写一份。读源目录根的逻辑：Claude 沿用 `providers::claude::get_base_path()`，另外**额外**检查环境变量 `CLAUDE_CONFIG_DIR`（底座后端只通过前端登记使用它，无界面模式没有前端，所以必须自己读）；Codex 沿用 `providers::codex::get_base_path()`（已含 `CODEX_HOME`）。

> 注意：launchd 拉起的进程**不继承你 shell 里的环境变量**。如果你的 `CLAUDE_CONFIG_DIR` / `CODEX_HOME` 只设在 `.zshrc` 里，定时任务读不到。P1 的处理：`install` 时把当前进程里这两个变量（若有）写进 plist 的 `EnvironmentVariables`。你本机目前两个都没设（我查过），所以验收不受影响。

---

## 4. 并发锁

**方案**：`<数据目录>/kb.lock`，用 `fs4` 的 `try_lock_exclusive()`（macOS/Linux 底层是 `flock(LOCK_EX | LOCK_NB)`，Windows 是 `LockFileEx`）。

- 拿到锁 → 在文件里写 `pid` + 起始时间（仅供诊断显示"谁在收集"）→ 同步 → 文件句柄 drop 时锁释放
- 拿不到 → 立即返回 `Locked`，不等待
- **崩溃后的残留锁**：内核在持有进程死亡（含 `kill -9`、断电后重启）时自动释放 flock。所以**没有"残留锁"这个问题，也不需要 pid 探活或超时清理**。锁文件本身可以一直留在磁盘上，里面的旧 pid 只是诊断信息，不参与判断
- 同进程内（GUI"立即收集" vs 同一进程里别的线程）：flock 是按"打开文件描述"算的，同一进程里**各自 `open` 一次**的两个句柄也会互斥，所以测试"同时运行两次"可以在单进程里用两个线程验证，不需要真起两个进程
- NFS 等网络盘上 flock 可能不可靠：数据目录若在网络盘，文档里提示（P2 配置页处理），P1 不处理

**GUI 的"立即收集"**：`kb_sync_now` 命令用 `spawn_blocking` 调同一个 `kb::sync::run_once()`；`Locked` 时返回 `{"status":"locked"}`，前端显示"收集中"。前端另有 `kb_sync_status` 命令（只读，轮询 `try_lock` 探测锁是否被占 + 读 `sync_runs` 最后一行），避免按钮状态只靠"我自己点过"。

**数据库并发**：持锁者独占写；GUI 的列表/状态查询用只读连接，WAL 模式下读不被写阻塞；统一设 `busy_timeout = 5000`。

---

## 5. 数据库

文件：`<数据目录>/kb.db`，打开时执行 `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;`。

> `synchronous=NORMAL` 在 WAL 下断电可能丢最后几个事务，但**镜像文件本身 fsync 过**，且游标比镜像"短"只会触发"重新追加"，不会丢数据——这正是"先写镜像、后更新游标"顺序的目的。数据库丢失的最坏情况 = 重新同步（`--kb-reindex` 是 P3 之后的事，P1 提供不了，见"遗留"）。

### 5.1 表结构草案

```sql
-- 键值：schema 版本之外的杂项
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- 每个被镜像的源文件一行（游标）
CREATE TABLE files (
  id             INTEGER PRIMARY KEY,
  source         TEXT NOT NULL CHECK (source IN ('claude','codex')),
  source_path    TEXT NOT NULL UNIQUE,       -- 源文件当前绝对路径
  mirror_rel     TEXT NOT NULL UNIQUE,       -- 相对 raw/ 的镜像路径，如 claude/projects/<dir>/<id>.jsonl
  src_size       INTEGER NOT NULL,           -- 上次看到的源大小
  src_mtime_ns   INTEGER NOT NULL,           -- 上次看到的源修改时间
  head_hash      BLOB    NOT NULL,           -- 开头 4KB 的 SHA-256（不足 4KB 就是实际长度）
  head_len       INTEGER NOT NULL,           -- 参与哈希的字节数（<=4096）
  mirror_offset  INTEGER NOT NULL,           -- 游标：已镜像且已索引的字节数（以换行结尾）
  is_compressed  INTEGER NOT NULL DEFAULT 0, -- .jsonl.zst
  source_status  TEXT NOT NULL DEFAULT 'present' CHECK (source_status IN ('present','deleted')),
  deleted_at     TEXT,                       -- 首次发现源消失的时间（RFC3339）
  first_seen_at  TEXT NOT NULL,
  last_synced_at TEXT NOT NULL
);

-- 会话索引（一个文件一个会话；子代理文件也是独立会话，用 parent_session_id 关联）
CREATE TABLE sessions (
  id                 INTEGER PRIMARY KEY,
  file_id            INTEGER NOT NULL UNIQUE REFERENCES files(id) ON DELETE CASCADE,
  source             TEXT NOT NULL,
  session_id         TEXT NOT NULL,          -- Claude: 文件名 uuid；Codex: session_meta.id
  client             TEXT,                   -- cli / vscode / desktop / 其他原样字符串
  cwd                TEXT,
  project_name       TEXT,                   -- cwd 最后一级目录名
  title              TEXT,
  title_source       TEXT,                   -- ai-title / threads / session_index / first_message
  started_at         TEXT,                   -- RFC3339
  last_activity_at   TEXT,                   -- RFC3339
  message_count      INTEGER NOT NULL DEFAULT 0,
  parent_session_id  TEXT,
  is_subagent        INTEGER NOT NULL DEFAULT 0,
  indexer_state      TEXT NOT NULL DEFAULT '{}', -- 增量累加器的 JSON（见 2.4）
  unknown_types      TEXT NOT NULL DEFAULT '{}'  -- {"type名": 次数}
);
CREATE INDEX idx_sessions_session_id ON sessions(source, session_id);
CREATE INDEX idx_sessions_activity   ON sessions(last_activity_at DESC);

-- 每次同步一行，给状态面板和第 2 期测试按钮用
CREATE TABLE sync_runs (
  id            INTEGER PRIMARY KEY,
  started_at    TEXT NOT NULL,
  finished_at   TEXT,
  trigger       TEXT NOT NULL,                -- schedule / cli / gui
  summary_json  TEXT,                         -- 与 stdout 摘要同一份 JSON
  ok            INTEGER
);
```

**设计要点**

- **会话 id 在 Codex 里是身份**：移入归档目录时，`sync` 先按 `source_path` 查 `files`，没命中再按"文件名里的 id（`session_id_from_rollout_filename`）"查 `sessions`；命中则只更新 `files.source_path`，镜像不动。这也是 `source_path UNIQUE` 之外还要 `idx_sessions_session_id` 的原因
- **Claude 的同一 `session_id` 可能出现在多个文件**（resume/fork 复制旧消息）：`sessions` 不对 `(source, session_id)` 设唯一，一个文件一行；去重留给第 3 期消息表
- **`sync_runs` 保留 500 行**，超出按 id 删最旧
- **镜像目录里保留的 `.prev-<时间戳>` 旧版本**（被重写时产生）：不入 `files` 表，只靠文件名约定识别，并且不以 `.jsonl` 结尾，所以不会被扫描

### 5.2 升级方式

`PRAGMA user_version` 存当前版本；代码里有一个有序数组 `MIGRATIONS: &[&str]`（第 N 项把版本 N-1 升到 N），打开数据库时在**一个事务**里从当前版本依次执行到最新，然后设置 `user_version`。

- 只加不改：P1 就是版本 1（上面全部建表）。后续期往数组末尾追加语句，不改已发布的项
- **库版本比程序新**（用户回退了 App）：拒绝打开，返回清晰错误，不尝试降级
- 数据库可以丢：唯一信息源是 `raw/`（PLAN 第 3 节第 5 条）。因此表里不放任何"只存在于库里的用户数据"。P4 的笔记会放别的地方或单独设计，不在 P1 范围

---

## 6. 改动清单

### 6.1 新增（全部在 `kb` 命名空间下）

| 文件 | 内容 | 预估行数 |
|---|---|---|
| `src-tauri/src/kb/mod.rs` | 模块声明、`pub use` | 20 |
| `src-tauri/src/kb/config.rs` | 读 `config.json`、默认值、`CLAUDE_CONFIG_DIR` 处理 | 120 |
| `src-tauri/src/kb/paths.rs` | 数据目录、`raw/`、`logs/`、镜像路径推导、`codex_mirror_roots_canonical()`、路径安全校验 | 120 |
| `src-tauri/src/kb/lock.rs` | `kb.lock` 获取与诊断信息 | 80 |
| `src-tauri/src/kb/db.rs` | 打开、PRAGMA、`MIGRATIONS`、查询封装 | 250 |
| `src-tauri/src/kb/discover.rs` | 枚举 Claude/Codex 源文件，排除前缀，符号链接处理 | 160 |
| `src-tauri/src/kb/mirror.rs` | 增量追加、重写检测、`.prev-` 改名、崩溃恢复（截断到游标）、fsync | 300 |
| `src-tauri/src/kb/index/{mod,claude,codex}.rs` | 增量累加器 | 450 |
| `src-tauri/src/kb/sync.rs` | 编排：加锁→扫描→逐文件处理→删除/移动检测→摘要 | 250 |
| `src-tauri/src/kb/log.rs` | 文件日志、14 天清理 | 80 |
| `src-tauri/src/kb/schedule.rs` | plist 生成、`launchctl bootstrap/bootout/print` 调用 | 200 |
| `src-tauri/src/kb/cli.rs` | `try_run` 与子命令分发 | 100 |
| `src-tauri/src/kb/commands.rs` | Tauri 命令：`kb_get_status`、`kb_sync_now`、`kb_list_sessions`、`kb_schedule_*` | 150 |
| 测试 | `kb/*` 内 `#[cfg(test)]`，覆盖任务文件列出的 10 个场景 | 600 |
| `src/kb/**` | `KbPanel.tsx`、`useKbStore`（独立小 store）、`api.ts`、`types.ts`、测试 | 450 |
| `src/i18n/locales/*/kb.json` ×5 | 新 namespace `kb`，key 前缀 `kb.` | 5×~30 键 |

### 6.2 修改底座文件（最小接入）

| 文件 | 改动 | 行数 |
|---|---|---|
| `src-tauri/src/lib.rs` | `pub mod kb;`；`run()` 里加分发块；`invoke_handler` 里注册 5 个 `kb_*` 命令 | **约 12** |
| `src-tauri/src/providers/codex.rs` | `validate_session_path` 追加镜像根白名单（见 1.3） | **约 6** |
| `src-tauri/Cargo.toml` | 加 `fs4`、`sha2`（见第 7 节） | **2** |
| `src/types/analytics.ts` | `AnalyticsView` 联合类型加 `"kb"` | **1** |
| `src/store/slices/analyticsSlice.ts` | 加 `switchToKb`（与 `switchToBoard` 等并列） | **约 8** |
| `src/App.tsx`（或其渲染视图的容器，实现时确认确切位置） | `currentView === "kb"` 时渲染 `<KbPanel/>` | **约 6** |
| 导航入口（侧栏/工具栏，实现时确认） | 加一个"知识库"按钮 | **约 6** |
| `src/i18n/index.ts` | 注册新 namespace `kb` | **约 6** |
| `src/i18n/types.generated.ts` | 由 `pnpm run generate:i18n-types` 生成，不手改 | 生成 |

**合计手改的底座行数约 50（含前端），Rust 底座文件只动 `lib.rs`、`codex.rs`、`Cargo.toml` 三个。** 其余全在 `kb/` 下，合并上游更新时冲突面很小。

> `src-tauri/CLAUDE.md` 里若有后端规则（底座 CLAUDE.md 说它在冲突时优先），我会在写代码前读一遍并在报告中说明遵守情况。

### 6.3 不改的

`providers/claude.rs`、`commands/session/*`、`commands/multi_provider.rs`、`commands/archive.rs`、`commands/watcher.rs` 一概不动。底座已有的"手动归档"（`archive.rs`，复制到 `~/.claude-history-viewer/archives/`）与我们的镜像**互不相干**，不共用也不冲突。

---

## 7. 新依赖

| crate | 版本 | 为什么 | 备选与否决理由 |
|---|---|---|---|
| `fs4` | `0.13`（实现时确认能满足 `rust-version = "1.77.2"` 的版本，必要时降） | 跨平台安全封装的文件锁（`try_lock_exclusive`） | `libc::flock`：`Cargo.toml` 设了 `unsafe_code = "deny"`，要为它加 `#[allow(unsafe_code)]`，不值得。`std::fs::File::try_lock`：1.89 才稳定，而 `rust-version` 是 1.77.2，会破坏 MSRV。`fd-lock`：也可以，但 `fs4` 的 API 更直接 |
| `sha2` | `0.10`（与 Cargo.lock 现有 0.10.9 一致） | 开头 4KB 的哈希，判断文件是否被重写 | 已在依赖树里（Tauri 的传递依赖），**升为直接依赖不增加编译量**。不用 `std::hash`：它不保证稳定，存进数据库的哈希必须跨版本一致 |

**不加**：`plist`（Cargo.lock 里有，但只是传递依赖；我们的 plist 是固定模板，手写 XML + 转义 5 个字符即可，约 30 行）；`sysinfo`（内存峰值用 `/usr/bin/time -l` 在验证时测，不进产品代码）；`flate2`/压缩（本期不压缩）；任何联网相关 crate。

`rusqlite`（bundled）、`zstd`、`walkdir`、`chrono`、`memchr`、`dirs`、`serde_json`、`simd-json` 底座已有，直接用。

---

## 8. 本期补充的几个设计细节

这些任务文件没有逐条要求，但会影响实现，在这里先说明。

### 8.1 镜像路径推导

- Claude：`<源>/projects/<相对路径>` → `raw/claude/projects/<相对路径>`
- Codex：`<源>/sessions/<相对路径>` → `raw/codex/sessions/<相对路径>`；`<源>/archived_sessions/<相对路径>` → `raw/codex/archived_sessions/<相对路径>`（**仅当该会话 id 首次就是在归档目录发现时**；否则见 1.3）
- 安全：相对路径取自 `walkdir` 的条目，不来自文件内容。写镜像前对目标路径做"必须在 `raw/` 之内"的 `starts_with` 检查（规范化父目录后比较），拒绝含 `..` 的相对路径
- `mirror_rel` 有 `UNIQUE`，两个不同的源不会落到同一个镜像文件

### 8.2 写镜像的流程（单文件）

```
1. stat 源 → (size, mtime)；与 files 行比较：都没变 → 跳过（不读内容）
2. 读源开头 min(4096, size) 字节算 SHA-256，与 head_hash/head_len 比较
3. 分支：
   a. 新文件                         → 镜像 = 空，offset = 0，走 c
   b. size < mirror_offset 或 hash 变 → 旧镜像改名 <name>.prev-<UTC时间戳>；offset = 0；走 c
   c. 追加：
      - 镜像文件当前长度 L；若 L > offset → 先 truncate 到 offset（崩溃恢复）
      - 若 L < offset → 镜像被外部破坏，视同 b（改名旧镜像，重来）并写错误项
      - 从 offset 读到最后一个 \n 为止，append → fsync(文件)；新建目录时 fsync(父目录)
      - 同一事务：更新 files.mirror_offset/src_size/src_mtime/head_hash，更新 sessions
4. 源文件消失 → files.source_status='deleted'，deleted_at=now，镜像不动
5. 源文件重新出现（同路径）→ 回到 'present'，按 2/3 正常处理
```

- **"源文件消失"的检测**：本次扫描没枚举到的 `files` 行，且 `source_status='present'`。**前提**：只有当该来源的根目录本身可读时才做这个判断（根目录暂时不可读、被卸载、权限问题都**不能**把所有会话标成已删除）。对根目录不存在/不可读的来源，本次跳过删除检测并记错误项
- **mtime 精度**：用纳秒；APFS 支持。size 与 mtime 都没变就跳过——代价是同大小、同 mtime 的改写会被漏掉，这在 append-only 的 JSONL 里不会发生
- **正在写入的文件**：Agent 可能正在往源文件里写。读到"最后一个换行符"为止，半行留给下次；我们读的时候文件还在增长也没关系，因为只取读取时刻已存在且完整的行
- **Windows 路径**：P1 只做 macOS 验证，但路径拼接一律用 `Path::join`，相对路径分隔符存库时统一成 `/`（检查清单"跨平台"一条）

### 8.3 摘要 JSON（stdout 一行）

```json
{"status":"ok","trigger":"schedule","scanned_files":264,"changed_files":3,"copied_bytes":18234,
 "new_sessions":1,"deleted_sources":0,"moved_sources":0,"rewritten_files":0,
 "errors":[],"unknown_types":{"foo":2},"duration_ms":412}
```

`errors` 是 `[{"path":"...","stage":"mirror|index|scan","message":"..."}]`；单个文件出错**不中断**整次同步，该文件游标不推进，下次重试。

### 8.4 launchd

- Label：`com.claude.history-viewer.kb-sync`（沿用 `tauri.conf.json` 的 `identifier = com.claude.history-viewer`；任务文件写的是 `<应用标识>.kb-sync.plist`）。**注意**：这是底座上游的 identifier；如果你以后要给二开 App 换标识符，这个 Label 会跟着变，已安装的任务需要重装。P2 处理
- 路径：`~/Library/LaunchAgents/com.claude.history-viewer.kb-sync.plist`
- 写入用"临时文件 + rename"原子写
- 加载：`launchctl bootstrap gui/$(id -u) <plist>`；卸载：`launchctl bootout gui/$(id -u)/<label>`；`status`：`launchctl print gui/$(id -u)/<label>`，解析 `state`、`last exit code`；再结合 `sync_runs` 最后一行给出"上次运行时间和结果"
- `install` 对已存在的任务**先 bootout 再 bootstrap**（幂等，间隔变更也能生效）
- 可执行文件路径：`std::env::current_exe()`。开发期是 `src-tauri/target/debug/claude-code-history-viewer`，**在报告里注明**；打包后的 `.app` 路径与 `.app` 被移动/更新后的失效问题留给第 2 期
- 测试策略：plist 内容生成做纯函数单测；`launchctl` 调用封装成 trait，单测用假实现；真机验证在步骤 3 做

### 8.5 前端 `useKbStore`

不往底座 `useAppStore` 里塞新 slice（避免改它的类型和合并冲突）；新建独立的小 zustand store，放在 `src/kb/`。与底座的耦合点只有一个：点击会话时调用底座 `selectSession`。

### 8.6 日志

`<数据目录>/logs/sync-YYYY-MM-DD.log`，每次同步追加：开始、每个出错文件、未知类型汇总、结束摘要。启动时删除 `logs/` 下 mtime 超 14 天的 `sync-*.log`。launchd 的标准输出/错误另写 `logs/launchd.out.log`、`launchd.err.log`（不轮转；P1 先接受，体量很小，遗留项里记一笔）。

---

## 9. 底座基线（步骤 0 记录）

> 在分支 `agent-kb`（= `main` @ `7f48907` + 1 个 docs 提交）上执行，`pnpm install` 已完成。

| 检查 | 结果 |
|---|---|
| `pnpm tsc --build .` | ✅ 通过 |
| `pnpm vitest run` | ✅ 128 个文件、1392 个测试全部通过（输出里有一段 `listOnTimeout` 的堆栈打印，是测试里模拟的错误日志，不影响结果） |
| `pnpm lint` | ✅ 通过（0 error） |
| `pnpm run i18n:validate` | ✅ 通过；有 ko/zh-CN/zh-TW 各 35 个"疑似未翻译"的**警告**（`common.provider.*` 等，底座既有） |
| `cargo test -- --test-threads=1` | ✅ 通过：主 lib 1111 passed / 0 failed / 1 ignored，其余 4 个测试二进制共 18 passed、6 ignored |
| `cargo clippy --all-targets --all-features -- -D warnings` | ❌ **底座原有失败，2 处**（见下） |
| `cargo fmt --all -- --check` | ✅ 通过 |

**底座原有的失败项（不是我们引入的，也不会由我们修）**

`cargo clippy --all-targets --all-features -- -D warnings` 在 rust 1.97.0 下报 2 个 `clippy::doc_markdown`，都在 `src-tauri/src/providers/zcode.rs`：

- 第 3 行：`//! Z Code keeps the authoritative transcript history in a single SQLite store`（`SQLite` 缺反引号）
- 第 542 行：`/// safe for a `LIKE` prefilter: SQLite's `LIKE` is case-insensitive for ASCII`（同上）

这是新版 clippy 的新增检查，上游 CI 用的工具链可能较旧。后果：lib 编译不过 clippy，**依赖 lib 的其余目标（bin、集成测试）被一并挡住，其真实状态未被检出**，所以"clippy 全绿"这条在 P1 里最多做到"相对基线无新增"。

处理建议（请你定）：
- A. 不碰，P1 验收时用"新增 0 条 clippy 告警"替代（我会对比 `kb/` 下的告警数，并在本地临时给 `zcode.rs` 打补丁验证后还原，以便看到被挡住的目标）
- B. 单独开一个只改这两处注释的小提交（改动 2 行，纯文档注释），让 clippy 在我们的分支上能全绿。这会多一个和上游潜在冲突的点，但上游大概率也会很快修

**两个环境注意事项（也是基线发现）**

1. `--all-features` 会启用 `webui-server`，它在**编译期**嵌入 `../dist`（`src/server/mod.rs` 的 `#[derive(RustEmbed)]`）。`dist/` 不存在时 clippy 会先报 3 个与代码无关的错。必须先 `pnpm build`（`dist/` 已被 gitignore）。我已构建过，后续检查前要保证 `dist/` 在
2. `just` 未安装，全部直接用 pnpm / cargo 命令（与 PLAN 一致）

---

## 10. 实现顺序（审核通过后）

按任务文件步骤 2→5，每步一个提交：

1. `kb::paths/config/db/lock` + 迁移 + 测试（地基）
2. `discover` + `mirror`（含崩溃恢复）+ 测试
3. `index::{claude,codex}` + 增量累加器 + 测试
4. `sync` 编排 + `cli::try_run` + `lib.rs` 接入 + 10 个场景测试
5. `schedule` + `--kb-schedule`
6. Tauri 命令 + 前端面板 + i18n + `codex.rs` 白名单
7. 真实数据验证 → `P1-report.md`

## 11. 遗留 / 本期明确不处理

- `--kb-reindex`（从 `raw/` 重建数据库）：PLAN 第 3 节第 5 条提到，P1 任务文件未列入。P1 的 `sessions.indexer_state` 设计已保证可重建，但命令本身留到后续期。**如果你希望 P1 就做，请告诉我**
- 镜像孤儿清理：`.prev-*` 旧版本永远保留（原样备份原则），P1 不清理
- launchd 日志不轮转
- 数据目录在网络盘时 flock 的可靠性
- 打包后 `.app` 路径与标识符问题（P2）

## 12. 需要你确认的清单（回复时可直接写"同意"或指出修改）

- [ ] **clippy 基线（第 9 节）**：选 A 还是 B
- [ ] **决策点 1**：P1 就支持 Codex `.jsonl.zst`（整文件复制）
- [ ] **决策点 2**：Codex 会话被归档后，镜像留在原位置，不新增 `archived_sessions` 副本
- [ ] **决策点 3**：配置/HOME 走 `crate::utils::{home_dir,config_dir}`
- [ ] **决策点 4**：自己找最新的 `state_*.sqlite`，不动底座常量
- [ ] 展示方案采用 A（`kb.db` 驱动列表 + `codex.rs` 加白名单，Claude 零改动）
- [ ] 引入 `fs4`、`sha2` 两个直接依赖
- [ ] `--kb-reindex` 不进 P1
