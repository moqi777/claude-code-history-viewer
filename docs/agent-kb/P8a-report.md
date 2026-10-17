# 第 8a 期报告：重复备份（版本 0.6.4）

打好的包：`src-tauri/target/release/bundle/macos/AgentKB.app`（本地 ad-hoc 签名，`codesign --verify --deep --strict` 通过；主程序 SHA-256 `203ed80e5f36541ec89bff45bf301749d5c6f08f826a4d7fcf118cd853eedd05`，61,377,040 字节）。**没有安装到 /Applications**。不升 schema，不碰压缩（P8b 没做）。

## 1. 做了什么

新文件 `kb/dedupe.rs`，三处接入：

| 做什么 | 在哪 |
|---|---|
| **收集后清理 `raw/`**：没有任何数据库行指向的 `X.jsonl`，若 `X.jsonl.zst` 有行指向且存在，并且 `X.jsonl` 的**每一个字节都是 `.zst` 解压内容的开头（或全部）**，就删 | `sync.rs: collect` 末尾（所有文件镜像完之后，新的 `.zst` 备份已经在了才会证明）。同一个函数既处理以后新产生的，也处理已有的 214 个，不需要单独的"一次性清理" |
| **移动硬盘上同样处理**：硬盘上的 `X.jsonl`，主目录里已经没有它、有 `X.jsonl.zst`，硬盘上的 `.zst` 与主目录的同大小（拷完整了），并且证明包含它，才删 | `backup.rs: copy_all` 末尾（本次备份没有任何错误时才做） |
| **提示一次**："清理了 N 个重复备份，省下 X GB"，点 ✕ 后不再出现 | 数据库 `meta` 里记累计和"没读过的"；`KbStatus.duplicates_notice`（新键）；前端 `KbHome` 加一条横幅（沿用"计划任务已改指向"那条的样式）；`kb_dismiss_notice("duplicates_cleaned")`；5 种语言的 `kb.notice.duplicatesCleaned` |

`sync` 的 JSON 多了 `duplicates_removed`、`duplicate_bytes_freed`，备份摘要里同名两个键（只增不改）。`--kb-search` 键名没动。

**证明的规则**：复用 `mirror::content_continues`（`reconcile` 本来就用它判断"同一个文件换了存法"）。不删的情形（日志里写原因，两份都留）：内容不同；`.zst` 损坏或被截断而读不完；读文件出错；空文件（空什么也证明不了）；`.zst` 那边没有行指向。**只处理 `X.jsonl` 配 `X.jsonl.zst`**，`.prev-*`、`.conflict-*` 这些"旧内容被挪开"的文件不碰。单次最多花 60 秒开始新的证明，剩下的下次再做。

**写入顺序**：只有"最后删文件"这一步会改磁盘；之前只读。所以任何时刻被杀掉，每一份内容都还在。

## 2. 测试

新增或改写（全部通过）：
- `dedupe.rs`：相等和更短的前缀判为多余；内容不同、比 `.zst` 长、空文件、损坏的 `.zst`、被截断的 `.zst` 都不算；只认 `X.jsonl`；累计与"没读过"的计数。
- `sync.rs`：
  - **复现"Codex 压缩源文件后再收集一次"**：以后 `raw/` 里只剩 `.zst` 一份，搜索只命中一次，数据库里记下一条通知，关掉后消失，下一轮不再删（`once_the_compressed_backup_exists_only_it_is_left…`）。原来的 `a_rollout_compressed_in_place…` 里"旧备份保留"的断言改成"已删除"。
  - **新旧内容不同时两份都留**：把留下的旧备份改坏一个字节 → 不删，多次收集也不删（`a_plain_backup_that_is_not_contained…`）。
  - **以前版本留下的孤儿**：一个是开头一段（删，字节数对），一个没有 `.zst` 孪生（不删），一个 `.prev-*`（不碰）（`plain_backups_older_versions_left_behind…`）。
  - **中途被杀**：数据库已指向新名字、`.zst` 还没写出来 → 旧备份不删；`.zst` 只写了一半 → 也不删（`a_pass_killed_before_the_compressed_backup_exists_loses_nothing`）。
  - 原来的 `sessions_stored_twice_by_an_older_version…`：被合并掉的那一行的旧备份现在也被清掉了（它是 `.zst` 的前缀），断言相应改了。
