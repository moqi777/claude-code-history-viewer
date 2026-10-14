# P3 设计：全文搜索 + `--kb-search` / `--kb-show` + skill

状态：**有条件通过**（`P3-design-review.md`），已按审核意见修订，本文是修订后的版本；修订处用「【审核修订】」标出，落实情况见 `P3-report.md`。

文中的数字都是我在本机真实数据的**副本**上量的（`~/AgentKB/raw` 拷到临时目录，274 个会话文件、2.8 GB，不动 `~/AgentKB`），原型代码在 `scratch/p3/proto/`（gitignored，不进仓库）。两个 Agent 的实测记录见第 6 节。

## 0. 先说结论和需要你拍板的三件事

方案一句话：消息级抽取（只取该索引的内容、截断）→ jieba 在 Rust 里预先切词 → 写入 `kb.db` 里的 FTS5 无内容表（contentless）+ 一张保存原文片段的 `messages` 表；`--kb-search`/`--kb-show` 只读 `kb.db`；skill 是一个很短的 SKILL.md，指向 App 里的二进制。

实测数字（真实数据副本）：索引 **99 MB**（原始数据的 3.5%），首次建索引 **约 5 秒**，一次查询 **1–25 ms**，`--kb-search` 整个进程（含加载 jieba）**0.08–0.10 秒**，都远好于验收线。

**需要你拍板：**

1. **kb.db 的 WAL 模式在 Codex 沙箱里打不开**（第 6 节实测）：没有 `-wal/-shm` 文件时，沙箱里的只读打开直接 `unable to open database file`。我的方案是 `--kb-search/--kb-show` 先正常只读打开，失败再用 `immutable=1` 重试（实测在 Codex 的 workspace-write 和 read-only 两种沙箱里都能用）。代价写在 6.2：只在「没有任何进程持有 kb.db、WAL 已全部落盘」时才会走到 immutable，所以不会少数据，只有极小概率在收集进程恰好开始写的瞬间读到不一致的页，结果是报错重试而不是错数据。你如果不接受，备选是把 kb.db 改成 DELETE 日志模式（会让 GUI 和收集之间的并发读写变差），我不推荐。

【审核修订】leader 同意；补两点：输出里加 `"read_mode": "normal" | "immutable"`；immutable 模式下查询出错时自动重试一次（间隔 150 ms，最多一次）。
2. **Claude Code 的权限提示不能完全消除**（第 6 节实测）：skill 文件里写 `allowed-tools` 后，调用我们的二进制不再提示；但**第一次调用 `Skill(agentkb)` 本身**在默认权限模式下会提示一次（用户可以选「总是允许」）。要彻底免提示只能改 `~/.claude/settings.json` 的 permissions，这超出 PLAN 里「只写 `skills/agentkb/`」的例外，所以我**不做**，只在安装界面里告诉用户第一次会问一次、点「总是允许」。【审核修订】leader 同意；安装界面上用一句话说明「第一次会问一次，选“总是允许”」。
3. **App 移动或升级后 skill 里的路径过期怎么修**：PLAN 例外只允许「用户点安装」时写 `~/.claude`、`~/.codex`，所以不能像定时任务那样在 App 启动时自动改。方案：App 启动和设置页只**检测**（已安装但版本/路径过期 → 首页提示「Agent 技能需要更新」，一个按钮，用户点了才写）；同时 SKILL.md 里带一条不用写盘的兜底（路径不存在时用 `mdfind` 按 bundle id 找 App），所以过期时不会完全失效。【审核修订】leader 确认：点「更新」算用户点击安装，在 PLAN 例外范围内；mdfind 兜底保留。

## 1. 表结构与增量

### 1.1 表（`db.rs::MIGRATIONS` 追加 v2，不改 v1）

```sql
-- v2
CREATE TABLE messages (
  id         INTEGER PRIMARY KEY,           -- 同时是 FTS 的 rowid
  file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  seq        INTEGER NOT NULL,              -- 该文件内第 N 个候选消息，从 1 起，稳定且可重算
  kind       TEXT NOT NULL CHECK (kind IN ('user','assistant','tool_use','tool_result','tool_error')),
  ts         TEXT,                          -- 行时间戳（ISO）
  byte_off   INTEGER NOT NULL,              -- 该行在镜像（未压缩流）里的起始字节
  msg_key    TEXT,                          -- Claude: 行 uuid；Codex: NULL（见 9.3）
  text       TEXT NOT NULL,                 -- 截断后的原文，用于片段和 --kb-show
  truncated  INTEGER NOT NULL DEFAULT 0,
  UNIQUE (file_id, seq)
);
CREATE INDEX idx_messages_file ON messages(file_id, seq);
CREATE VIRTUAL TABLE messages_fts USING fts5(
  body, tool,                               -- body: 用户/助手正文；tool: 工具命令/结果
  content='', contentless_delete=1,         -- 只存倒排，原文在 messages.text
  tokenize='unicode61 remove_diacritics 0'
);
ALTER TABLE sessions ADD COLUMN kb_turns INTEGER NOT NULL DEFAULT 0;  -- 被「自己搜自己」规则排除的轮数，仅诊断
```

- `messages` 与 `sessions` 通过 `file_id` 关联（`sessions.file_id` 本来就 UNIQUE，见 `db.rs` v1），`files` 保存游标，三者一一对应。会话元数据（标题、项目、客户端、时间、源文件状态）**不复制**，查询时 JOIN `sessions`/`files`，所以 Codex 原生标题刷新（`sync.rs::refresh_codex_titles`）、源文件被删（`source_status`）自动反映到搜索结果。
- 为什么用 contentless：FTS5 里存的是我们**预先切好词、用空格连起来**的文本（`连接 连接池 耗尽`），不能当原文展示；原文放 `messages.text`。contentless 表不支持 `snippet()`，所以片段我们自己在 Rust 里从 `text` 截（反正中文切词后 FTS 自带的片段也不合适）。`contentless_delete=1` 需要 SQLite ≥ 3.43；本项目 `rusqlite` 的 bundled 是 libsqlite3-sys 0.30.1（SQLite 3.46），原型实测建表、`DELETE … WHERE rowid IN (…)` 删除、再查询都正常（删掉一个会话的消息后，`死锁` 的命中从 11 降到 9），不加新的 SQLite 依赖。
- FTS 虚表没有外键级联：删消息必须同时删 FTS 行，统一走一个函数 `db::purge_messages(file_id, from_seq)`（先 `DELETE FROM messages_fts WHERE rowid IN (SELECT id …)` 再删 `messages`），不依赖 `ON DELETE CASCADE`。
- 两个 FTS 列的目的：`bm25(messages_fts, 1.0, 0.4)` 让正文命中比工具输出命中权重高（工具输出是证据，不是结论）。

