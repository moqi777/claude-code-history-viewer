# P5 报告：一键导出 / 导入（AgentKB 0.5.0）

提交在 `agent-kb` 分支，已推送到 `origin`；没有动 `upstream`。
**没有安装到 `/Applications`**（按要求）：`/Applications/AgentKB.app` 仍是 0.4.1。0.5.0 的包已打好：`src-tauri/target/release/bundle/macos/AgentKB.app`，`codesign --verify --deep --strict` 通过，主程序 SHA-256 `c9b49f03…c58e`。
没有任何真实 AI 调用；没有写过真实的 `~/AgentKB`、`config.json`、`~/.claude`、`~/.codex`、skill。所有导出、导入都在临时 HOME 里，对的是真实库的**副本**（APFS 克隆）。

## 1. 做了什么

| 部分 | 内容 |
|---|---|
| 步骤 0 | 技能是否过期只看 `TEMPLATE_VERSION` 和程序路径（`c898616`，设计审核已看过） |
| 容器格式 | `kb/pack.rs`：`.agentkb` = 16 字节头 + 一个 zstd 流（条目流）+ 尾部 manifest，按设计 §1。**无新依赖** |
| 导出 | `kb/export.rs`：锁只在快照时拿（`VACUUM INTO` + `raw/` 硬链接 + 复制 `notes/`），之后流式写；建不了硬链接就退回整个导出期间拿锁；整理在跑直接拒绝；写到 `.partial`、权限 0600、取消会清理 |
| 导入 | `kb/import.rs` + `kb/merge.rs`：`prepare`（解到数据目录里的 `.import-<id>/`，逐个文件核对大小和 SHA-256，整理库副本）→ `commit`（拿锁，空库用改名切换、数据库最后换；有数据的库逐文件合并） |
| 来自另一台设备 | schema v5：`files.origin`。收集不会把它们标成已删除；源文件在本机出现后自动认回来；对外 `source_status = elsewhere` + `origin`；`resume.available=false`；首页/搜索有徽标，筛选里有"只看来自另一台设备的会话" |
| 命令行 | `--kb-export <文件> [--overwrite]`；`--kb-import <文件> [--inspect｜--plan] [--no-sync] [--no-settings] [--adopt-settings]` |
| 界面 | 设置页"搬到另一台电脑"（导出、导入，含进度、取消、预览、出错提示）；向导第一页"从文件导入"（导入后设置预填、跳到最后一页，不写 `config.json`） |
| 版本 | `AGENTKB_VERSION = 0.5.0`（Rust、`branding.ts`、`fixtures.ts`、测试） |

审核意见第 2 节的 6 处：

| # | 做法 | 验证 |
|---|---|---|
| 1 | 头部 `flags` 和保留 4 字节必须为 0；条目顺序、个数、大小必须和 manifest 完全一致；负载按长度截断后才交给 zstd；另加 `payload_sha256`（zstd 自带的校验和覆盖不到帧头的几个字节，单字节测试第一次就抓到了） | `pack.rs` 的 `changing_any_single_byte_is_refused`：对一个小文件的**每一个字节**各改一位，全部拒绝；头部、保留字段、截断、追加、新版本各有测试 |
| 2 | 合并只认 `mirror_offset` 之前的内容（`merge.rs::classify`），追加前 `fsutil::append_tail` 先把游标之后的孤儿字节截掉 | `a_tail_that_was_half_appended_when_the_process_died_is_cut_and_written_again`：本机文件末尾有写了一半的新内容加垃圾 → 再导入，结果逐字节等于一次成功的导入 |
| 3 | 没有 `files` 行的文件（`.prev-*`、Codex 明文兄弟文件、`.imported-*`）：本机没有→复制；一样→跳过；不一样→加 `.imported-<标签>` 后缀两份都留 | `merge.rs::every_case_of_one_session_file`；真实库导入里 214 个明文兄弟文件走的就是"复制" |
| 4 | 空库导入在两次改名之间被杀 | `a_crash_between_the_renames_of_an_empty_import_is_repaired_by_importing_again`：`after_raw`、`after_notes` 两个点 × "先跑一次收集 / 先打开库"，再导入后会话数、笔记数、搜索结果、文件的 SHA 都和一次成功的一样 |
| 5 | 导入结果带 `checks`（会话、笔记、有笔记的会话、`raw/` 文件数、笔记文件数），空库对 manifest 的 `counts`，合并对"导入前 + 预览里的新增数"；对不上只报告（界面和命令行 JSON 里都有），不算失败 | 见 §3；界面有测试 |
| 6 | 真实库的导出文件都在 `/private/tmp/agentkb-p5-final/`（仓库外），证据脚本跑完就 `rm -rf`；见 §5 | `ls /private/tmp` 里没有残留 |

