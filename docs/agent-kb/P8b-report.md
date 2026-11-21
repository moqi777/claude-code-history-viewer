# P8b 报告：旧备份压缩存放（0.8.0，步骤 2）

基线 `7dc5702`（设计审核通过）。提交：

| 提交 | 内容 |
|---|---|
| `c207709` | **恢复时改指向孪生文件**（审核第 2 节第 1 条，单独提交、单独测试，Codex `.zst` 情形；0.7.6 就有的问题） |
| `d61403a` | 压缩本体：schema v6、`kb/shrink.rs`、收集里解包/压缩、dedupe 4 种孪生、导出/合并/恢复/硬盘备份的配合、App 读压缩会话、设置页、版本 0.8.0 |
| `4044e6d` | App 打开会话时续接链检查不再重复解压（实测后加的小优化）+ 计时用的忽略测试 |

包：`src-tauri/target/release/bundle/macos/AgentKB.app`（ad-hoc 签名，`codesign --verify --deep --strict` 通过；主程序 `Contents/MacOS/AgentKB` SHA-256 `29b8c738cf5bcbadfd3c21da1befd0238f523981477994090c474609d9366949`，61,855,248 字节；在 `4044e6d` 上打的）。**没有安装到 /Applications**，真实 `~/AgentKB` 只读（只用 `sqlite3 -readonly .backup` 和 `cp -c` 复制）。

## 1. 审核意见第 4 节的三项

### 1.1 App 里打开 89.7 MB 压缩会话的额外等待和峰值内存

做法：App 打开会话走的是 `load_provider_messages_paginated` → `load_session_messages_paginated`（先取 100 条，往上翻再取 100 条）和 `get_session_message_count`。我加了一个忽略测试 `stored::tests::open_a_real_session`，在优化编译的测试程序里按这个顺序调用同一组函数，用 `/usr/bin/time -l` 量整个进程的峰值内存。**没有在界面上点开计时**（这台机器上没有可用的界面自动化），界面渲染两种情况相同，差别全在这几个后端调用里。

文件：真实库副本里最大的 Claude 会话 `15bf7c02…`（89,715,796 字节，压成 39,066,519），同目录 4 个兄弟文件一起（它有 14 处续接标记，打开时会扫兄弟文件）。"未压缩"对照是把同一目录整个解压到另一处。

| | 第一页 100 条 | 第二页 | 数总数 | 峰值内存 |
|---|---|---|---|---|
| 未压缩（稳定后） | 102 ms | 49 ms | 13 ms | 136 MB |
| 压缩 | **150 ms** | 45 ms | 9 ms | 142–146 MB |
| 差 | **+约 48 ms** | 不慢 | 不慢 | **+约 6–10 MB** |
| 合成 538 MB（压成 235 MB），未压缩 / 压缩 | 596 / **880 ms（+约 285 ms）** | 292 / 264 ms | 77 / 50 ms | 602 / 604 MB |

- 内存几乎不涨：未压缩时 mmap 读到的页也算在常驻内存里；压缩时是解压到内存的一份，同一时间只留最近打开的一个文件。
- `4044e6d` 之前，压缩会话的第一页是 180 ms / 1,070 ms（续接链检查把文件又解压了一遍），改后如上表。
- 目标"额外等待 ≤ 1 秒"：89.7 MB 是 0.05 秒，538 MB 是 0.29 秒。

### 1.2 真实库副本上压缩前后的搜索和 `--kb-show` 对比

副本：`/private/tmp/p8bwork`（仓库外，已删），来源目录为空，语义搜索开（向量和模型一起克隆）。脚本 `p8b-evidence/eval.py`：16 道旧题 + `P6-eval.md` 15 题，每题整句和关键词两种问法共 **62 次搜索**（`--limit 10`），再加 6 个会话的 **11 次 `--kb-show --full`**（其中 5 次定位到会话末尾附近的长消息，必须从备份文件读原文，是压缩后最慢的读法）。比较时只去掉 `data_as_of`。

| 阶段 | 压了 | 压缩前 → 后 | 用时 | 峰值内存 | `raw/` 合计 | 73 项对比 |
|---|---|---|---|---|---|---|
| 压缩前 | | | | | 3,376,133,266 字节，449 个文件 | — |
| N = 30（默认） | 55 个（全是 Claude） | 140,544,929 → 57,083,420 | 2.1 秒 | 72.5 MB | 3,292,671,757 | **0 条不同** |
| 再按 N = 7（含 89.7 MB 和 67 MB 两个最大的） | 又 35 个 | 559,687,412 → 217,153,668 | 5.1 秒 | 47.1 MB | 2,950,138,013 | **0 条不同** |
| 导出（2.76 GB，15.7 秒）→ 导入空库 | 90 行仍是压缩的，`all_match` | | | | | **0 条不同** |