### 1.2 抽取规则（哪些行变成消息）

放在新文件 `kb/index/messages.rs`，Claude/Codex 各一个函数，被现有 `ClaudeIndexer::feed` / `CodexIndexer::feed`（`index/claude.rs:135`、`index/codex.rs`）在解析每一行时顺带调用。截断上限的依据见第 3 节表格：

| 来源 | 变成什么 | 截断 |
|---|---|---|
| Claude `user`，content 为字符串或 `text` 块 | `user`，跳过 `is_injected_user_text`（`index/text.rs`）认定的注入内容 | 16 KB（覆盖 97.9%） |
| Claude `assistant` 的 `text` 块 | `assistant` | 16 KB（覆盖 100%） |
| Claude `assistant` 的 `tool_use` | `tool_use`：工具名 + 取 `command/file_path/path/pattern/query/url/description/prompt` 这些字段（每个 ≤400 字符），没有就取 JSON 前 300 字符 | 2 KB（覆盖 94%） |
| Claude `user` 的 `tool_result` | `tool_result`；`is_error` 为真则 `tool_error` | 头 1000 + 尾 400 字符（错误：头 2000 + 尾 400） |
| Codex `response_item/message`，role=user | `user`；`## My request for Codex:` 之前的 IDE 上下文去掉；注入内容跳过 | 16 KB |
| Codex `response_item/message`，role=assistant | `assistant` | 16 KB |
| Codex `function_call` / `custom_tool_call` | `tool_use`：名字 + 参数里的 `cmd/command/workdir…` 或 `input` | 2 KB |
| Codex `function_call_output` / `custom_tool_call_output` | `tool_result`（去掉 `Chunk ID…Output:` 头） | 头 1000 + 尾 400 |
| **不索引** | Claude `thinking`、`attachment`、`system`、`file-history-snapshot`、图片块；Codex `reasoning`、`event_msg/*`（含体积最大的 `item_completed`，649 MB 重复内容）、`compacted`（838 MB）、`token_count`、`turn_context`、`developer` 消息 | — |

Codex 的 `item_completed` 和 `compacted` 合计占 Codex 数据的一大半，且是别处内容的重复（原型里按子串跳过，不解析 JSON，所以首次建索引才这么快）。

### 1.3 `INDEXER_VERSION` 与首次建索引

- `sync.rs::INDEXER_VERSION` 由 `"3"` 升到 `"4"`。升级后第一次同步走现有的 `reindex_if_outdated` → `reindex_one`（`sync.rs:396`）：从镜像重新喂一遍，这一遍同时产出 `messages` 行。**不需要新的迁移通道**。
- `reindex_one` 目前只 upsert `sessions`，要改成：先 `purge_messages(file_id, 0)`，再批量插入 indexer 产出的消息行，同一个事务提交。失败则不写 `indexer_version`，下次重试（现有语义）。
- 原型实测整个 2.8 GB 首次建约 5 秒；真实流程多了 `sessions` 的 upsert 和 zstd 解压（压缩镜像），预计 10 秒量级，在持有 sync 锁的那次收集里完成（GUI 看到的是「收集中」）。步骤 5 量真实数字，写进报告。
- `--kb-reindex`：新增命令。清掉 `messages`/`messages_fts` 和 meta 里的 `indexer_version`，然后跑一次同步（仍在同步锁里），等价于首次建索引；`sessions` 也一并按镜像重派生。完成后的搜索结果必须与增量建出来的一致（验收 3，步骤 5 有测试：对同一份数据「增量建」和「重建」后逐条比较 `messages` 的 `(file_id, seq, kind, text)`）。

### 1.4 增量更新

现有流程：`process_file`（`sync.rs:438`）→ `mirror::decide` 给出 `Unchanged | Append{from} | Rewrite`（`mirror.rs:24`）→ `Indexer` 只喂新追加的字节 → `commit_file`（`sync.rs:~520`）在**一个事务**里更新游标 + `upsert_session`。消息索引完全挂在这条路径上：

- Indexer 内部新增 `pending: Vec<MessageRow>`，`feed` 时按抽取规则追加；`commit_file` 在同一个事务里 `INSERT` 这些行和对应的 FTS 行，然后清空。游标、sessions、messages 同生共死，不会出现「游标前进了但消息没写」。
- 需要的新状态放进已有的 `indexer_state` JSON（会话行上，`db::load_indexer_state`）：`next_seq`、`bytes_fed`（用来算每行的 `byte_off`，恢复时必须等于镜像游标，不等就走现有的 `restore_indexer` 重放分支）、`turn_start_seq`、`in_kb_turn`、`kb_call_ids`（第 5 节）。
- **重放分支**（`restore_indexer` 在状态损坏时从镜像重读）会重新产出已入库的消息 → 该分支标记 `replace_all`，提交时先 `purge_messages(file_id, 0)` 再插入，避免重复。
- **`Decision::Append`**：只处理新增字节，增量成本 = 新增行数 × 单行成本（原型 ≈ 17k 条/秒），一次典型收集（几十条新消息）是毫秒级。步骤 5 在真实数据副本上实测并写进报告（验收线 2 秒）。
- **`Decision::Rewrite` / `.prev-*`**（源文件被改写，旧镜像被 `rotate_aside` 改名为 `.prev-<ts>`）：对这个文件从头建，`purge_messages(file_id, 0)` 后插入新内容，**旧版本只留在 `.prev-*` 里，不进索引**。取舍：改写很少见（P1 记录里 `rewritten_files` 基本为 0），旧版内容仍在磁盘上可以手工找；如果也索引旧版，`byte_off`/`seq` 会指向不存在的位置，`--kb-show` 无法展示。`.prev-*`、`.conflict-*` 文件本来就不是 `files` 表的行（P2 里 `.conflict-*` 改名成不以 `.jsonl` 结尾），索引不会扫到它们。
- **源文件被删**（`source_status='deleted'`）：消息保留（镜像还在），`--kb-search` 结果里标 `source_status:"deleted"`，resume 命令不可用（第 4 节）。
- 备份：每天一次的 `kb.db` 在线快照（`backup.rs`）会带上索引，快照从 ~1 MB 变成 ~100 MB，`kb.db` + `kb.db.prev` 共约 200 MB 写到外置盘，可以接受；索引是派生数据，丢了能重建。