## 2. 与设计的偏差、实现中发现并修掉的问题

1. **合并路径靠"索引声明过期"自愈**：合并时数据库行和索引不在同一个事务里，崩溃后可能有行没有索引。所以插入行的同一个事务里把 `meta.indexer_version` 设成 `importing`（任何版本都不等于当前），索引完才恢复；崩溃后下一次收集会全量重建。测试 `a_crash_in_the_middle_of_a_merge_gives_the_same_result_as_one_run`（`after_files`、`after_db` 两个点）。
2. **Codex 标题被误当成"线程已删除"**（真实数据上发现）：导入后第一次收集，`refresh_codex_titles` 在本机的 Codex 里找不到这些线程，就把带原生标题的会话改回"第一条消息"的标题（`--kb-show` 对比 8/16 不同）。修：`db::load_codex_titles` 跳过 `origin` 非空的行。测试 `a_codex_title_given_by_the_other_computer_survives_the_collection_here`（去掉修复会失败，已验证）。
3. **笔记行顺序**：一开始导入时用 `notes::rebuild_all` 重建笔记表，等分的笔记排序会变（`--kb-search` 16 题里 2 题的 `notes` 不同）。改成 `notes::reconcile`：和笔记文件一致的行原样保留。
4. **`exclude_cwd_prefixes` 要同时留"原样"和"换算后"两条**：会话文件内容里的 `cwd` 是原设备的路径，重建索引时排除规则按内容匹配，只留换算后的会让导入的会话在下一次重建索引时重新出现。`sessions.cwd` 也不再换算（以文件内容为准）。空库导入后把 `exclude_cwd_applied` 设成本机的列表，首次收集不会因为写法不同而全量重建（真实库上首次收集 5 毫秒）。
5. **`VACUUM INTO` 之后清掉整理的运行状态**（`digest_rounds`、`digest_items`、`digest_first_round_seen`）：换机器没有意义。`digest_sessions` 由笔记文件重建，所以已有笔记的会话不会被重新整理。
6. **导入时 `db::open` 会给旧版本快照拍一份 `kb.db.pre-v<N>` 安全副本**：在工作目录里，导入结束随工作目录删除，不进数据目录。
7. **本机升级到 0.5.0 时**：第一次打开库会做 v4→v5 迁移，并按已有约定在 `~/AgentKB` 旁边留一份 `kb.db.pre-v5-<时间>`（约 110–180 MB，和 v4 时一样；不自动删）。
8. **合并导入遇到"本机源文件还在、包里更长"（F 格）按审核决定两份都留**；真实库上 F、G 各 **0**（见 §3 第 7 项），逻辑由单元测试覆盖（`merging_two_libraries_keeps_everything_either_side_had`：F、G、E、笔记冲突、重复导入不变各一例）。
9. **发现、不属于本期、未改**：同一批笔记文件，"一条一条增量建出来的笔记索引"和"从笔记文件整体重建的笔记索引"，同一个查询的笔记分数不同（"电池 耗电"：16.55 对 14.51；对 A 的副本自己重建一次就能复现，不涉及导入）。结果顺序没变，只是分数和偶尔进榜的第 2、3 条笔记不同。建议第 6 期（语义搜索）一起看。

## 3. 验收第 5 节逐项（执行 AI 自己验证）

全部命令和输出在 `docs/agent-kb/p5-evidence/run_all.log`，由 `run_all.sh` 一次跑出（`compare.py` 是对比脚本）。环境：Apple Silicon，热缓存；A = 真实库副本（314 个会话、116 条笔记、528 个 `raw/` 文件、19 个笔记文件，4.0 GB）。