- 文件数不变（压缩是一换一）。默认 30 天在这份库上只省 83 MB，因为 Codex 自己会压、Claude 的大会话大多还不到 30 天；按 7 天能省 426 MB。
- `--verbose` 输出里 `paths.mirror` 变成 `….jsonl.packed.zst`（键名不变），默认输出不带这个键。
- 6 个抽查会话都在被压的 90 个里，含最大的 89.7 MB。

### 1.3 恢复修复前后：同一份"快照指向已删文件"的硬盘备份

造法：用副本做一个硬盘备份文件夹，`kb.db` 是压缩**之前**的快照（0.8.0 升级时自动留的 `kb.db.pre-v6-*`，v5），`raw/` 是压缩**之后**的（90 个只剩 `.packed.zst`）。这正是"白天压了、当天快照没重拍、硬盘上旧文件被清理"的样子。用 `restore_a_real_backup` 忽略测试恢复：

| | missing | 改指向 | 恢复后第一次收集 |
|---|---|---|---|
| 修复前（`7dc5702`，临时 worktree，已删） | **90** | — | 90 个错误（`cannot open mirror`） |
| 修复后（当前） | **0** | 90 | 0 个错误，搜索正常 |

单元测试另覆盖了 Codex `.zst` 情形（`c207709`）、内容对不上不改指向、压缩后快照换电脑恢复时源路径换算成 `.jsonl`。

## 2. 按审核第 2、3 节做的

| 项 | 做法 |
|---|---|
| 2-1 恢复改指向 | `restore.rs: repoint_to_replacement`：行指向的文件不在时，按 `dedupe::replacements_of` 找另外两种存法，证明"开头 `head_len` 字节哈希相同、总长 ≥ `mirror_offset`"才改；改字段用 `db::convert_file`（Codex `.zst`）或 `db::set_packed`。报告里多一个 `repointed_files` |
| 2-2 内存解压 | 1 GB 上限保留（`shrink::MAX_BYTES`） |
| 2-3 续接链 | 做了，前一半、后一半被压都能接上（测试 `a_compressed_backup_can_be_either_half_of_a_chain`），没有退回 |
| 2-4 不建待删清单 | 删不掉的留着，dedupe 每次收集证明后再删 |
| 3-1 读的时候文件被换掉 | 每日整理和语义索引不读 `raw/`（grep 过）。`--kb-show`/搜索读原文（`search.rs: full_text`）打不开时重新读这一行的 `mirror_rel` 再试一次；App 打开（`stored.rs`、`codex.rs`）打不开时换另一个名字再试一次。测试：读之前另一边把它压缩了、或解包了，结果都一样 |
| 3-2 改名目标已存在 | 不依赖 rename 覆盖：`shrink::place` 先处理目标位置的残留——证明它被新文件完整包含就删，否则挪成 `.prev-*`；删不掉（Windows 被占用）就这个文件这次失败、行仍指向旧的完整文件、收集照常结束，下次再试。Mac 上用 `fsutil::REFUSE_REMOVE` 注入模拟 |

## 3. 改了什么

- 新增：`kb/shrink.rs`（选候选、压缩、解包、一轮、统计、"现在压缩"）、`kb/shrink_tests.rs`、`commands/session/stored.rs`（App 读会话文件：明文 mmap / 压缩的解压并缓存最近一个 / 换名重试）、`src/kb/KbShrinkField.tsx`
- `db.rs`：v6 `files.packed`；`FileRow::mirror_is_zst`；`set_packed`；`convert_file` 同时清 `packed`；读老库（还没升 v6 的只读连接）时 `packed` 当 0
- `sync.rs`：`packed` 且源变了 → 先解包再 `decide`；文件处理完、dedupe 之前跑一轮压缩（20 秒，单独线程，macOS 后台 QoS / Windows 线程后台模式）；关闭时只清临时文件；4 处改 `mirror_is_zst`
- `dedupe.rs`：4 种孪生（raw 和硬盘两处）；`fsutil.rs`：`write_tmp`、`install_tmp`、`remove_synced`、`is_tmp_name`
- `export.rs` 压缩文件不截断；`merge.rs` 导入副本名 `X.imported-<tag>.jsonl.packed.zst`；`import.rs` 合并插入压缩行时 `mirror_offset` 保持明文长度；`backup.rs` 硬盘上的 `.zst` 只整份替换不追加；`restore.rs` 换算源路径去掉 `.packed.zst`
- App：`load.rs` 4 处 mmap 改走 `stored::session_bytes`；`chain.rs` 认 `.jsonl.packed.zst`；`codex.rs` 认 `.jsonl.packed.zst` 并换名重试
- 设置页"旧备份压缩"：开关、天数（1–3650）、已压缩统计、上次压缩、"现在压缩一遍"；`config.json` 新节 `pack {enabled, after_days}`（默认开、30）；命令 `kb_shrink_stats`/`kb_shrink_now`；CLI `--kb-shrink --all|--status`；5 种语言（ko/ja/zh-TW 先填英文）
- 版本 0.8.0：`kb/mod.rs`、`src/kb/branding.ts`、`src/kb/fixtures.ts`