## 2. 分词

### 2.1 jieba 怎么带进程序

- 依赖 `jieba-rs = "0.7"`（MIT，实测 0.7.4），默认 feature `default-dict` 把 `dict.txt`（5.07 MB）编译进二进制，运行时用 `Jieba::new()` 从内置词典建 trie，**不需要额外的资源文件，也不联网**。
- 二进制体积：原型（jieba + rusqlite bundled + serde_json）release 7.6 MB，jieba 词典占其中约 5 MB；预计 AgentKB 二进制从 29.0 MB 增加约 5 MB（`.app` 约 +5 MB）。不开 `textrank/tfidf`（那会再引入 6 MB 的 `idf.txt`）。
- 启动成本：`Jieba::new()` 实测 **73–104 ms**。只在文本里出现 CJK 字符时才初始化（`once_cell::sync::Lazy`），纯英文查询（如 `NoSuchMethodError`）不付这笔钱；`--kb-search` 整个进程实测 0.08–0.10 秒（含这 75 ms）。收集进程只在有新的中文消息时才付一次。
- 不放用户词典（第 6 期以后再说）；留一个扩展点：如果存在 `<数据目录>/user_dict.txt` 就加载，**本期不做**。

### 2.2 切词规则（`kb/index/tokenize.rs`，索引和查询共用同一个函数，保证一致）

先按字符类别把文本切成「CJK 段」和「其它段」：

| 文本 | 规则 | 例子 |
|---|---|---|
| 中日韩字符段 | `jieba.cut_for_search`（搜索引擎模式，会把长词再切出子词），丢掉标点、空白和中文停用词表里的词（的、了、是、什么、怎么、之前、那个、问题 之类的口语词，约 70 个，写在代码里） | `死锁` → `死锁`；`连接池耗尽` → `连接 连接池 耗尽` |
| 英文、数字段（`[A-Za-z0-9_]+`，其余字符都当分隔符） | 小写后的**整词**一定保留；如果是驼峰/下划线/字母数字交界，再额外保留各个部分（长度 ≥2） | `WechatAgreementRepository` → `wechatagreementrepository wechat agreement repository`；`HTTPServer2` → `httpserver2 http server` |
| 带点的包名、报错 | `.`、`/`、`-`、`:` 都是分隔符，所以每一段单独成词；整词不拼回去 | `java.lang.NoSuchMethodError` → `java lang nosuchmethoderror no such method error` |
| 路径 | 同上，按 `/` 和 `.` 切 | `/Users/me/a_b.txt` → `users me a_b txt` |
| 超长串（> 64 字符，如 base64、哈希） | 丢弃 | — |

（原型 `proto tok` 对上述每一类输入的实测输出与表中一致。）

### 2.3 查询怎么组合

`--kb-search "<问题或关键词>"` 的处理（`kb/search.rs`）：

1. 用同一个切词函数得到 token 列表，去重，丢停用词。一个英文单词产生的「整词 + 各部分」视为**一组**：组内匹配条件是 `整词 OR (部分1 AND 部分2 …)`，这样查 `NoSuchMethodError` 能命中写成 `No Such Method Error` 的文字，也能命中整词。
2. **第一轮「全部匹配」**：各组之间 `AND`。结果会话数 ≥ 3 就用这一轮，`mode_used:"all"`。
3. **第二轮「放宽」**（第一轮不足 3 个会话，或 token 超过 6 个）：各组之间 `OR`，取前 300 条消息，对每条候选重新切词算**覆盖率**（命中了多少个查询组），`得分 = bm25 × (0.5 + 覆盖率)`，丢掉覆盖率 < 40% 的。`mode_used:"any"`，输出里写明，让 Agent 知道这是放宽过的结果。
4. 排序：消息得分用 `bm25(messages_fts, 1.0, 0.4)`（SQLite 里越小越好，取反）。**会话得分** = 命中的消息里最高分 + 0.25 × 第二高 + 0.1 × 第三高，再乘一个温和的新近度系数 `1 + 0.15 × max(0, 1 − 年龄天数/365)`（最近的会话略微靠前，不会压过明显更相关的老会话）。这些系数是起点，**用第 7 节的检索测试集调**，调完把最终值和调参前后的命中率写进报告（不拿测试集以外的东西调，避免过拟合：挑完题目后系数只允许改一轮）。
5. 子 Agent 会话（`is_subagent`）默认**包含**，得分 ×0.8，输出里标 `is_subagent:true` 并给父会话的 resume 命令；`--no-subagents` 可排除。理由：很多真实排查是在子 Agent 里做的（原型里 `agent-*.jsonl` 命中很多），排除会漏。

查询性能（原型，真实数据）：`死锁` 11 命中 1.6 ms；`限流` 56 命中 9 ms；`BLOCKED` 227 命中 24 ms；`NoSuchMethodError` 1.2 ms。远低于 300 ms。

## 3. 本机真实数据量出来的数字

测试对象：`~/AgentKB/raw` 的副本（`cp -R` 到 `$TMPDIR`，274 个文件，2.8 GB；Claude 341 MB + Codex 2.4 GB）。`kb.db` 没动。

**抽取量与索引大小**（原型按第 1.2 节规则，工具输出按头 1000+尾 400，tool_use 暂按 1 KB）：

| | 数值 |
|---|---|
| 消息数 | 84,168（user 4,672 / assistant 14,240 / tool_use 33,536 / tool_result 31,509 / tool_error 211） |
| 抽取后的文本 | 53.9 MB |
| `kb` 索引总大小 | **99.4 MB**（`messages` 表含原文 62 MB，FTS 倒排 30 MB）= 原始数据的 **3.5%** |
| 首次建索引耗时 | **4.8 s**（含解析 2.8 GB JSONL、切词 1.9 s、写库；峰值内存 219 MB） |
| 查询耗时（进程内） | 0.9–24 ms |
| `--kb-search` 进程总耗时 | 0.08–0.10 s（含 jieba 加载 75–85 ms） |

**为什么截断上限定成这样**（不截断是什么样：全量建索引 657 MB、24 s，因为一个 `tool_result` 最大 4.7 MB、Codex 的输出动辄上百 KB）。下表是各类消息**截断前**的长度分布和不同上限下的被截断比例（在副本上不截断全量跑出来的）：