| # | 项目 | 结果 |
|---|---|---|
| 1 | 一致性 | 导出到 B（另一个用户名的 HOME）：会话 314、笔记 116、有笔记的会话 18、`raw/` 528、笔记文件 19，**全部和 manifest 一致**（`all_match: true`）。P3-eval 15 题 + "电池 耗电"：**两边 `--kb-search` 的会话 id、顺序、首条命中、笔记（含分数）完全一致 16/16；对每题第 1 名会话 `--kb-show --head 30` 的消息一致 16/16**。另用"文件声称来自 `/Users/<另一个用户>`"的改写版导入到又一个用户（F）：会话 314 个全部 `elsewhere`、0 个 `deleted`、按工作目录排除的 30 个仍排除，16/16 与 16/16 |
| 2 | 完整性 | `raw/`、`notes/` 共 547 个文件，A 和 B 逐个 SHA-256 一致（`identical in B`） |
| 3 | 防损坏 | 对导出文件副本的第 1,000,000,123 字节翻一位 → `corrupt: the checksum of raw/codex/… is wrong`，退出码 1；B（已有数据）所有文件的哈希、目录列表不变，没有残留的 `.import-*`。另有 `one_changed_byte_is_refused_and_nothing_changes`（头、负载、尾共 6 处）；`pack.rs` 对每个字节各改一位全部拒绝 |
| 4 | 防中断 | `kill -9` 正在解包（7 秒时）的导入进程：数据目录里只有 `.import-*` 工作目录，没有 `raw/`、`notes/`、`kb.db`，也没有写 `config.json`；再导入一次成功，`all_match`，与 A 的 547 个文件逐个一致，搜索 16/16。更细的中断点（两次改名之间、合并的文件之后/数据库之后、追加到一半）见 §1 的 2、4 和 §2 的 1 |
| 5 | 合并 | 见下面的"合并"小表；任何一边原有的文件都没被删（D：导入前 314 个文件，导入后 528 个，缺失 0、改变 0） |
| 6 | 导出时不出错 | 对 D（读真实 `~/.claude`/`~/.codex` 的库）导出的同时每秒跑一次 `--kb-sync`：**17 次收集全部 `status: ok`**，没有一次 `skipped: locked`（不是整个导出期间拿锁的证明）；另有单元测试在"快照之后、写文件之前"插入收集并新增一个会话，收集正常完成，导出文件仍是快照时刻的 3 个会话。整理在跑（用持有 `digest.lock` 的进程模拟）：`digest_running: a digest is running (since …)`，没有写文件 |
| 7 | 设置 | 导入后 B 的 `config.json`：`digest.enabled=false`、`cli_path` 空、`mirror` 关、`data_dir` 是 B 自己的；排除规则带过来了（原样 + 换算后两条）并生效（F：30 个会话被排除）。已有 `config.json` 的机器：只合并排除规则，间隔/整理开关保持不变（测试 `settings_travel_but_the_machine_specific_ones_do_not`），导入前把原文件另存为 `config.json.before-import-<时间>` |
| 8 | 不碰源目录 | `~/.claude`、`~/.codex` 里 10811 个闲置超过 15 分钟的文件，对同一批路径 stat 两次（大小+修改时间）哈希：做了 `--kb-sync`、`--kb-export`、`--kb-import`（含其收集）之后**不变**（`f4fcfd78b9d74073` 两次相同）。仅限闲置文件：我自己的会话文件还在被写，不可比 |
| 9 | 速度、门禁 | `--kb-search` 0.08–0.13 秒（≤300 ms）；门禁见 §4 |

**合并**（真实库的导入文件 → 各种本机）：

| 目标 | 预览 / 结果 | 搜索对比 |
|---|---|---|
| E：A 的会话、没有笔记 | 528 个文件相同、笔记新增 19 个；`all_match`；再导入一次：新增 0、笔记相同 19 | 16/16（笔记分数不比，原因见 §2 第 9 项）|
| D：自己收集了真实 `~/.claude`、`~/.codex` 的库（314 个会话，没有笔记） | 314 个相同、214 个明文兄弟文件被复制、笔记 +19；**F（本机源文件还在、包里更长）0 个，G（内容分叉）0 个**；`all_match`；D 原有的 314 个文件缺失 0、改变 0 | — |
| 单元测试里的构造库 | 新增 1、接着补全（E）1、本机源文件还在（F）1、分叉（G）1、笔记冲突 1（留较新的，较旧的在 `notes/.conflict-<标签>/`）；原有文件除"接长"的那一个外全部不变；再导入：新增 0 | — |

## 4. 门禁

| 命令 | 结果 |
|---|---|
| `pnpm tsc --build .` | 通过 |
| `pnpm vitest run` | 145 个文件，**1556** 个测试通过（新增 `KbTransfer.test.tsx` 等） |
| `pnpm lint` | 通过 |
| `pnpm run i18n:validate` | 通过（新增 60 个键 × 5 个语言：`kb.transfer.*`、`kb.wizard.importFromFile`、`kb.sessions.elsewhere*`、`kb.filters.onlyElsewhere`；`en`、`zh-CN` 认真写，其余英文） |
| `cargo test -- --test-threads=1` | 1415 通过，0 失败，2 忽略（原有的 `kilo` 真机测试 + 新增的手动证据辅助测试 `repack_as_another_machine`） |
| `cargo clippy --all-targets --all-features -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` | 通过 |

