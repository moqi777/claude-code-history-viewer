# 第 5 期设计说明：一键导出 / 导入（AgentKB 0.5.0）

状态：**等待 leader 审核（检查点）**。本文只写设计，没有开始实现。步骤 0（技能不再因为 App 版本变化而提示更新）已另行提交。

文中"实测"的数字都来自真实库的副本（临时目录，只读源；用命令行工具 `sqlite3`、`tar`、`zstd` 和已安装的 0.4.1 做代理测量）。真正的导出、导入代码做出来之后，会在报告里用同样的方法重测一遍。

## 0. 先说三件需要 leader 拍板的事

| # | 事项 | 我的建议 |
|---|---|---|
| A | 打包格式：tar 还是自己定义 | 自己定义一个很小的"条目流"，外面包一层 zstd，**不加新依赖**（理由见 §1.2） |
| B | 导出期间能不能让定时收集照常跑（验收第 6 项要求） | 锁只在"拍快照"那几秒里拿，之后用硬链接固定住文件再慢慢写（§2）。硬链接建不了的文件系统，退回"整个导出期间拿锁" |
| C | 同一个会话两边都有，一边是另一边的开头，**但本机的源文件还在** | 这种情况不能安全地"留长的"（会和收集的游标打架，§5.3 说明），建议**两份都留**；只有本机源文件已经不在了，才把长的接在短的后面。请确认 |
| D | 加密 | 第一版不做，格式里预留标志位（§7） |

另有一处和任务文件的字面要求略有不同，先说明：任务里写"schema 版本不一样就从 `raw/` 和 `notes/` 重建"。我的做法是：快照比本机**旧**，就用现有的迁移（`db::open` 里的 `migrate`）升级，消息索引版本落后的部分由下一次收集的 `reindex_if_outdated` 补；比本机**新**，直接拒绝并提示先升级 AgentKB。没有"凭空从 `raw/` 重建 `files` 表"这一步，因为 `files.source_path`、`head_hash`、`src_mtime_ns` 这些游标无法从镜像文件里恢复，硬造出来会让后面的收集判断出错。依据和实测见 §3.4。

## 1. 文件格式

### 1.1 结构

一个 `.agentkb` 文件，从头到尾：

```
偏移 0   固定头 16 字节：
           "AGENTKB\0"        8 字节魔数
           format             u16 小端，格式版本，现在是 1
           flags              u16 小端，bit0 = 已加密（第一版恒为 0，见 §7）
           保留               u32，写 0
偏移 16  负载：一个 zstd 流（开了内容校验和），解压后是"条目流"
结尾     尾部：
           manifest           UTF-8 JSON
           manifest_sha256    32 字节（对上面的 manifest 字节算的）
           manifest_len       u64 小端
           "AGENTKB\0"        8 字节魔数（再写一遍）
```

**manifest 放在尾部**：写文件时各文件的 SHA-256 是边写边算的，放尾部就不需要先读一遍再写一遍。读 manifest 只要读最后几十 KB：先读最后 16 字节拿到长度，再往前读（`--kb-import --inspect` 和界面里的"看一眼文件内容"都是这样做的，不用解压）。尾部魔数不对、长度不合理、manifest 的 SHA 不符，都说明文件没写完或被截断，直接拒绝。

**条目流**（解压后）：

```
重复：  kind u8 = 1（文件）| path_len u16 | path（UTF-8，用 "/"）| size u64 | 内容 size 字节
结束：  kind u8 = 0
```

顺序固定为：`kb.db`、`notes/**`、`raw/**`（文件名排序）。未知的 `kind` 一律报错拒绝，不跳过。