| 类型 | 中位数 | p90 | p99 | 上限 → 被截断比例 → 保留体积 |
|---|---|---|---|---|
| user | 93 B | 791 B | 30 KB | 8 KB: 3.0% / 2.4 MB；**16 KB: 2.1% / 3.4 MB**；32 KB: 0.9% / 4.4 MB |
| assistant | 243 B | 1.7 KB | 5 KB | 4 KB: 1.7% / 8.0 MB；**16 KB: 0.0% / 8.6 MB** |
| tool_use | 302 B | 1.4 KB | 8.4 KB | 1 KB: 15.8% / 13.5 MB；**2 KB: 6.0% / 16.7 MB**；8 KB: 1.0% / 21.5 MB |
| tool_result | 1.4 KB | 21 KB | 92 KB | 全量 311 MB；**头 1000+尾 400 约 21–25 MB**；8 KB 上限要 92 MB 且仍有 21.8% 被截断 |

结论：正文（user/assistant）16 KB 基本不丢；`tool_use` 2 KB 覆盖 94%；`tool_result` 报错通常出现在输出开头或结尾，所以取「头 1000 + 尾 400」而不是单纯取前 N 字——这是体积（占索引一半以上）和「靠报错信息能搜到」之间的折中。tool_use 改成 2 KB 后索引大约再增加 3 MB，总量仍在 ~105 MB 量级；最终数字以步骤 5 的实现为准。

增量耗时、`--kb-reindex` 耗时、真实流程下的首次建索引耗时在步骤 5 用同一份副本重量，写进 `P3-report.md`。

## 4. `--kb-search` / `--kb-show`

两者都在 `lib.rs::run` 最前面分发（`cli.rs::try_run` 增加两个分支，和现有 `--kb-sync` 同一机制，不起窗口，不经过 single-instance）。**只读 `kb.db`**：不拿 `lock.rs` 的同步锁，不写任何东西（包括不写日志文件、不更新 meta），不触发收集。数据目录通过 `KbConfig::load()` 读 `~/Library/Application Support/agent-kb/config.json` 得到。

### 4.1 `--kb-search "<query>"`

| 参数 | 说明 |
|---|---|
| `<query>` | 位置参数，关键词或一句话（建议 2–4 个关键词，skill 里这样教 Agent） |
| `--source claude\|codex` | 来源 |
| `--client <名>` | 客户端（`cli`/`vscode`/`exec`/`desktop` 等，`sessions.client`） |
| `--project <文本>` | 项目名或 cwd 包含该文本（不区分大小写） |
| `--since <7d\|30d\|2026-09-01>` / `--until <…>` | 按会话最后活动时间 |
| `--limit N` | 返回会话数，默认 5，上限 20 |
| `--per-session N` | 每个会话最多给几条命中片段，默认 2，上限 5 |
| `--no-subagents` | 排除子 Agent 会话 |
| `--mode auto\|all\|any` | 默认 `auto`（第 2.3 节） |
| `--format json\|text` | 默认 `json`（给 Agent 看）；`text` 给人看 |
| `--kinds user,assistant,tool_use,tool_result` | 只在这些类型里搜，默认全部 |

JSON 输出（默认；字段名稳定，skill 依赖它）：

```json
{
  "agentkb_marker": "agentkb-result-v1",
  "query": "死锁",
  "tokens": ["死锁"],
  "mode_used": "all",
  "read_mode": "normal",
  "data_as_of": "2026-10-15T00:30:22Z",
  "total_sessions": 11,
  "returned": 2,
  "results": [
    {
      "rank": 1,
      "score": 7.37,
      "session": {
        "source": "codex",
        "session_id": "01a08505-d675-7c42-936d-9cda9c2f1c29",
        "title": "排查 Ghostty 卡死",
        "client": "vscode",
        "project": "ghostty-debug",
        "cwd": "/Users/mo7/Dev/ghostty-debug",
        "started_at": "2026-09-09T15:15:47Z",
        "last_activity_at": "2026-09-09T16:02:10Z",
        "source_status": "present",
        "is_subagent": false
      },
      "resume": {
        "available": true,
        "command": "codex resume 01a08505-d675-7c42-936d-9cda9c2f1c29",
        "note": null
      },
      "paths": {
        "mirror": "/Users/mo7/AgentKB/raw/codex/sessions/2026/09/09/rollout-2026-09-09T15-15-47-01a08505-….jsonl",
        "original": "/Users/mo7/.codex/sessions/2026/09/09/rollout-2026-09-09T15-15-47-01a08505-….jsonl"
      },
      "hit_messages": 3,
      "hits": [
        {
          "seq": 26,
          "kind": "assistant",
          "at": "2026-09-09T15:40:12Z",
          "snippet": "…不能仅凭 objc_retain 宣布 ARC 或 WindowServer 已<<死锁>>，应保存完整的全线程 sample/spindump…",
          "show": "/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-show codex:01a08505-d675-7c42-936d-9cda9c2f1c29 --around 26"
        }
      ]
    }
  ],
  "hint": null
}
```

- 一个会话命中多条消息：**按会话合并**，`hit_messages` 是命中的消息总数，`hits` 只给得分最高的 `--per-session` 条（按得分，不按时间）。
- `resume.command`：Claude `cd '<cwd>' && claude --resume <session_id>`（Claude 的会话按 cwd 分目录，必须在原目录里 resume；cwd 为空则不给）；Codex `codex resume <session_id>`（`codex resume --help` 实测：`SESSION_ID` 是 UUID，**不依赖 cwd**，`--all` 只影响选择器；真正 resume 一次在步骤 5 做）。子 Agent 会话给**父会话**的命令并在 `note` 里说明。
- **源文件已删除**（`source_status:"deleted"`）：`resume.available:false`，`note:"原会话文件已被删除，无法 resume；用 --kb-show 读取备份内容"`。
- 没有结果：`results:[]`，`hint` 给一句建议（换关键词、放宽时间/来源）。
- 退出码：0 成功（包括 0 个结果）；2 未配置/没有 `kb.db`（输出 `{"error":"…"}`）；3 索引还没建好（`INDEXER_VERSION` 不匹配或 `meta.indexer_version` 缺失，输出里写「正在建索引，稍后重试」）；1 其它错误。
- `data_as_of`：`sync_runs` 里最近一次成功的 `finished_at`。

`--format text`（给人看）：