## 4. 测试

任务书第 5 节逐条：

| 要求 | 测试 |
|---|---|
| 压缩每一步被杀，下次收集后一个字节不少 | `every_step_of_compressing_can_be_cut_short`（3 处钩子） |
| resume：变长 → 解包 → 追加，新旧都能看到 | `new_messages_turn_the_backup_back_into_plain_text_then_append`、`every_step_of_unpacking_can_be_cut_short` |
| 压缩过程中源文件变长 | `the_source_growing_while_it_is_compressed_loses_nothing` |
| 压过后 Codex 又压了源文件 | `codex_compressing_the_source_after_us_leaves_one_backup` |
| 导出 / 导入 / 换目录 / 硬盘备份 / 从硬盘恢复 | `compressed_backups_travel_through_export_and_import`（空库和合并）、`moving_the_data_folder_keeps_compressed_backups_readable`、`the_disk_backup_follows_and_a_restore_finds_the_compressed_backup` |
| Windows 删不掉，收集照常，下次补删 | `a_file_that_cannot_be_removed_stays_and_goes_next_time`、`a_leftover_at_the_unpack_target_never_blocks_for_good` |
| 关闭后不再压，已压的照常可读 | `switched_off_nothing_new_is_compressed_and_old_ones_stay_readable` |

另有：压缩前后搜索 / `--kb-show` / `messages`（含 `byte_off`）完全一致且 `--kb-reindex` 后也一致；候选按"最后消息和文件时间都早于 N 天"；残留临时文件清理；读到一半被压缩或解包；App 用新旧两个名字都能打开；续接链；Codex 读压缩备份；恢复改指向（含内容对不上不改）；设置页（统计、天数校验、保存、现在压缩的三种结果）。

7 项检查（在 `4044e6d` 上）：`pnpm tsc --build .` 通过；`pnpm vitest run` 149 个文件 / 1596 通过；`pnpm lint` 通过；`pnpm run i18n:validate` 通过；`cargo test -- --test-threads=1` 1558 + 18 通过、15 忽略（忽略的是需要真实文件的计时/演练测试）；`cargo clippy --all-targets --all-features -- -D warnings` 通过；`cargo fmt --all -- --check` 通过。

## 5. 和要求不同、没做或要知道的

1. **"现在压缩一遍"在 App 进程里跑**（和"立即收集"一样），不能取消。真实副本上全部到期的一次压完用了 2.1 秒和 5.1 秒，所以没做成后台子进程
2. **0.7.6 碰到 v6 库**：收集干净拒绝（"schema v6 is newer than this program supports"，什么都不改）；但它的只读搜索仍然能用（只读连接本来就不查版本），读压缩会话原文也正常（按 `.zst` 扩展名解压）
3. **导出文件不带 `pack` 设置**（导出的"与机器无关的设置"沿用 P5 的清单，没改格式）；导入后用默认（开、30 天）
4. **Windows**：`platform/windows.rs` 新加的 `lower_thread_priority`（`SetThreadPriority`）这台 Mac 没装 Windows 目标，没有编译过，留给步骤 3
5. 合并时"同一会话本机压缩、对方未压缩"按约定两份都留；之后每次收集 dedupe 会在日志里写一行"kept"
6. 会话列表里已显示的 `file_path` 在压缩/解包后要刷新才变；在那之前点开靠换名重试，能打开
7. 没测到的情形：恢复把行改指向 Codex `.zst` 后，这台电脑上又恰好有那个源文件，下一次收集怎么处理（推测会按"源换了存法"整份重新复制，可能多出一份 `.prev-*`，没有验证）
8. 这台机器的 shell 设了 `HTTP(S)_PROXY`，会让 `semantic::model` 的 3 个本地下载测试拿到 502；7 项检查里的 `cargo test` 是去掉代理变量跑的，和代码无关
9. 构建缓存：做完删了 `src-tauri/target/debug`（13 GB），留下 `target/release`（1.7 GB，包在里面）；真实库副本工作目录 `/private/tmp/p8bwork`（14 GB，克隆）已删