条目里的路径统一是相对路径、`/` 分隔，不带权限位、属主、时间、符号链接。导入时一条一条检查：不能是空、不能以 `/` 开头、不能含 `..` 或 `.` 或空的段、不能含 `\`、`:`、控制字符，段不能是 Windows 保留名，同一个包里大小写不敏感地不能重名。这些检查复用 `paths::safe_join`（`src-tauri/src/kb/paths.rs:75`）再加几条。这样同一个文件第 7 期可以原样在 Windows 上导入。

### 1.2 为什么不用 tar

| | 自己定义的条目流 | tar + zstd |
|---|---|---|
| 新依赖 | 无（`zstd`、`sha2` 已在 `Cargo.toml`） | 要加 `tar` crate（当前 `Cargo.lock` 里没有） |
| 路径长度 | 任意（`u16`） | 老式头 100 字节，Claude 的项目目录名很长，要靠 GNU/PAX 扩展头 |
| 不该出现的东西 | 格式里根本没有符号链接、设备文件、权限位、硬链接 | 这些都要在导入时逐一拒绝，容易漏 |
| 能不能用别的工具打开 | 不能（要用 AgentKB） | 能，`tar` 可以解 |
| 代码量 | 读写各约 100 行，含测试 | 少一点，但安全检查一样多 |

"能用别的工具打开"对用户的价值不大（里面本来就要靠 `kb.db`+游标一起用），而"格式里不可能出现符号链接和路径穿越"对安全更有用，所以选自己定义。代价是我们要自己写好、测好读写和损坏处理。

### 1.3 manifest 完整示例

数字取自真实库（313 个会话、116 条笔记），只改了名字：

```json
{
  "format": 1,
  "min_reader_format": 1,
  "kind": "agentkb-export",
  "export_id": "01JA4Z3K8Q-3f9c1e",
  "exported_at": "2026-10-16T08:40:12Z",
  "app": { "name": "AgentKB", "version": "0.5.0" },
  "schema_version": 5,
  "source_device": {
    "name": "MacBook-A",
    "os": "macos",
    "home": "/Users/a",
    "claude_root": "/Users/a/.claude",
    "codex_root": "/Users/a/.codex"
  },
  "counts": {
    "sessions": 313,
    "notes": 116,
    "sessions_with_notes": 19,
    "raw_files": 527,
    "note_files": 19
  },
  "totals": { "uncompressed_bytes": 3893301842, "payload_files": 548 },
  "compression": { "codec": "zstd", "level": 3 },
  "encryption": { "scheme": "none" },
  "settings": {
    "interval_secs": 600,
    "sources_enabled": { "claude": true, "codex": true },
    "exclude_prefixes": [],
    "exclude_cwd_prefixes": ["/Users/a/Dev/proj/scratch"],
    "digest": {
      "engine": "claude", "model": "", "time": "06:40",
      "max_sessions_per_round": 30, "recent_days": 7, "quiet_minutes": 30,
      "min_messages": 4, "max_chunks_per_session": 3, "codex_reasoning": "low"
    }
  },
  "files": [
    { "path": "kb.db", "size": 116555776, "sha256": "…64 位十六进制…" },
    { "path": "notes/claude/0194….json", "size": 4210, "sha256": "…" },
    { "path": "raw/claude/projects/-Users-a-Dev-…/0194….jsonl", "size": 918233, "sha256": "…" }
  ]
}
```

要点：

- `files` 列的是**包里实际的内容**，`size`/`sha256` 是导出时算的，导入时逐个核对。`raw/` 里被数据库游标记录了长度的文件，只导出到 `mirror_offset`（见 §2.2），所以这里的 `size` 可能比磁盘上的文件小。
- `settings` 只有与机器无关的部分（§3.5）。数据目录、`mirror`、`cli_path`、`schedule_enabled`、`digest.enabled` 不写进去。
- `source_device` 是路径换算（§4.1）的依据；`name` 取主机名，会写进文件，导出界面会显示出来。
- `counts` 是验收对账用的；`sessions` 取 `SELECT COUNT(*) FROM sessions`，`notes` 取 `notes` 表行数。

### 1.4 格式以后怎么升级

- `format` 是整数。**读的规则**：`format > 本程序支持的最大值` 或 `min_reader_format > 支持的最大值` → 拒绝并提示"这个文件是更新版本的 AgentKB 导出的，请先升级"。`format` 相同或更小 → 按对应版本的规则读。
- manifest 里的未知键读的时候忽略（加字段不用升格式）；条目流里的未知 `kind` 不忽略（那是结构变化，必须升 `format`）。
- 加密、分卷这类以后的功能：`flags` 里还有 15 位，`encryption.scheme` 现在恒为 `"none"`，读到别的值现在就拒绝。
- `schema_version` 和 `format` 无关：`format` 管"包怎么读"，`schema_version` 管"里面的 `kb.db` 是哪一版"，见 §3.4。

## 2. 导出流程

### 2.1 锁和快照

**为什么不能整个导出过程都拿着收集锁**：`sync::run_sync` 拿不到锁就按 `LOCK_WAIT = 3s` 放弃，返回 `skipped: locked`（`sync.rs:158`，`LOCK_WAIT` 在 `sync.rs:25`）。导出要读 3.9 GB、写 2.1 GB，哪怕只有十几秒，期间的定时收集就会被跳过。验收第 6 项要求的是"导出进行中定时收集照常完成"，所以锁只能拿很短。

做法（新模块 `src-tauri/src/kb/export.rs`）：

1. **先看整理有没有在跑**：`lock::held_by_live_process(&data.digest_lock())`（`lock.rs:96`，`digest/mod.rs:882` 同样在用）。有 → 返回"正在整理，等它完成后再导出"（带 pid），不等待、不打断。
2. **拿两把锁**：`SyncLock::acquire(&data.lock(), 60s)`、`SyncLock::acquire(&data.digest_lock(), 0)`（与 `migrate_dir::migrate_data` 同一对锁，`migrate_dir.rs:234-241`）。拿不到收集锁（比如收集正好在跑）最多等 60 秒；整理锁在第 1 步之后仍然被抢走则报同样的提示。
3. **拿着锁做四件事，然后立刻放锁**：
   - `VACUUM INTO '<staging>/kb.db'`：一个一致的、紧凑的快照（实测见 §6，1.0 秒，215 MB → 116 MB）。`VACUUM INTO` 在一个读事务里完成，不阻塞也不受后面写入影响。导出的这一份里同时做两件清理：删掉 `digest_rounds`、`digest_items`（运行状态，换机器没有意义，§3.3）和 `meta` 里与机器有关的键。这些清理在 staging 里的副本上做，不动本机的库。
   - 列出 `raw/`、`notes/` 里所有文件，记下每个文件的**要导出的长度**（§2.2）。
   - 把每个 `raw/` 文件在 `<staging>/links/` 下**建一个硬链接**（同一个文件系统，不占空间，527 个文件几毫秒）。
   - 把 `notes/` 整个复制到 staging（共 248 KB）。
4. 放锁。从这一刻起收集、整理可以照常跑。

**为什么硬链接能让后面的读取保持一致**：`mirror.rs` 里对镜像文件只有三种写法——在末尾追加（同一个 inode，我们读到记录的长度为止就不受影响）；重写时把旧镜像改名为 `.prev-*` 并新建一个同名文件（`mirror.rs` 开头注释；我们的硬链接仍指向旧 inode）；崩溃恢复时截掉游标之后的孤儿字节（我们记录的长度不超过游标）。三种情况下，硬链接指向的内容在"记录的长度"范围内都不会变。

**硬链接建不了**的情况（数据目录在 exFAT 的移动硬盘上、网络盘等）：退回"整个导出期间拿着锁"，界面上提示"这次导出需要暂停收集，约 X 秒"，不会静默变慢。

### 2.2 要导出多长

- `raw/` 里被 `files.mirror_rel` 引用的文件：导出 `min(磁盘大小, mirror_offset)`。`mirror_offset` 是数据库认可的有效长度（`db.rs` 的 `FileRow.mirror_offset`）；游标之后的是上次崩溃留下的孤儿字节，不该进备份。压缩镜像（`is_compressed`，Codex 的 `.jsonl.zst`）是整文件，按磁盘大小。
- 没被引用的文件（比如 Codex 已经压缩过之后，旧的明文兄弟文件；`.prev-*` 旧镜像）：整个文件。**这些是备份的一部分，不能丢。**
- 排除：`*.tmp`、`kb.db*`（旧备份）、`logs/`、`digest-work/`、`kb.lock`、`digest.lock`、`.agentkb-*`。

### 2.3 流式写

- 写到 `<目标>.partial`，同目录，权限 `0600`（`OpenOptionsExt::mode`，Windows 忽略）。
- 读文件 → 算 SHA-256 → 写入 zstd 编码器 → 写文件。每次 1 MiB 缓冲，**内存与库大小无关**（`fsutil::sha256_file` 已有同样思路，`fsutil.rs:98`）。zstd 用已有依赖 `zstd 0.13`，开 `include_checksum(true)`，单线程（实测见 §6，3.9 GB 约 5.5 秒，不需要多线程）。
- 全部写完：写尾部 → `fsync` → `rename` 成最终名字。目标已存在时由界面先问"覆盖吗"，程序层面**不覆盖**已存在的文件，除非传了覆盖开关。
- 随手核对：写入的字节数、文件个数要和 `counts` 一致；不一致就当作失败，删掉 `.partial`。
- 空间检查：目标卷的 `fsutil::free_space`（`fsutil.rs:156`）小于"未压缩总量的 60%"就先警告（实测压缩后是 54%）；真写满了就清理 `.partial` 并报错。

### 2.4 进度和取消

- 进度复用 `progress::Progress`（`progress.rs`），`op = "export"`，`phase`：`snapshot` → `write` → `finish`；`done/total` 是文件个数，`bytes_done/bytes_total` 是未压缩字节。通过现有的 `kb-progress` 事件发给界面，`--kb-export` 命令行把进度打在 stderr 上。
- 取消：一个 `Arc<AtomicBool>`，写循环每个 1 MiB 块检查一次。取消 = 关掉写入、删除 `.partial`、删除 staging（含硬链接）；此时锁早已放掉，不需要回滚任何数据。界面在写的阶段显示"取消"按钮，快照那几秒不能取消（只有几秒）。
- 任何错误路径（包括 panic 之外的所有 `?`）都走同一个清理：删 `.partial`、删 staging。staging 放在数据目录下的 `.export-<id>/`，如果进程被杀，下次导出开始时扫描并清理 24 小时前的残留。

### 2.5 对真实数据的保护

导出对 `~/AgentKB` 只有读（以及 staging 里的硬链接，导出后删掉）。硬链接不改原文件。拿锁只是在 `kb.lock`/`digest.lock` 上加 `flock`，和现有的收集互斥方式一致。

## 3. 导入流程

新模块 `src-tauri/src/kb/import.rs`（读包、校验、规划、应用）和 `merge.rs`（逐个文件的合并规则）。

### 3.1 共同的前半段：读、校验、解到临时目录

1. **读 manifest**（只读尾部）：检查魔数、`manifest_sha256`、`format`、`min_reader_format`、`encryption.scheme`、`schema_version ≤ 本程序 latest_version()`（`db.rs`）。任何一项不对：拒绝，**到这一步为止没有写过任何东西**。
2. **空间检查**：`totals.uncompressed_bytes × 1.1` 与 staging 所在卷的剩余空间比较（与 `migrate_dir::check` 同样的 110%，`migrate_dir.rs:28`）。
3. **拿锁**：数据目录的收集锁（最多等 60 秒）和整理锁（整理在跑 → "等整理完成后再导入"）。
4. **解到临时目录** `<data_dir>/.import-<export_id>/`（与数据目录同一文件系统，最后用 `rename` 切换）。边解边算 SHA-256、数字节，和 manifest 里每个文件的 `size`/`sha256` 比；多出来的条目、缺的条目、不一致的内容，一律失败。zstd 自带的内容校验和先于 SHA 发现损坏。**改掉导出文件的任何一个字节，到这里一定失败**：头部 → 魔数/格式；负载 → zstd 校验和或 SHA；尾部 → manifest 的 SHA。
5. **整理临时目录里的 `kb.db`**（在临时目录里做，不碰数据目录）：
   - `db::open` 打开并迁移到本程序的最新 schema（§3.4）；
   - 路径换算（§4.1）；
   - 标记"来自另一台设备"（§4.2）；
   - 清理机器相关的 `meta` 键：`exclude_cwd_applied`（让下一次收集按本机的排除规则重新计算）、`digest_first_round_seen`；
   - `PRAGMA integrity_check`，必须是 `ok`；
   - 用 `notes::rebuild_all`（`notes.rs:353`）从临时目录的 `notes/*.json` 重建 `notes`、`notes_fts`、`digest_sessions`——**笔记 JSON 是可信来源，库里的是派生的**（`db.rs` v4 注释）。重建后条数要和 manifest 的 `counts.notes` 一致。

到这里，数据目录仍然一个字节都没有变。后半段分两种。

### 3.2 空库导入（数据目录里没有任何会话、笔记）

"空"的判断：`raw/` 和 `notes/` 下没有文件，且 `kb.db` 不存在或 `files` 表为空。（向导第一页走的就是这种。）

切换步骤：

1. 如果有一个空的旧 `kb.db`，先改名为 `kb.db.before-import-<时间>`（沿用 `backup.rs` 里已有的备份命名习惯）。
2. `rename` 临时目录里的 `raw/`、`notes/` 到数据目录；
3. 最后 `rename` `kb.db`（同时清掉旧的 `kb.db-wal`/`kb.db-shm`）。
4. 写设置（§3.5），写 `config.json` 用现有的原子写（`fsutil::write_atomic`），写之前把原来的 `config.json` 另存为 `config.json.before-import-<时间>`。
5. 删临时目录，放锁。

**中断**：步骤 1-3 每一步都是单个 `rename`（原子）。顺序保证"数据库最后换"：中途被杀，最坏情况是 `raw/`、`notes/` 已经就位而 `kb.db` 还是旧的/没有。此时再导入一次：`raw/` 非空，走合并路径（§3.3），逐个文件比对发现全部"相同"，跳过，最后把库补上。这一点会有一个测试。解包阶段被杀：只留下 `.import-*` 临时目录，数据目录不变，下次导入开头清理它。

### 3.3 合并导入（本机已有数据）

1. 前半段一样，但临时目录里的 `kb.db` 只当作**数据来源**，不替换本机的库。
2. **先备份本机的库**：`online_backup`（`backup.rs:434`）→ `kb.db.before-import-<时间>`（完整一致的副本）。
3. **规划**（只读，不改任何东西）：对包里每个文件，按 §5 的规则得出动作：`skip`（相同）、`add`（本机没有）、`extend`（本机是开头，接上后半段）、`keep_both`（两份都留）。同时规划笔记。得到的计划可以直接给用户看（"新增 23 个会话、跳过 290 个相同、2 个需要两份都留、笔记 5 条冲突"）。
4. **执行文件操作**，每个文件独立、可重复执行：
   - `add`/`keep_both`：`fsutil::copy_atomic`（临时文件 + `rename`，`fsutil.rs:53`）放进 `raw/`；
   - `extend`：按已有的"先追加并 fsync，再推进游标"的顺序，用 `fsutil::append_tail`（`fsutil.rs:79`）把后半段追加到本机镜像文件末尾；
   - 本机文件的已有字节一个也不改、不删、不改名。
5. **数据库在一个事务里**更新：插入/更新 `files` 行（`db::insert_file`，`db.rs`），`mirror_offset` 推进到新长度；然后对每个受影响的文件调用 `sync::reindex_one`（`sync.rs:610`）生成消息和全文索引——这是现成的，不另写索引代码。
6. **笔记**：按 §5.5 规则写 `notes/` 里的 JSON（冲突的另存到 `notes/.conflict-<时间>/`），再 `notes::rebuild_all`。
7. 设置按 §3.5 合并，写 `config.json`（先另存 `.before-import-*`）。

**失败怎么办**：步骤 4 的新增文件如果在数据库更新之前被打断，只是多了几个没有数据库行的文件，下次导入会判定为"相同"并补上行。步骤 5 是一个事务，失败整体回滚。步骤 4 唯一会改动已有文件的是 `extend`（只在末尾追加），崩溃后的孤儿尾巴由 `mirror::execute` 里已有的"游标之后截断"逻辑处理（`mirror.rs` 的 `Append` 分支），与普通收集中途被杀的情况完全一样。

### 3.4 schema 版本不同怎么办

| 包里的 `schema_version` | 做法 | 依据 |
|---|---|---|
| = 本机 | 直接用 | — |
| < 本机（旧） | 在临时目录里 `db::open` → 自动按 `MIGRATIONS` 升级（`db.rs:13-155` 的 append-only 迁移）。消息索引的版本（`meta.indexer_version`）落后的，导入后的那次同步由 `reindex_if_outdated` 自动补（`sync.rs:543`） | 这和 App 升级时库自动升级是同一条路径，已经在 v3→v4 上用过 |
| > 本机（新） | 拒绝，提示"这个文件来自更新版本的 AgentKB（schema N），请先升级本机" | `db::open` 对更新的库本来也拒绝 |

"重建索引"的实测：对真实库副本跑 `--kb-reindex`（把 8.6 万条消息的索引全部重做），**10.9 秒**。这是旧快照需要补索引时的最坏耗时。

### 3.5 设置

包里的 `settings` 只有：`interval_secs`、`sources_enabled`、`exclude_prefixes`、`exclude_cwd_prefixes`、`digest` 的参数部分（不含 `enabled`、`cli_path`）。

| 情况 | 做法 |
|---|---|
| 本机还没有 `config.json`，或者是向导里导入 | 采用包里的设置；`digest.enabled = false`；数据目录用本机选的（向导里是默认或用户选的），`mirror`、`cli_path`、定时任务都是空，由向导/设置页自己完成 |
| 本机已有设置 | 默认只**合并排除规则**（两边取并集，去重）；其他数值保持本机的。界面上有一个选项"也采用文件里的收集间隔和整理参数"，默认不勾 |
| 任何情况 | `digest.enabled` 一律为 `false`（已有设置里本来开着的，保持本机的值不动——我们不替用户关掉本机正在用的整理）；`digest.cli_path`、`data_dir`、`mirror`、`schedule_enabled` 永不从文件里取 |

**说明**：任务里写"导入后 `digest.enabled` 一律设为 `false`"。我理解为"从文件来的设置不能把它打开"；如果本机原来就是开着的，合并导入不会替用户关掉。这一点请 leader 确认（报告里会把行为写清楚）。

排除规则里的绝对路径按 §4.1 换算后再合并。

导入后：**立刻跑一次正常同步**（§3.6）。

### 3.6 导入后要不要立刻同步一次

要，原因有三：① 让本机上"碰巧也有"的源文件被认回来（§4.2）；② 按本机的排除规则重新计算 `cwd_excluded`；③ `reindex_if_outdated` 补旧版本的消息索引。界面上导入成功后自动触发一次 `run_sync(.., "import")`；`--kb-import` 默认也做，传 `--no-sync` 可以关。同步失败不影响导入结果（导入已经完成并校验），只在界面提示。

## 4. 路径换算和"来自另一台设备"

### 4.1 路径换算

对 `kb.db` 里的 `files.source_path`、`sessions.cwd`，以及设置里的 `exclude_prefixes`、`exclude_cwd_prefixes`，按下面的顺序换算（新函数 `import::convert_path`，纯函数，单测覆盖）：

1. 以 `source_device.claude_root + "/"` 开头 → 换成本机的 Claude 根目录（`SourceRoots::resolve`，`config.rs:456`）+ 剩余部分；
2. 以 `source_device.codex_root + "/"` 开头 → 同理换成本机的 Codex 根目录；
3. 以 `source_device.home + "/"` 开头 → 换成本机家目录（`crate::utils::home_dir`）；
4. 以上都不是 → 原样保留。

例：`/Users/a/.claude/projects/x/s.jsonl` → `/Users/b/.claude/projects/x/s.jsonl`；`/Volumes/Disk/proj` 不动。导出的机器与本机操作系统不同（路径风格不同）时一律不换算，原样保留。

`mirror_rel` 本来就是相对 `raw/` 的路径，不用换。`sessions.cwd` 换算只是为了显示和排除规则对得上；一旦源文件在本机出现，下一次收集会按文件内容里的真实 `cwd` 覆盖它。

### 4.2 "来自另一台设备"的状态

**需要改表**：schema v5，一条迁移：

```sql
ALTER TABLE files ADD COLUMN origin TEXT;   -- NULL = 本机收集的；否则 = 来源设备名
```

不改 `source_status` 的 CHECK（SQLite 改 CHECK 要重建整张 `files` 表，而 `messages` 等通过外键依赖它，代价大，风险也大）。

规则：

- 导入时，一个文件行换算路径后，**本机上这个路径存在** → `origin = NULL`（当作本机收集的，下一次同步按游标核对，内容是开头关系就接着追加，对不上就按现有的"重写"流程保留旧镜像为 `.prev-*`）；**不存在** → `origin = 设备名`，`source_status` 保持包里的值（`present`；包里本来就是 `deleted` 的保持 `deleted`）。
- **收集不能把它当成已删除**：`sync::reconcile`（`sync.rs:337`）里 `vanished` 的条件加上 `row.origin.is_none()`。`FileRow` 增加 `origin: Option<String>` 字段。
- **认回来**：同一个路径后来出现了源文件，`db.rs` 里更新游标的几条语句（`db.rs:367`、`389`、`418`）顺手 `origin = NULL`。从此就是普通的本机会话。
- 对外：`source_status` 在 `--kb-search`/`--kb-show`/界面里多一个取值 `elsewhere`（查询里 `CASE WHEN f.origin IS NOT NULL THEN 'elsewhere' ELSE f.source_status END`），同时给出 `origin`（设备名）。
  - `search::resume_for`（`search.rs:571`）：`elsewhere` → `resume.available = false`，提示"这个会话来自另一台设备（X），本机没有它的源文件，不能恢复；用 `--kb-show <id>` 读取备份"。
  - 首页（`KbHome.tsx:777`）、搜索（`KbSearch.tsx:201`）：`deleted` 旁边多一个灰色徽标"来自另一台设备"；更多筛选里加一个"只看来自另一台设备的会话"。`KbSourceStatus` 类型（`src/kb/types.ts`）加 `"elsewhere"`。
  - `status.rs:105` 统计 `deleted` 的地方不变（"源文件已删除"仍然只表示本机上被删除的）。
- 老版本（0.4.x）的 skill 读 `results[].resume`，`resume.available=false` 时不会尝试恢复；`source_status` 多一个新值不会让它出错（它只在 `deleted` 时有特殊说明）。

**已知局限**：如果本机上出现的源文件路径和导入时换算的路径不一样（比如用户把 `~/.codex/sessions/2026/10/16/x.jsonl` 放到了 `archived_sessions/`），收集会把它当作新文件，同一个 `session_id` 会有两行。这和现在"同一个会话在两个目录里"的已知重复（P4 记下的那个子代理会话）是同一类问题，本期不处理。

## 5. 合并规则

**匹配**：以 `(source, mirror_rel)` 为准找本机对应的文件（`mirror_rel` 同一个 App 里是由会话文件的路径决定的，同一个会话文件在两台电脑上相同）。比较内容用 `mirror::content_continues`（`mirror.rs:226`，P2 里用来判断"旧镜像是不是新文件的开头"，能处理明文/`.zst` 两种存储）。

### 5.1 会话文件

| 情况 | 例子 | 结果 |
|---|---|---|
| A. 只有包里有 | 旧电脑上的 2025 年的会话，新电脑没有 | `add`：复制进 `raw/`，新增 `files` 行，`origin` 按 §4.2 |
| B. 只有本机有 | 新电脑上新聊的 | 不动 |
| C. 两边一样 | 同一个文件，字节相同（大小和 SHA 相同） | `skip` |
| D. 包里是本机的开头 | 包里 120 KB，本机 150 KB，前 120 KB 相同 | `skip`，本机保留 |
| E. 本机是包里的开头，**本机的源文件已经不在了**（`origin` 非空，或 `source_status = deleted`） | 本机 100 KB，包里 130 KB | `extend`：把包里 100 KB 之后的 30 KB 追加到本机镜像，游标推进。本机原有的 100 KB 一个字节不动 |
| F. 本机是包里的开头，**本机的源文件还在** | 同一个会话在两台电脑上都继续聊了 / 用户把 `~/.claude` 拷过去了 | `keep_both`：见 §5.3 |
| G. 两边对不上（谁也不是谁的开头） | 同名但内容分叉 | `keep_both`：包里那份存为 `raw/<目录>/<名>.imported-<export_id>.<扩展名>`（扩展名保持 `.jsonl`/`.zst`，这样能被索引），新增独立的 `files` 行，`origin` 设为设备名。**本机的文件和行完全不动**。做法参照 P2 备份冲突的处理 |
| H. 存储形式不同（一边明文 `.jsonl`，一边 `.jsonl.zst`） | Codex 压缩过的 | 用 `content_continues` 比较解压后的内容，按 C/D/E/F/G 处理；E 的追加只在两边都是明文时做，否则当作 `keep_both` |

### 5.2 为什么 F 不能"留长的"

镜像文件和 `files` 行的游标（`src_size`、`src_mtime_ns`、`head_hash`、`mirror_offset`）是对着本机那个**源文件**写的：下一次收集 `mirror::decide`（`mirror.rs:51`）看到源文件变化，就从 `mirror_offset` 开始读源文件接着追加。如果我们把镜像追加得比源文件还长，`mirror_offset` 超过源文件已有的长度，后面源文件长出来时，追加的位置就错了，镜像里会混进两台电脑分叉后的内容；如果不推进 `mirror_offset`，崩溃恢复逻辑会把追加的尾巴当孤儿字节截掉。两种做法都会损坏备份，所以 F 选"两份都留"。

### 5.3 F 的代价与待确认

"两份都留"的结果：同一个会话 id 在会话列表里出现两次（和现在已知的那个重复的子代理会话一样）。leader 要求的是"留长的"，F 是这个规则里唯一做不到的格子。备选：对 F 也照 E 处理（追加到镜像、把游标改成"对着镜像"），但需要重新设计游标含义，风险大，建议不做。**请 leader 确认 F 用 `keep_both`。**

### 5.4 `files` 行

- 新增（A、G）：包里的行换算路径后插入。`source_path` 要唯一（`UNIQUE`）：换算后与本机已有行撞了，说明是同一个源文件，按"匹配"处理（它的 `mirror_rel` 也会一样）；`mirror_rel` 唯一：`keep_both` 用 `.imported-<id>` 后缀避免撞。
- `E`：只更新游标。

### 5.5 笔记

笔记文件是 `notes/<source>/<session_id>.json`（`paths::notes_file`，`paths.rs`），一个会话一个文件，里面有 `digested_at`。

| 情况 | 结果 |
|---|---|
| 只有包里有 | 复制过来 |
| 只有本机有 | 不动 |
| 两边都有，内容一样 | 跳过 |
| 两边都有，不一样 | 留 `digested_at` 更新的那份放在 `notes/<source>/<session_id>.json`；另一份存为 `notes/.conflict-<导入时间>/<source>/<session_id>.json`。两份都在磁盘上，不删 |

`digested_at` 相同但内容不同：留本机的，包里的存为 conflict。`notes::rebuild_all` 只读 `notes/<source>/*.json`，不会把 `.conflict-*` 目录当笔记（实现时加测试）。

### 5.6 设置里的排除规则冲突

| 情况 | 结果 |
|---|---|
| 同一条规则两边都有 | 去重，一条 |
| 只有包里有 | 加进来（换算路径后）。导入前的预览里写明"将新增 N 条排除规则：…"，因为它会让本机的一部分会话从搜索里隐藏（数据仍在备份里） |
| 只有本机有 | 保留 |
| 想要"以文件为准" | 不支持。排除是"隐藏"，两边取并集更安全 |

## 6. 用真实数据量出来的数字

真实库 `~/AgentKB`：313 个会话、116 条笔记、`raw/` 527 个文件（明文 `.jsonl` 3.05 GB + `.zst` 0.84 GB，合计 3.6 GiB 磁盘占用）、`notes/` 19 个文件（248 KB）、`kb.db` 215.7 MB（空闲页 25,312 页 ≈ 99 MB）。

测量方法：`kb.db` 复制到 `scratch/`（只读源）；`raw/` + `notes/` 用 `tar -cf - | zstd -3 -T1`（和计划里的 zstd 级别 3、单线程一致）；解包后用 `shasum -a 256` 全量核对；索引用已安装的 0.4.1 在临时 HOME 里跑 `--kb-reindex`。全部在 Apple Silicon、热缓存下测，**冷缓存或移动硬盘会慢**，代码做好后在报告里再用真实实现重测。

| 项 | 实测 |
|---|---|
| `VACUUM INTO` 紧凑快照 | 1.0 秒；215.7 MB → **116.6 MB**（去掉约 99 MB 空闲页） |
| 紧凑快照再 zstd -3 | 0.1 秒；→ **35.7 MB** |
| 导出：`raw/` + `notes/` 打包 + zstd -3 | **5.5 秒**；3.9 GB → **2.107 GB**（54%）。其中 `.zst` 部分（0.84 GB）基本压不动，明文部分约压到 0.4 |
| 估算整个 `.agentkb` | ≈ **2.14 GB**（2.107 + 0.036），导出总耗时 ≈ 7 秒 |
| 导入：解包 | **3.2 秒** |
| 导入：全量 SHA-256 核对 | 8.1 秒（命令行 `shasum` 是 Perl 版本，较慢；Rust `sha2` 估计 3-4 秒） |
| 导入：直接用包里的 `kb.db`（schema 一样） | 解包 3.2 + 核对 ≈ 4 + 库整理（迁移、换算、笔记重建、`integrity_check`）约 1-2 秒 ≈ **8-10 秒** |
| 导入：需要重建消息索引（旧快照） | 上面的 + `--kb-reindex` **10.9 秒**（86,645 条消息） |
| 导入前拿锁的最长时间 | 导出：快照那几秒（1 秒的 `VACUUM INTO` + 硬链接 + 复制 `notes/`）；导入：整个过程（解包 + 切换，约 10 秒量级） |

对账用的数字（导入后要一致）：会话 313、笔记 116、有笔记的会话 19、`raw/` 文件 527、`notes/` 文件 19。

测完已经删除了 `scratch/p5/` 里解出来的原始会话副本（含个人内容）。

## 7. 加密

**建议：第一版不做。**

| | 不加密（建议） | 口令加密 |
|---|---|---|
| 威胁 | 导出文件放在移动硬盘/网盘/拷给别人时，原文（可能含密码、Token）可读。但数据目录本身在电脑上也是明文，并且没有脱敏 | 文件被别人拿到也读不了 |
| 怎么做 | 导出界面明确提示敏感；文件权限 `0600`；建议存到加密的磁盘（FileVault、加密的磁盘映像）；格式预留 `flags` 位和 `encryption.scheme` | 口令 → Argon2id 派生密钥（`argon2 0.5` 已在 `Cargo.toml`，目前是 WebUI 的可选依赖，需要变成常驻依赖）；分块 AEAD（ChaCha20-Poly1305 的流式构造，需要新增 `chacha20poly1305` 依赖）；manifest 也要加密，只有 16 字节头明文 |
| 性能 | 无 | ChaCha20 比 zstd 快，几乎不增加耗时；Argon2id 派生一次约 0.5-1 秒 |
| 忘了口令 | 不存在这个问题 | **永久无法恢复**，而这个文件正是"电脑坏了之后的救命备份"。必须设计成输入两次、强提示，并且无法提供找回 |
| 代价 | 0 | 新依赖 2 个、约 300 行代码、一整套错误提示和测试；口令输入在无界面命令里也要有办法（`--password-file`，否则会进 shell 历史） |

第一版对用户的实际风险是"文件放错地方"，这靠提示和存放位置建议能处理一大半；而加密带来"忘了口令数据就没了"的新风险，和这个功能的初衷（保险）有冲突。建议等有用户明确需要，在格式 `flags` bit0 的位置加上。**由 leader 定。**

## 8. 界面草图

### 8.1 设置页：新的"迁移"分组

```
┌ 迁移 ────────────────────────────────────────────────────────────┐
│ 导出                                                              │
│   把整个知识库（原始备份、笔记、索引、设置）打包成一个文件。          │
│   当前：313 个会话 · 116 条笔记 · 约 2.1 GB（估算）                 │
│   ⚠ 导出文件包含会话原文，没有脱敏，里面可能有密码、Token。          │
│      请当作和数据目录一样敏感的文件保管。                            │
│                                      [ 导出到… ]                  │
│ ─────────────────────────────────────────────────────────────────│
│ 导入                                                              │
│   从另一台设备导出的文件导入。已有的数据不会被删除，两边会合并。      │
│                                      [ 从文件导入… ]              │
└──────────────────────────────────────────────────────────────────┘
```

导出：点"导出到…"→ 系统保存对话框（默认文件名 `AgentKB-2026-10-16.agentkb`）→ 进入进度：

```
正在导出… 拍快照（1/3） → 写入（2/3） → 完成（3/3）
████████████░░░░░░░░  1.4 / 3.9 GB    [ 取消 ]
```

完成：`已导出 313 个会话、116 条笔记，2.14 GB，用时 8 秒。[ 在访达中显示 ]`。
正在整理时：`正在整理（上次开始于 10:51），等它完成后再导出。`
出错：显示具体原因（磁盘满、没有写权限、目标卷被拔出），并说明"没有生成不完整的文件"。

导入：选择文件 → 先**只读 manifest**，弹确认框：

```
导入文件：AgentKB-2026-10-16.agentkb （2.14 GB）
来自：MacBook-A · AgentKB 0.5.0 · 2026-10-16 16:40 导出
内容：313 个会话 · 116 条笔记

和本机合并：新增 23 个会话；290 个已有且相同，跳过；
           2 个需要两份都保留；5 条笔记有冲突（保留较新的，另一份另存）
将新增 1 条排除规则：/Users/a/Dev/proj/scratch
□ 同时采用文件里的收集间隔和整理参数
⚠ 导入前会先备份本机的 kb.db。整理开关保持你现在的设置。
                                         [ 取消 ]  [ 开始导入 ]
```

（空库时没有"和本机合并"这一段，改成"本机还没有数据，将直接导入"。）

进度：`校验 → 解包 → 合并 → 建立索引 → 收集一次`，同样有字节进度和取消（切换前可以取消；开始切换后不可取消，按钮灰掉，切换只有几秒）。
成功：`已导入。会话 336（来自另一台设备 23），笔记 121。整理开关为"关"，请在设置里测试后再打开。`
**校验失败**：`文件已损坏或不完整（第 N 个文件的校验和不对）。没有导入任何内容，你的数据没有变化。` 按钮：[ 好 ]。

### 8.2 向导第一页

在第一页（现在是"数据来源"，`KbWizard.tsx` 的第一个 `Step`）之前增加一个很简单的选择，只在 `config.json` 不存在时出现：

```
欢迎使用 AgentKB
  ( ) 全新开始            —— 从这台电脑上的 Claude Code、Codex 记录开始
  ( ) 从另一台设备导入    —— 选择之前导出的 .agentkb 文件      [ 选择文件… ]
```

选了导入 → 数据目录用向导默认的（或用户在后面步骤改的）→ 空库导入 → 导入完成后向导**跳到最后一步**，设置已经预填（整理为关），用户确认并创建定时任务即可（定时任务是本机的，导入不带）。

### 8.3 首页

- 会话行的徽标：`来自另一台设备`（灰色），替代"源文件已删除"的位置；
- "更多筛选"里多一项"只看来自另一台设备的会话"；
- 状态栏的"已备份 N 个会话"数字包含导入的。

## 9. 改动清单和新依赖

### 9.1 新依赖

**无。** `zstd`、`sha2`、`rusqlite`（含 `backup`）、`fs4`、`chrono`、`tempfile` 都已在 `Cargo.toml`。（若 leader 决定做加密，才新增 `chacha20poly1305`，并把 `argon2` 从可选变成常驻。）

### 9.2 新增文件

| 文件 | 内容 |
|---|---|
| `src-tauri/src/kb/pack.rs` | 容器格式：头、条目流读写、尾部 manifest、`Manifest` 结构、路径检查 |
| `src-tauri/src/kb/export.rs` | 导出：整理锁检查、快照（`VACUUM INTO`、硬链接）、流式写、进度、取消 |
| `src-tauri/src/kb/import.rs` | 导入：读 manifest、解到临时目录并校验、库整理、路径换算、空库切换、设置合并 |
| `src-tauri/src/kb/merge.rs` | 合并规划与执行（§5），规则单测 |
| `src/kb/KbTransfer.tsx`（暂名） | 设置页"迁移"分组、导入确认框、进度 |
| `docs/agent-kb/P5-report.md` | 步骤 3 之后 |

### 9.3 修改文件（尽量小）

| 文件 | 改动 |
|---|---|
| `kb/mod.rs` | 注册模块；`AGENTKB_VERSION = "0.5.0"` |
| `kb/db.rs` | 迁移 v5（`files.origin`）；`FileRow.origin`；更新游标的语句清 `origin` |
| `kb/sync.rs` | `reconcile` 的 `vanished` 条件加 `origin.is_none()` |
| `kb/search.rs`、`kb/status.rs` | `source_status` 输出 `elsewhere` 与 `origin`；`resume_for` 增加 `elsewhere` 分支 |
| `kb/cli.rs` | `--kb-export <file>`、`--kb-import <file> [--inspect] [--verify] [--no-sync] [--no-settings]`（无界面，进度打 stderr，结果 JSON 打 stdout） |
| `kb/commands.rs`、`lib.rs` | Tauri 命令：`kb_export_start`、`kb_export_cancel`、`kb_import_inspect`、`kb_import_apply`，进度走现有 `kb-progress` |
| `src/kb/types.ts`、`api.ts`、`KbSettings.tsx`、`KbWizard.tsx`、`KbHome.tsx`、`KbSearch.tsx`、`fixtures.ts` | 对应的界面与类型 |
| `src/i18n/locales/*/kb.json` | 新增键，5 个语言（`en`、`zh-CN` 认真写，其余先写英文） |
| `AGENTKB_VERSION` 同步 | `src/kb/branding.ts`、`src/kb/fixtures.ts`、相关测试 |

不改 skill 模板（`TEMPLATE_VERSION` 不变，免得用户又要点"更新"）。

### 9.4 测试计划（对应任务步骤 3）

- 容器：往返、截断、改一个字节（头/负载/尾各一次）、未知 `kind`、路径穿越（`..`、绝对路径、`\`、符号链接形状）、`format` 太新。
- 导出：一致性（导出过程中另一个线程做收集，收集完成且导出内容与快照一致）、整理在跑时拒绝、取消后无残留、硬链接失败的退路。
- 导入：空库、合并的每个格子（§5.1 的 A-H，§5.5，§5.6）、路径换算（含用户名不同、根目录被改过）、`elsewhere` 状态与"认回来"、中途被杀（在每个阶段注入失败）、schema 旧/新、设置（`digest.enabled`）。
- 前端：入口、进度、取消、确认框、出错提示。

## 10. 需要 leader 回复的清单

1. 同意用自己定义的容器格式（不用 tar）？（§1.2）
2. F 格（本机源文件还在、包里更长）用"两份都留"？（§5.2-5.3）
3. 同意"锁只拿快照的几秒 + 硬链接固定文件"（§2.1）？
4. 加密第一版不做、预留标志位？（§7）
5. "旧快照用迁移升级，新快照拒绝，不做凭空重建"替代任务里的"从 `raw/`、`notes/` 重建"？（§0、§3.4）
6. 合并时本机原本开着的整理开关保持不动？（§3.5）
