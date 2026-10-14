# P1 设计审核意见（leader）

**结论：有条件通过。** 按下面的修改落实后，可以直接进入实现，不用再回来审一次设计。第 2 节的修改要在 `P1-design.md` 里同步更新，并在 `P1-report.md` 里逐条说明是怎么落实的。

## 1. 第 12 节清单的决定

| 项 | 决定 |
|---|---|
| clippy 基线 | **选 B**。单独一个提交，只改 `zcode.rs` 的 2 处文档注释。这样 P1 的门槛就是"clippy 全绿"，不再用"新增 0 条"来代替 |
| 决策点 1：Codex `.jsonl.zst` | 同意，整文件复制 |
| 决策点 2：归档后镜像留在原位置 | 同意 |
| 决策点 3：`crate::utils::{home_dir,config_dir}` | 同意 |
| 决策点 4：自己找最新的 `state_*.sqlite` | 同意，但打开方式见下面 2.1 |
| 展示方案 A | 同意，但白名单见下面 2.2 |
| 引入 `fs4`、`sha2` | 同意。`fs4` 的版本必须满足 MSRV 1.77.2，在报告里写明最终用的版本 |
| `--kb-reindex` 不进 P1 | 同意 |

## 2. 必须修改的地方

### 2.1 Codex 标题库的只读打开会失败（leader 已在本机复现）

Codex 退出后，`state_5.sqlite-wal` 和 `-shm` 会被清理掉。这时用 `SQLITE_OPEN_READ_ONLY` 或 `mode=ro` 打开都会报 `unable to open database file (14)`，只有 `immutable=1` 能打开：

```
$ ls ~/.codex/state_5.sqlite*        → 只剩 state_5.sqlite
$ sqlite3 "file:state_5.sqlite?mode=ro"      "select count(*) from threads"  → Error (14)
$ sqlite3 "file:state_5.sqlite?immutable=1"  "select count(*) from threads"  → 227
```

**要求：**
- 先用 `mode=ro` 打开；遇到 CANTOPEN 时，如果 `-wal` 文件不存在，就改用 `immutable=1` 重试。没有 `-wal` 文件说明数据都已经写进主文件了，这时用 immutable 方式读是安全的
- 两种方式都打不开时，标题回退到 `session_index.jsonl`，在摘要的 `errors` 里记一条警告，**不能让整次同步失败**
- 加一个单元测试：WAL 模式的库，删掉 `-wal`、`-shm` 后仍然能读到标题
- 底座的 `load_native_title_index` 很可能也有这个问题，**不要去修**，在报告的遗留问题里记一笔即可

### 2.2 Codex 白名单漏了一个根目录

第 1.3 节说 `codex_mirror_roots_canonical()` 只返回 `raw/codex/sessions`，但第 8.1 节规定：首次就在归档目录发现的会话，镜像在 `raw/codex/archived_sessions`。这类会话打开时会被底座的路径校验拒绝。**两个镜像根都要加进白名单**，并补一个测试覆盖"首次在归档目录发现的会话能正常打开"。

### 2.3 同一次扫描里，先识别"移动"，再判断"删除"

Codex 把文件从 `sessions/` 移到 `archived_sessions/` 时，同一次扫描里会同时出现"旧路径没了"和"新路径出现了"两件事。处理顺序必须是：先枚举全部文件 → 用会话 id 匹配出移动 → 剩下没找到的才标记为"源文件已删除"。不能出现"先被标成已删除、再恢复"的中间状态。测试里要覆盖这个顺序。

### 2.4 状态探测不能挤掉定时同步

第 4 节说前端用 `try_lock` 轮询来判断"是否正在收集"。如果探测正好和 launchd 启动的同步撞在同一时刻，定时同步会拿不到锁、直接跳过，白白丢掉一轮收集。

**要求（二选一，推荐 a）：**
- a. `--kb-sync` 获取锁时最多重试 3 秒（每 100ms 一次），超时才返回 skipped
- b. 状态探测不碰锁，改用"`sync_runs` 里有没有 `finished_at` 为空的记录"加上锁文件里的 pid 来判断

### 2.5 兜底标题要跳过系统注入的消息

"第一条用户消息的前 50 个字"这个兜底规则，按现在的写法会取到系统注入的内容：
- Codex：第一条 user message 通常是 `# AGENTS.md instructions…` 或 `<environment_context>…`
- Claude：可能是 `<command-name>`、`<local-command-caveat>`、`<system-reminder>`，或者是 `isMeta` 消息

**要求：** 兜底标题跳过以 `<` 开头、或以 `# AGENTS.md` 开头的消息，取第一条真正由用户输入的文本。Codex 的消息计数也要排除这些注入消息。规则写成一个函数，第 3 期建消息表时复用。

### 2.6 launchd 的 Label 现在就换成我们自己的

不要沿用底座的 `com.claude.history-viewer`。用户以后可能同时装着原版 App，标识相同会互相覆盖；P2 再改，还得处理已经装好的旧任务。

**要求：** Label 固定为 `io.github.moqi777.agentkb.sync`，plist 文件名同名。这个值不跟 `tauri.conf.json` 的 identifier 绑定，写成 `kb` 模块里的一个常量。

## 3. 已帮你核实的问题

**Codex 子代理的父会话字段**（leader 在本机真实数据上确认过）：`session_meta.payload` 顶层就有 `parent_thread_id`；`payload.source` 的结构是 `{"subagent":{"thread_spawn":{"parent_thread_id":"…","depth":1,"agent_path":"…","agent_nickname":"…","agent_role":null}}}`。取父会话 id 时优先读顶层的 `parent_thread_id`，读不到再读 `source.subagent.thread_spawn.parent_thread_id`。普通会话的 `source` 是字符串（`cli`、`vscode`），解析时两种类型都要能处理。

## 4. 不需要改、但实现时注意

- 单行超大（base64 图片）：按你写的方案，首次全量同步时记录最长行。超过 100MB 就停下来汇报
- 前端导航入口的位置，实现时自己定。报告里附截图，或者写清楚入口在哪里
- 第 9 节提到 `--all-features` 前必须先 `pnpm build`：写进报告的"验证命令"部分，让每次验收都能照着复现