- `backup.rs`：硬盘上的旧备份在主目录压缩后被删、`.zst` 留下；硬盘上的副本内容不同、或主目录里又有这个 `.jsonl` 时都不删。
- 前端 `KbHome.test.tsx`：横幅显示数量和大小，点 ✕ 调用 `dismissNotice("duplicates_cleaned")` 并消失。

7 项检查：`pnpm tsc --build .` 通过；`pnpm vitest run` 146 个文件 / 1575 通过；`pnpm lint` 通过；`pnpm run i18n:validate` 通过；`cargo test -- --test-threads=1` 1484 通过、6 忽略；`cargo clippy --all-targets --all-features -- -D warnings` 通过（`twin_of` 里一处大小写比较加了说明的 `allow`，名字是 Codex 写的精确 `.jsonl`）；`cargo fmt --all -- --check` 通过。

## 3. 在真实库副本上跑一次

在 `/private/tmp/p8work/t8`（仓库外）做：`sqlite3 .backup` 复制 `kb.db`，`cp -c` 克隆 `raw/`，来源目录为空、语义关；真实 `~/AgentKB` 只读。先用 0.6.3 的二进制收集一次作为"之前"，再用新二进制收集。

| | 之前 | 之后 |
|---|---|---|
| `raw/` 文件数 | 529 | **315** |
| `raw/` 大小 | 3,832,304 KB（3.66 GiB） | **1,739,392 KB（1.66 GiB）** |
| 删掉 | | **214 个，2,142,669,252 字节**（与 leader 核对的数字完全一致） |
| 收集用时 | | 3.7 秒 |

- **16 道旧题**（整句和关键词两种问法，共 32 次搜索，每次取前 10，比较会话 id、分数、命中片段、命中会话数）：之前和之后 **0 条不同**。
- **`--kb-show` 抽查**：33 个会话 id 各取 `claude:` 和 `codex:` 两种来源的 `--around 3` 输出（实际有结果的约 30 个，含 Codex 已压缩的会话和 Claude 会话），去掉 `data_as_of` 时间戳后**逐字相同**。
- **P5 导出导入**：清理后的副本 `--kb-export`（1,315,048,980 字节，315 个会话、315 个 raw 文件）→ `--kb-import` 进一个空 HOME，`raw/` 315 个文件，同样 32 次搜索与"之前"**0 条不同**。
- 再收集一次：`duplicates_removed` 为 0（幂等）。
- 数据库里记下了 `dedupe_total_files=214`、`dedupe_total_bytes=2142669252`，"没读过"的也是这两个数，横幅会显示 "214 个 / 1.99 GiB"。

没有在真实 `~/AgentKB` 上跑；用户装上 0.6.4 后第一次收集就会清理。

## 4. 和 Windows 版的关系

`origin` 上现在只有 `agent-kb` 和 `main` 两个分支（`git ls-remote`），**`agent-kb-win` 还没有推上来**，所以没法按审核要求对比它有没有改同一个函数。本期动到的位置供 leader 对照：`sync.rs`（`Summary` 结构、`collect` 末尾一段、测试）、`backup.rs`（`BackupSummary`、`copy_all` 末尾一段、测试）、`status.rs`（`KbStatus` 一个字段）、`commands.rs`（`kb_dismiss_notice` 一个分支）、`mod.rs`（一行），其余是新文件和前端。新代码里删除文件用 `std::fs::remove_file`，目录同步用 `fsutil::fsync_dir`；Windows 上"文件被占用时删不掉"只会让这一项在日志里写一行并留到下次，不影响别的。

## 5. 需要知道的

- 删除的是**没有行指向、且证明被 `.zst` 完整包含**的文件。`join_twin_rows` 合并掉的那一行（旧版本留下的双份）的未压缩备份，现在也会被清掉（只要证明成立）；以前"两份备份都留"的约定为此改了。
- 清理发生在收集的最后；如果某次收集中途出错退出，下一次再做，不会漏。
- 横幅在用户点 ✕ 之前一直显示，不会在重新打开后消失。
- 硬盘的清理需要硬盘接着且本次备份无错；没接着时硬盘上的旧文件留着，下次接上后清理。
- 没做：压缩（P8b）；已压缩备份的重压缩；`.prev-*`/`.conflict-*` 的任何处理。