```
找到 11 个会话，显示前 2 个（数据截至 2026-10-15 08:30）
1. [Codex · vscode] 排查 Ghostty 卡死 — ghostty-debug · 2026-09-09 23:15
   resume: codex resume 01a08505-d675-7c42-936d-9cda9c2f1c29
   #26 assistant  …不能仅凭 objc_retain 宣布 ARC 或 WindowServer 已<<死锁>>…
   还有 2 处命中；看上下文：AgentKB --kb-show codex:01a08505-… --around 26
```

### 4.2 `--kb-show <会话> [--around <seq>]`

`<会话>` 写法：`<source>:<session_id>`，或不带来源的 session id（前缀 ≥ 8 位且唯一即可；歧义时列出候选并退出码 2）。

| 参数 | 说明 |
|---|---|
| `--around <seq>` | 以该消息为中心，前后各 `--context N` 条（默认 4） |
| `--head N` / `--tail N` | 不指定 `--around` 时取开头/结尾 N 条（默认 `--head 6`） |
| `--kinds` | 默认 `user,assistant`（Agent 要读结论，工具输出默认不展开）；要看命令和报错加上 `tool_use,tool_result` |
| `--max-chars N` | 每条消息最多输出多少字符，默认 2000（保护 Agent 的上下文），超出标 `truncated:true` |
| `--full` | 命中的那条消息改读镜像原文（按 `byte_off` 直接定位，压缩镜像则解压到该偏移），不受索引截断限制 |
| `--format json\|text` | 同上 |

```json
{
  "agentkb_marker": "agentkb-result-v1",
  "data_as_of": "2026-10-15T00:30:22Z",
  "session": { "source": "codex", "session_id": "…", "title": "…", "project": "…", "cwd": "…", "source_status": "present" },
  "resume": { "available": true, "command": "codex resume …" },
  "range": { "from_seq": 22, "to_seq": 30, "total_messages": 214 },
  "messages": [
    { "seq": 26, "kind": "assistant", "at": "2026-09-09T15:40:12Z", "text": "…", "truncated": false, "focus": true }
  ]
}
```

没有被「自己搜自己」规则排除、也没被索引的消息（thinking 等）在 `--kb-show` 里同样不出现；`--kb-show` 只展示索引里有的消息。

## 5. 自己搜到自己【审核修订：不再整轮排除】

问题：Agent 调 `--kb-search` 后，① 命令行（含查询词）作为 `tool_use` 写进会话；② JSON 结果作为 `tool_result` 写进会话；③ 用户的问题和 Agent 的回答也写进会话。下次收集后这些都会被索引，搜同一个词又会搜到「上次的搜索」。

原方案（只要这一轮调用过就整轮不索引）会把「先搜历史、再照着修」这种最常见用法里真正的工作内容也丢掉。**修订后的规则**（一轮 = 从用户的一条真实提问到下一条真实提问之前）：

1. **`--kb-search`、`--kb-show` 的调用和结果一律不进索引。** 识别：`tool_use` 的命令里含 `--kb-search`/`--kb-show`（Claude 的 `Bash.input.command`、Codex 的 `exec_command/shell` 参数里的 `cmd`），或 Claude 的 `Skill` 调用 `skill=agentkb`；Indexer 状态里记下该调用的 `tool_use_id`/`call_id`，配对的 `tool_result` 跳过。第二道保险：任何 `tool_result` 文本里含 `"agentkb_marker":"agentkb-result-v1"`（输出的第一个字段）的也跳过。
2. **纯问答轮**：这一轮里除了 kb 调用**没有别的工具调用** → 这一轮的用户提问和助手回答也不进索引。
3. **做了别的事的轮**：这一轮里还有别的工具调用 → 用户提问、助手回答、其他工具调用和结果都照常索引。
4. **实现（流式，增量安全）**：每轮维护三态 `Undecided → KbOnly → Work`（`index/messages.rs`，状态序列化进已有的 `indexer_state`）。
   - 轮内的 `user` 提问和 `assistant` 文本先正常产出，同时记在本轮的 `turn_rows`（有上限：最多 32 条、64 KB，超过就当作工作轮，不再保留）。
   - 出现第一个 kb 调用且此前没有别的工具调用：进入 `KbOnly`，对本轮已经产出的行发出 `Remove(seq)`（`commit_file` 在同一事务里 `purge`），之后这一轮的 assistant 文本只保留在 `turn_rows` 里、不产出。
   - 在 `KbOnly` 里又出现别的工具调用：进入 `Work`，把 `turn_rows` 里被撤回的行重新产出（`seq` 不变），此后一律照常产出。`Work` 是终态，同一轮之后再出现 kb 调用也只跳过调用本身和结果。
   - 下一条真实用户提问到来：关闭上一轮（`KbOnly` 保持被排除），开新轮。
   - 「真实用户提问」= 带非注入文本的 `user` 消息，不是只有 `tool_result` 的 `user` 行。
5. **确定性**：`seq` 按「候选消息」计数，被跳过/被撤回的行**也占序号**，所以增量建和 `--kb-reindex` 重建的 `seq` 完全一致，`--kb-show --around` 的序号稳定。
6. **验收 7 的测试口径**（步骤 5 自动化 + 真机各一遍）：纯问答轮连续搜 3 次、再收集后，第 4 次的结果里不出现前 3 次的内容；再加一个「搜完之后接着改代码」的轮，确认改代码那部分**能**搜到，而它之前的 kb 调用/结果仍然搜不到。另有一个「先问答、后来在同一轮追加了别的工具调用」的增量用例（跨两次同步），验证 `KbOnly → Work` 能把撤回的行补回来。

## 5b. 输出前脱敏【审核修订：新增】

搜索结果会进入 Agent 的上下文（发给 Anthropic 或 OpenAI），而且是跨 Agent 的，所以**在建索引时**脱敏，`messages.text` 里存的就是脱敏后的文本，FTS 也不收录这些值。新文件 `kb/redact.rs`，第 4 期整理时复用。