## 5. 真实数据量与耗时（命令行，release 版）

| 项 | 实测 |
|---|---|
| 导出文件大小 | 2.14 GB（未压缩 4.0 GB，54%）；`kb.db` 由 215 MB 的库经 `VACUUM INTO` 变成约 112 MB |
| 导出耗时 | **18–21 秒**（比设计里 `tar|zstd` 的 5.5 秒慢：单线程 zstd 加 `sha2` 的软件实现 SHA-256；`sha2` 在 aarch64 默认没有硬件加速，需要时可开 `asm` 特性） |
| 导入耗时（空库） | **16–19 秒**，其中解包+逐文件校验约 15 秒；切换（三次改名）几十毫秒。直接用包里的 `kb.db`，不重建索引；之后首次收集 5 毫秒 |
| 导入耗时（合并，D） | 约 20 秒预览 + 约 35 秒（合并+随后的收集，含 3.3 GB 的收集读取） |
| 数据目录所在盘需要的空间 | 未压缩总量的 110%（真实库 4.4 GB）；导出文件若放在同一个盘上还要再加它本身 |

测试用的导出文件（约 2 GB × 若干份）和 A–F 的库副本都放在仓库外的 `/private/tmp/agentkb-p5-final/`，脚本末尾 `rm -rf` 掉了，`/private/tmp` 里没有残留；仓库里没有任何导出文件、会话原文（`p5-evidence/` 里只有脚本和日志，日志里是查询词、8 位会话 id、数量）。

## 6. 文件清单

- 新增 Rust：`kb/pack.rs`、`kb/export.rs`、`kb/import.rs`、`kb/merge.rs`、`kb/transfer.rs`、`kb/transfer_commands.rs`、`kb/import_tests.rs`
- 修改 Rust：`kb/db.rs`（v5、`origin`、导入用的行读写、`origin_column`/`status_expr`）、`kb/sync.rs`（`reconcile` 跳过 `origin`、`reindex_one` 可见）、`kb/search.rs`、`kb/status.rs`（`elsewhere`、`origin`、`only_elsewhere`）、`kb/cli.rs`、`kb/mirror.rs`、`kb/mod.rs`、`lib.rs`（6 个新命令）
- 前端：`KbTransfer.tsx`、`transferErrors.ts`（新）；`KbSettings.tsx`、`KbWizard.tsx`、`KbHome.tsx`、`KbSearch.tsx`、`api.ts`、`types.ts`、`useKbStore.ts`、`progressLabel.ts`、`testkit.ts`、`fixtures.ts`、`branding.ts`；5 个语言的 `kb.json`
- 文档与证据：`P5-design.md`、`P5-report.md`、`p5-evidence/`（`run_all.sh`、`run_all.log`、`compare.py`）、`p5-shots/`（10 张截图，模拟数据）
- 新依赖：无

## 7. 没做 / 已知局限

- **加密**：按审核意见没做，格式里预留了 `flags` 位和 `encryption.scheme`；导出界面有"请存到 FileVault 或加密的磁盘映像"的提示。
- **同一个会话在新机器上的路径与导入时不同**（比如被放进 `archived_sessions/`）：收集会把它当新文件，同一个 `session_id` 有两行。和 P4 记下的子代理重复同类，留到第 6 期。
- **Windows**：路径统一 `/` 相对、不依赖权限位，格式可原样用；跨操作系统时不换算路径。未在 Windows 上验证（第 7 期）。
- 对在别的机器上本来就是明文/`.zst` 不同存储形式的同一个会话，只在两边内容逐字节相同时视为相同，否则两份都留（见设计 §5.1 H 的简化）。

## 8. 用户真机确认（装上 0.5.0 之后）

10. 在设置页点"导出到文件…"，看到文件大小、用时、保存位置；用 `AgentKB --kb-import 文件 --inspect` 能读出会话数、笔记数（不会改动任何东西）。**注意**：导出时若"每日整理"正在跑，会提示等它完成。
11. 有第二台 Mac 时：装好 AgentKB，在向导第一页点"从文件导入…"（或设置页），导入后搜一个记得的问题，结果应和原来那台一样；首页对应的会话会带"来自另一台设备"的标记。没有第二台就跳过，由第 1 项覆盖。
12. 本机第一次打开 0.5.0 会把库升级到 v5，并在 `~/AgentKB` 里留一份 `kb.db.pre-v5-<时间>`（约 180 MB，不会自动删）。