| 类别 | 例子 | 处理 |
|---|---|---|
| 键值对 | `password=…`、`"passwd": "…"`、`secret`、`token`、`api_key`、`apiKey`、`access_key`、`client_secret`、`private_key`、`--password x`、环境变量 `export FOO_TOKEN=…` | 键名保留，值换成 `[REDACTED]`；键名必须**以敏感词结尾**才算，所以 `token_count`、`max_tokens`、`tokens` 不会被误伤 |
| 授权头 | `Authorization: Bearer …`、`Basic …`、单独的 `Bearer <长串>` | `[REDACTED]` |
| 常见密钥格式 | `sk-…`（含 `sk-ant-…`）、`ghp_/gho_/ghu_/ghs_/ghr_…`、`github_pat_…`、`AKIA…`/`ASIA…`、`xox[baprs]-…`、`AIza…` | `[REDACTED]` |
| JWT | `eyJ….eyJ….…` | `[REDACTED]` |
| 私钥块 | `-----BEGIN … PRIVATE KEY-----` 到 `END`（或到文本结尾） | `[REDACTED]` |
| URL 里的凭据 | `https://user:pass@host` | `https://[REDACTED]@host` |

- 顺序：**先脱敏、后截断**，脱敏在比上限多 512 字节的窗口上做，避免密钥正好被截断点劈开留下前半截。
- 其它出口也过一遍：`--kb-show --full` 读镜像原文后、搜索结果里的标题和片段（纵深防御）。
- 单元测试覆盖上面每一类，加反例（`token_count`、`max_tokens`、普通英文句子里的 "secret garden"、没有值的 `password:`）。
- 副作用：按密钥值搜索会失效，这是预期行为。

## 6. 在 Agent 里能不能跑起来（本机实测）

测试环境：Claude Code 2.1.299、codex-cli 0.162.1。测试用的 skill 指向原型二进制和副本索引，装在**项目目录**里（`scratch/p3/agent-test/{A,B,C,D,E}`），没有往真实的 `~/.claude/skills`、`~/.codex/skills` 写任何东西。注意：这些测试会话本身会被 Agent 记录到真实的 `~/.claude/projects/…-scratch-p3-agent-test-*` 和 `~/.codex/sessions/`（按 cwd 很容易认出来），AgentKB 之后会把它们收进真实库；我不会去动这些真实文件。

### 6.1 Claude Code

用户的全局设置是 `bypassPermissions`，所以测试时加 `--permission-mode default` 并用 `-p`（非交互时需要确认的调用会被拒绝并记入 `permission_denials`，等价于「会弹提示」）。

| 场景 | 结果 |
|---|---|
| skill 含 `allowed-tools: Bash(<二进制> search:*)`，不放行 `Skill` | **`Skill(agentkb)` 调用被拒**（要确认）；Agent 随后改用 `grep ~/.claude/projects`，也被拒 |
| 同上，并放行 `Skill(agentkb)`（`--allowedTools`） | 搜索调用**没有任何拒绝**，Agent 给出了会话文件名和命中原文；被拒的只有 Agent 自己额外的尝试（`grep` 原始会话、`--help`、`find`） |
| skill 不写 `allowed-tools` | 每一次调用二进制都要确认（被拒） |
| 全局 `bypassPermissions`（用户自己的环境） | 完全没有提示（两种 skill 都能跑） |

结论：`allowed-tools` 能让调用二进制免提示；第一次 `Skill(agentkb)` 需要用户在 Claude Code 里点一次「允许」（可选「总是」）。另一个发现：**被拒后 Agent 会去 grep 原始会话文件**，所以 SKILL.md 里明确写「不要自己 grep `~/.claude`/`~/.codex`，用 `--kb-show` 读上下文」，并保证 `--kb-show` 能提供它需要的内容（这就是它存在的原因）。

### 6.2 Codex

Codex 的 skill 发现：`$CODEX_HOME/skills/<name>/SKILL.md`（用临时 `CODEX_HOME`、只软链了 auth 做的验证：Agent 先 `cat` 了那个目录里的 SKILL.md 然后照做）；项目级 `.agents/skills/<name>/SKILL.md` 也被发现（目录 C）。说明 PLAN 里的 `~/.codex/skills/agentkb/SKILL.md` 是对的。用户的 Codex 配置是 `danger-full-access`，所以测试时显式用 `-s workspace-write` 和 `-s read-only`（`approval_policy=never`）。

| 场景（二进制读 WAL 模式的 `idx.db`，放在沙箱可写目录之外） | 结果 |
|---|---|
| 目录里**没有** `-wal`/`-shm`（没有进程持有库） | **两种沙箱都失败**：`SqliteFailure(CannotOpen) unable to open database file` |
| 有另一个进程持有库（`-wal`/`-shm` 存在） | 两种沙箱都成功，结果正确 |
| 没有 `-wal`/`-shm`，二进制捕获失败后改用 `file:…?immutable=1` 重试 | 两种沙箱都成功，结果正确（`normal read-only open failed … retrying immutable`） |

原因：SQLite 读 WAL 库需要能创建/写 `-shm`，沙箱不允许写这个目录。`immutable=1` 告诉 SQLite 这个文件不会变，不用 WAL、不用锁。**代价与风险**：immutable 读不到 `-wal` 里还没写回主库的数据，并且如果此刻有进程正在写，可能读到不一致的页。但只有「没有任何进程持有这个库」时才会失败而落到 immutable 分支，而 SQLite 在最后一个连接关闭时会把 WAL 全部写回主库并删掉 `-wal`/`-shm`，所以那种情况下主库就是完整的；剩下的只是「我们判断它没人用、随后收集进程正好开始写」这一个很窄的窗口，表现为一次查询报错或少量异常，重试即可。结果里仍会带 `data_as_of`。（备选：把 kb.db 改成 DELETE 日志模式，读者不需要 `-shm`；但会让 GUI 查询和收集写入互相阻塞，我不推荐。）

Codex 在沙箱里读取 `~/AgentKB` 本身**没有问题**（读权限不受限）；默认的 `workspace-write + on-request` 下，在沙箱内能完成的命令不需要审批。`untrusted` 审批策略我没有测。

另外：Codex 的 Agent 在没有 `--kb-show` 时，会自己用 `sqlite3`、`python3` 去读索引库（原型里就是这样）——正式版 SKILL.md 要引导它用 `--kb-show`，别去碰 `kb.db`。

### 6.3 Agent 行为观察（影响 SKILL.md 写法）

- 两个 Agent 都会自己把长句拆成关键词再搜，并在不满意时换词重搜（Claude 6–9 轮、Codex 十几次调用）。所以 SKILL.md 要限制「最多搜 3 次」，否则耗时和上下文都会被吃掉。
- 它们会把整句话当查询词传进来；所以服务端必须能处理（第 2.3 节的停用词 + 放宽）。
- Claude 的结果里正确指出了「最接近的会话不是真正的死锁」，说明片段+`--kb-show` 提供的证据足够它判断；但排序把「一道面试题清单」里的工具调用排在第一位，这是第 7 节要调的。

## 7. 检索效果测试集（步骤 5 做，这里先定规则）

- 【审核修订】期望会话**不能**用 `scratch/p3/agent-test/*` 这类测试会话（它们已进真实库），挑题时避开。
- 从本机真实会话挑 **15 个**「以后真会问」的问题，中文为主，含 2 个字的词、类名、报错信息各至少 2 个；每个写明**应该命中哪个会话**（session id）。问法像用户回想时的口吻，**不照抄会话里的原话**。
- 题目在**看到检索结果之前**写好并提交到 `docs/agent-kb/P3-eval.md`（附期望会话 id），之后只允许调一轮排序系数（第 2.3 节第 4 点），改前改后的命中率都写进报告。
- 指标：前 5 条会话里是否包含期望会话；未命中的逐条分析（切词问题 / 索引没收 / 排序问题 / 题目本身含糊）。
- 备注：我已经在原型上用「死锁」「限流」「NoSuchMethodError」「消息丢失」做过冒烟（都能搜到相关会话），但这不算测试集。

## 8. SKILL.md 与安装

### 8.1 SKILL.md 草稿（Claude 版；Codex 版去掉 `allowed-tools` 一行）

```markdown
---
name: agentkb
description: Search the user's past Claude Code and Codex conversations (backed up by AgentKB) to recall how something was solved or decided before. Use when the user says "之前 / 上次 / 以前 / 那个问题是怎么解决的 / 我们是不是碰到过 / 有没有做过", or asks to find an earlier session or get its resume command.
allowed-tools: Bash(/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-search:*), Bash(/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-show:*)
---
<!-- agentkb-skill v1 · app 0.3.0 · exe=/Applications/AgentKB.app/Contents/MacOS/AgentKB -->

# 查历史会话（AgentKB）

1. 从用户的话里提 2–4 个具体关键词（报错类名、组件名、中文名词），**不要传整句话**。
2. 搜索：`/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-search "<关键词>"`
   可选：`--source claude|codex`、`--project <名>`、`--since 30d`、`--limit 5`。
3. 看结果里每个会话的 `title`、`last_activity_at`、`hits[].snippet`。没找到或不相关：换关键词（同义词、中英文各一次）再搜，**一共最多 3 次**，仍没有就如实告诉用户没找到。
4. 对最相关的 1–2 个会话，读上下文再回答：
   `/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-show <source>:<session_id> --around <hits[].seq>`
5. 回答：先给结论，再列出处——会话标题、日期、项目，以及结果里的 `resume.command`。`resume.available` 为 false 时说明原会话已删除，只能用 `--kb-show` 看备份内容。

注意：
- 调用命令时不要加管道、`cd` 或其他命令拼接（会让预授权的前缀匹配失效，又弹确认）。
- 不要自己 grep 或读取 `~/.claude`、`~/.codex`、`~/AgentKB`，只用上面两个命令。
- 结果是过去的记录，可能已过时（看 `data_as_of` 和会话日期），据此动手前先到代码里核实。
- 搜索产生的内容不会被再次索引，不用担心循环。
- 命令不存在时（App 被移动）：`mdfind "kMDItemCFBundleIdentifier == 'io.github.moqi777.agentkb'"` 找到 AgentKB.app，把上面路径换成它里面的 `Contents/MacOS/AgentKB`。
```

约 330 字（不含 frontmatter）。设计意图：触发词写进 `description`（两个 Agent 都按它决定是否加载）；正文只有「怎么搜、怎么读、怎么答」，不放数据格式细节（JSON 字段名自解释）。

### 8.2 安装和卸载（`kb/skill.rs`，新文件）

- 位置：`<HOME>/.claude/skills/agentkb/SKILL.md`、`<HOME>/.codex/skills/agentkb/SKILL.md`（`CLAUDE_CONFIG_DIR`/`CODEX_HOME` 环境变量存在则用它，和 P2 `schedule.rs` 一致）。
- **状态**（按目录逐个判断）：`not_installed`（目录不存在）；`current`（有标记注释 `agentkb-skill v1 · app <ver> · exe=<路径>`，版本和当前 App 一致、路径等于当前可执行文件且存在）；`stale`（有标记，但版本较旧或路径已不存在/不是当前路径）；`foreign`（目录存在但没有我们的标记——比如用户自己建了个叫 agentkb 的 skill，**不覆盖、不卸载**，界面提示）。
- **安装**：只在用户点按钮时；先检查当前可执行文件稳定（沿用 P2 的 `ExeStability`/`classify_exe`：debug 构建或临时路径拒绝，因为路径会写进 skill）；渲染模板 → 写到同目录临时文件 → fsync → `rename` 到 `SKILL.md`（`fsutil::write_atomic`）。`skills/` 目录不存在才创建（PLAN 例外覆盖的是 `skills/agentkb/`，创建父目录是它的必要前提，两个目录这台机器上都已存在）。`agentkb` 如果是符号链接，拒绝。
- **卸载**：只在目录有我们的标记时才删：删 `SKILL.md`，目录为空则删目录；`remove_dir_all` 不用，逐个删已知文件，避免用户在里面放了别的东西被误删；路径用字面量拼接并校验以 `…/skills/agentkb` 结尾。
- **测试按钮**：实际执行一次当前可执行文件的 `--kb-search`（取最近一个会话的标题里的一个词），显示「找到 N 个会话，用时 X 毫秒」；并检查 skill 文件里的路径存在。不调用 Agent。
- **升级**：`stale` 在首页和设置页提示一次，用户点「更新」才重写（同安装）。SKILL.md 里的兜底（8.1 最后一条）保证不更新也不至于完全失效。**不在 App 启动时自动写**（PLAN 例外只允许用户点安装时写）。
- 测试中**不碰真实目录**：所有单元测试用 `tempdir` 当 HOME；真机安装由你点。步骤 5 的「前后快照对比」用 `find ~/.claude ~/.codex -newer … -type f`/文件清单对比，在临时 HOME 里装一遍并证明除 `skills/agentkb/` 外零变化。

## 9. 首页搜索

### 9.1 界面（`src/kb/KbSearch.tsx`，挂在 `KbHome` 筛选行上方）

```
┌ 知识库 ───────────────────────────────  [立即收集] [设置] ┐
│ 状态区（上次收集 / 定时 / 备份 …，不变）                      │
│ ┌ 🔍 搜索会话内容（中文、英文、类名、报错都行）──────────┐ ✕ │
│ 来源[全部▾] 客户端[全部▾] 项目[全部▾] 时间[全部▾]   □含子代理 │
│ ─────────────────────────────────────────────────────  │
│ 找到 11 个会话 · 数据截至 08:30                              │
│ ▸ 排查 Ghostty 卡死        [codex] vscode · ghostty-debug · 9月9日 │
│    …不能仅凭 objc_retain 宣布 ARC 或 WindowServer 已【死锁】…（助手）│
│    …检查是否存在锁等待链…【死锁】…（助手）           还有 1 处命中   │
│ ▸ …                                                      │
└────────────────────────────────────────────────────────┘
```

- 搜索框为空：显示原来的会话列表（不变）。有内容：300 ms 防抖或回车触发 `kb_search`，结果列表**替换**会话列表；筛选行复用现有的来源/客户端/项目/时间下拉（`kb_session_facets`），作为 `--source/--client/--project/--since` 传给同一个后端函数。
- 后端新增 Tauri 命令 `kb_search`（桌面版，WebUI 不提供，和 PLAN 第 3 节第 8 条一致），**与 CLI 共用 `kb/search.rs::search()`**，所以 GUI 和 Agent 看到的是同一套排序。GUI 里用 `db::open_readonly`（正常 WAL 读，不用 immutable）。
- 命中词高亮：后端返回 `snippet` 同时给出 `highlights: [[start,end]…]`（字符偏移，不靠前端重新切词），前端用 `<mark>` 渲染。
- 搜索期间的状态：`index_ready:false`（正在建索引）时显示「正在建立搜索索引，稍后可用」，不是空结果；零结果显示建议（放宽筛选/换词）。
- 全部文字走 i18n，5 个语言文件加键（en、zh-CN 真翻译）。

### 9.2 点结果后跳到那条消息

复用 P2 的 `kb://` 打开方式（`src/kb/openSession.ts::openKbSession`）：打开会话后，用底座已有的 `navigateToMessage(uuid)`（`GlobalSearchModal.tsx:346` 的全局搜索就是这样跳的）滚动到消息并高亮。`openKbSession` 增加可选参数 `{ focus: { key, ts, kind } }`，打开并加载消息后解析出 uuid 再调用 `navigateToMessage`，找不到就停在会话顶部并提示一次「没能定位到具体消息」。

### 9.3 一个需要说清楚的坑：消息标识

底座 Claude 消息的 `uuid` 就是 JSONL 行的 `uuid` 字段，所以 `messages.msg_key` 存它，点击直接跳。**Codex 不行**：`providers/codex.rs::convert_codex_item`（约 1262 行）的 `uuid` 是 `item.id`，没有就是 `codex-<计数器>`，计数器是转换过程里的内部序号，我们的 `seq` 对不上。所以对 Codex，`msg_key` 为空，前端按 **（行时间戳 `ts` + 类型 user/assistant + 文本开头 40 字符）** 在已加载的消息里匹配（时间戳精度到毫秒，同毫秒同类型的消息极少）；匹配不上就退化到按时间戳最近的一条。这一块步骤 3 实现时用真实 Codex 会话逐个验证，写进报告。

## 10. 改动清单与新依赖

**新增文件（Rust，`src-tauri/src/kb/`）**：`redact.rs`（脱敏，第 4 期复用）、`index/messages.rs`（抽取规则 + 轮状态机）、`index/tokenize.rs`（切词）、`search.rs`（查询、合并、排序、片段、高亮；CLI 和 GUI 共用）、`skill.rs`（模板、状态、安装/卸载/测试）。

**修改文件（Rust）**：`db.rs`（迁移 v2、`purge_messages`、插入消息的函数）、`index/mod.rs` + `index/claude.rs` + `index/codex.rs`（Indexer 产出消息行、新增状态字段）、`sync.rs`（`INDEXER_VERSION` → "4"、`commit_file`/`reindex_one` 写消息、`--kb-reindex`）、`cli.rs`（`--kb-search`、`--kb-show`、`--kb-reindex`）、`commands.rs` + `lib.rs`（`kb_search`、`kb_skill_status/install/uninstall/test` 命令）、`mod.rs`（模块声明、`AGENTKB_VERSION` → "0.3.0"）。

**新增/修改文件（前端 `src/kb/`）**：`KbSearch.tsx`、`useKbStore.ts`（搜索状态）、`api.ts`/`types.ts`、`openSession.ts`（`focus`）、`KbSettings.tsx`（「让 Agent 能查历史」区块）、`KbWizard.tsx`（最后一页）、`KbHome.tsx`（搜索框；首页说明换行修复；skill 过期/升级后首次提示）；i18n `kb.json` 五个语言。

**底座文件的最小改动**：无新增（`navigateToMessage` 是已有接口）。

**新依赖**：`jieba-rs = "0.7"`（MIT；+约 5 MB 二进制；不开 textrank/tfidf）。SQLite FTS5 由已有的 `rusqlite` bundled 提供，不加新 crate。前端无新依赖。

**步骤 0 已做**：`KbHome.tsx` 说明文字加 `text-wrap: balance`（避免句号单独落到第二行）。

## 11. 风险与已知限制

- **改写文件的旧内容不进索引**（1.4）；**Codex 分叉会话**（resume 产生的新文件会复制历史）会让同一段内容出现在两个会话里，搜索可能出现两条相近的结果，本期不去重（结果里有各自的标题和时间，用户能分辨）。
- **immutable 回退**的窄窗口（6.2）。
- **Claude 首次 `Skill(agentkb)` 会问一次**（6.1）。
- 中文分词是词典切词，新词/行话（内部系统名）可能切碎，英文标识符不受影响；用户词典留作后续。
- 索引依赖抽取规则，Claude/Codex 日志格式变化时抽取会漏（未知类型已有计数，`unknown_types`）；`INDEXER_VERSION` 机制保证修规则后一次重建。
- 测试会话会被 AgentKB 收进真实库（6 节开头的说明）；我不会删除它们。
- 【审核修订】第一次在真实 `~/AgentKB` 上建索引之前先备份 `kb.db`，并在报告里写真实库的索引大小和耗时；Agent 真实测试沿用本文第 6 节的做法（项目级 skill / 临时 `CODEX_HOME`），不往真实 `~/.claude/skills`、`~/.codex/skills` 装东西。
