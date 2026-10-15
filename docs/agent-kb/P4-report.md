# P4 报告：每日整理成笔记（AgentKB 0.4.0）

分支 `agent-kb`，已推送到 `origin`（`moqi777/claude-code-history-viewer`）。没有向 `upstream` 推送，也没有在原项目提任何东西。
`/Applications/AgentKB.app` 已换成 0.4.0（与构建产物逐字节相同，SHA-256 `4c395693…816c`，`codesign --verify --deep --strict` 通过，ad-hoc 签名）。旧的 0.3.1 副本留在 `scratch/AgentKB-0.3.1.app`，需要时可以换回。
**`digest.enabled` 保持 `false`**，没有在真实库上整理过，也没有动 `config.json`（安装前后 SHA-256 相同，`172c4eb4…dee9`）。

## 1. 提交

| 提交 | 内容 |
|---|---|
| `22f185c` | P4 设计审核 + 按审核修改的设计 |
| `f1a0d62` | 整理引擎：笔记文件、schema v4、轮次、补跑、重试、假 AI 测试 |
| `ad55387` | 笔记进备份/迁移，`--kb-reindex` 重建笔记，整理工作目录不进索引，`seq` 约定测试 |
| `a6990dc` | `--kb-digest`、收集后拉起整理、`--kb-search` 带笔记、skill 模板 v3、App 命令 |
| `0aeb489` | 界面：首页整理卡片、笔记页、搜索里的笔记、设置和向导里的整理页、会话里的笔记面板 |
| `2d74101` | 每段 5 万字符、Codex 默认低推理档位、日志里记花费和轮数 |
| `398e552` | **`.jsonl` → `.jsonl.zst` 当作同一个会话**；把旧版本留下的 214 对合并；升级前复制 `kb.db`；步骤 4 审核意见 |
| `bbec1ac` | 每段记录覆盖率（日志和 `digest_items`） |
| `82dd490` | 跳转等消息加载完再滚动（高亮随之恢复）、⌘K 进知识库搜索、状态栏显示已备份数、去掉"换小模型省额度" |
| 版本号提交 | 界面上的版本号改成 0.4.0 |
| 本报告所在提交 | `P4-report.md`、`p4-shots/`、`PLAN.md`（leader 的改动，原样带上） |

`docs/agent-kb/P4-samples.md` **没有提交**（含个人行程、订阅花费等内容，仓库是公开的），已写进 `.git/info/exclude`。

## 2. 步骤 4 审核意见第 2 节：`.zst` 问题

**现象（真实库，只读核对）**：Codex 从今晚 22:10 起把旧的 `rollout-….jsonl` 原地压成 `rollout-….jsonl.zst`。0.3.1 的 `move_key` 把"是否压缩"和完整文件名都算进身份，认不出这是同一个文件：旧文件被标成"源已删除"，`.zst` 被当成新会话重新收集。结果 `files` 里 214 条 `deleted` 的 `.jsonl` + 214 条 `present` 的 `.jsonl.zst`，同一个会话 id 出现两次，搜索每条命中两次，其中一个的 resume 显示不可用。raw 里两份镜像都在，没有丢。

**修复**
- **认人不认名字**（`sync.rs::move_key`）：Codex 文件的身份是"去掉 `.zst` 之后的文件名"（它带会话 id）。源文件不见了、同名（可带 `.zst` 后缀）的文件出现，就进入"换了存储方式"的候选；**只有镜像里已有的内容是现在这个文件解压后内容的开头**（`mirror::content_continues`，逐块比较，新文件可以更长）才算同一个文件：更新 `source_path`、`mirror_rel`、`is_compressed`，状态保持 `present`，不算删除，不当新会话，下一步按"重写"重新索引（同一个 `file_id`，消息被替换，不重复）。内容对不上就按原来的规则：旧的记删除、新的当新文件。**无法比较（读失败）时什么都不改**，记入错误，下次再看。反向（`.zst` 解开成 `.jsonl`）同一套规则。
- **旧数据修复**（`sync.rs::join_twin_rows`，每次同步最先运行，在重建索引之前）：找"已删除的行 + 同身份、存储方式相反、且仍存在的行"，并且旧行的镜像必须是新行内容的开头，才合并：**先复制 `kb.db`**（`VACUUM INTO`，文件名 `kb.db.before-twin-join-<时间>`），再把旧行从库里去掉（连同它的会话和消息）。**两份镜像都留在 `raw/` 里，一个字节不动。** 内容不一致的不合并。
- **升级前复制 `kb.db`**：数据库版本低于当前、且里面有数据时，迁移前先复制成 `kb.db.pre-v<新版本>-<时间>`（`db::open`）。这样 0.4.0 第一次打开真实库（v3→v4）就有一份原样的副本，不需要我手动备份。
- 同时顺手修正：状态栏、首页的"已删除"计数不会再被这 214 条污染（它们已不在库里）。

**测试（`sync.rs`，临时 HOME，真的先收集 `.jsonl` 再换成 `.zst` 再同步）**

| 测试 | 断言 |
|---|---|
| `a_rollout_compressed_in_place_is_the_same_session_not_a_new_one` | 会话数不变（1），状态 `present`，`moved_sources=1, deleted=0, new_sessions=0`；`files` 里是 `.zst` 且 `is_compressed=1`；消息只索引 1 份，搜索只命中 1 次；**旧的 `.jsonl` 镜像和新的 `.jsonl.zst` 镜像都在**；再同步一次什么都不变 |
| `a_compressed_rollout_that_was_unpacked_again_is_also_the_same_session` | 反方向同样不重复 |
| `a_compressed_file_with_other_content_is_a_new_file_and_nothing_is_joined` | 内容不一致的 `.zst`：旧的 `deleted`、新的当新会话，之后的同步也不合并 |
| `sessions_stored_twice_by_an_older_version_are_joined_once_and_both_mirrors_stay` | 先造出旧版本留下的重复（搜索命中 2 次、消息 2 份），一次同步后：1 个会话、`present`、消息 1 份、搜索命中 1 次、`files` 1 行、两份镜像都在、生成了 1 个 `kb.db.before-twin-join-*` 副本、下一次不再合并 |
| `db::tests::upgrading_a_database_with_history_keeps_a_copy_and_a_new_one_makes_none` | v3 库升级时留下 `kb.db.pre-v4-*`，内容是升级前的样子；全新的库不留副本 |

**在真实库的一份副本上验证（用打包出来的 0.4.0 二进制，临时 HOME，只读真实的 `~/.claude`/`~/.codex`，`kb.db` 用 SQLite `.backup` 复制，`raw/` 用 `cp -cR`（APFS 克隆）复制）**

```
$ HOME=scratch/p4/final/home AgentKB.app/Contents/MacOS/AgentKB --kb-sync
{"status":"ok","scanned_files":309,"changed_files":7,"new_sessions":0,"deleted_sources":0,
 "moved_sources":0,"joined_twins":214,"errors":[],"duration_ms":16398}
日志：joining 214 sessions stored twice (plain and compressed); kb.db copied to …kb.db.before-twin-join-…
      indexer version changed: rebuilding 309 session rows from the mirror
文件：kb.db(210 MB)  kb.db.pre-v4-…(177 MB)  kb.db.before-twin-join-…(177 MB)
```

| 检查（副本上） | 之前（真实库） | 之后 |
|---|---|---|
| `PRAGMA user_version` | 3 | 4 |
| `sessions` | 523 | **309** |
| Codex `files`：`deleted` | 214 | **0**（`present` 32 个 `.jsonl` + 214 个 `.jsonl.zst`） |
| 全库 `deleted` 行 | 214 | **0** |
| `(source, session_id)` 重复 | 215 | **1**（见下） |
| 两份镜像 | — | `raw/codex/sessions/2026/05/29/…jsonl` 和 `…jsonl.zst` 都在 |
| 第二次同步 | — | `joined_twins:0`，`changed_files:1`（一个活跃会话），24 ms |

剩下的 1 个重复是 Claude 的子代理 `agent-a8b3c72952e6cfe40`：同一个 agent id 出现在两个不同的父会话目录下，是**两个不同的文件**（104 条和 210 条消息），都真实存在；`kb.db.before-p3.1` 里就已经是这样，和 `.zst` 无关，没动。

**真实库**：见 §7（装好后第一次定时同步的结果）。

## 3. 审核意见第 2 节的 5 条，逐条落实

| 条 | 落实 | 证据 |
|---|---|---|
| 2.1 键名 | 现有键一个没改，`notes` 只是新增的顶层键；`results`（会话命中数组）里新增 `has_note` | `sync.rs::notes_come_with_the_search_ahead_of_the_messages_and_can_be_turned_off`（`agent_json` 里 `json["notes"]`、`json["results"][0]["has_note"]`）；`every_field_the_skill_texts_read_is_in_the_default_output`（v1/v2 skill 读的字段逐个断言还在） |
| 2.2 共用额度 | 向导和设置页写"整理会用你自己的 Claude（或 Codex）额度，和你平时使用共用"（`kb.digest.sharedQuota`）；设置页显示"按最近 7 天估算，每天约 X"（`kb.digest.field.daily`）；首页卡片显示上一轮用的 token；额度不足时显示"额度不足，已暂停整理，… 之后自动重试。不是你这边的问题" | `KbDigestCard.test.tsx`："shows the last finished round with the tokens it used…"、"says plainly that a quota stop is not the user's doing"；`KbSettings.test.tsx`、`KbWizard.test.tsx` |
| 2.3 输出上限 | 15 道题（关键词版）默认输出平均 **6,688 B**（`--verbose` 22,123 B），低于 10 KB，最大一题 11,986 B；`NOTE_VERBOSE_ONLY` 为空，没有因此砍字段。关键词版期望会话进前 5 的 14/15（没进前 5 的是 `fastjson 升级 空指针` 那一题）。之前在同一个样本库上还量过：无笔记 5,420 B、整句版 7,648 B | `p4-evidence/search_size.py` + `search-size.txt`（用打包出来的二进制、步骤 4 的样本库，样本库里有 10 个会话的笔记） |
| 2.4 Codex 档位 | 1 次调用对比：低档位 26.9 s / 14,462+599 token / 2 条，默认档位同一会话 102.8 s / 3 条（设计阶段那次）。默认值 `digest.codex_reasoning="low"`，设置页不露出 | `step4-runs.json` 的 `c1`；`engine.rs` 的参数测试 |
| 2.5 `seq` 约定 | `index/messages.rs` 头部注释写明"`seq` 的编号规则一旦改动，已有的笔记会失效"；笔记 `evidence.msg_key` 记 `seq_from` 那条消息的 key，Codex 为 `null`，另记 `at`+`kind` 供按时间定位 | `sync.rs::seq_and_message_keys_are_the_same_after_incremental_builds_and_a_reindex`（增量 = 重建，`seq` 和 `msg_key` 一致）；`digest/tests.rs::a_note_remembers_where_its_evidence_starts_even_without_a_message_key` |
| 其他 | 长会话降到每段 5 万字符（未再改提示词）；首轮费用在首页提前公布，不弹确认框；`--max-budget-usd 2` 保留（只对按量计费有用，设置页以 2.2 为准）；假 AI 会读 stdin 直到 EOF 才退出 | `digest/tests.rs::the_ai_is_started_in_the_work_directory_with_input_on_stdin_and_never_in_argv`；`KbDigestCard.test.tsx` "announces the cost of the first round before it starts" |
| 步骤 4 审核新增 | 每段记录覆盖率（笔记指到的 user/assistant 消息数 ÷ 这一段的 user/assistant 消息数）：写进 `digest_items.messages/covered` 和日志（每一段一行 `piece … coverage=a/b`，每个会话一行 `coverage=a/b`），**只记录，不据此重做**；设置页提示不再提"换小模型" | `digest/tests.rs::a_night_off_is_made_up_at_the_first_check_and_the_round_completes`（末尾断言 18 条消息、覆盖数 > 0 且 ≤ 总数） |

覆盖率列加在 v4 的建表语句里（v4 还没有在任何真实库上跑过，所以没有另开 v5）。

## 4. 验收 1–11（假 AI 命令 + 假时钟，全部在临时 HOME）

命令统一是 `cd src-tauri && cargo test --lib kb::digest::tests::<名字> -- --test-threads=1`，输出都是 `test … ok`、`1 passed`（完整输出见 `scratch/p4/final/acceptance-tests.txt`，未提交）。

| # | 验收 | 测试 | 结果 |
|---|---|---|---|
| 1 | 关机一整晚，1 小时内补完整一轮 | `a_night_off_is_made_up_at_the_first_check_and_the_round_completes` | ok：03:00 过了期间不检查，00:00 的下一次检查就整理完，`round.state=done`，再检查 `idle`，调用数不变（3） |
| 2 | 额度用完：1 小时内不再调用，恢复后只补剩下的 | `when_the_quota_runs_out_nothing_is_called_for_an_hour_and_only_the_rest_is_done`、`codex_quota_and_login_problems_pause_the_same_way` | ok：第 3 次调用返回额度不足 → 暂停，1 小时内 0 次调用；之后只调剩下的；额度错误不算会话的失败次数 |
| 3 | 互斥 | `a_second_run_while_one_is_working_is_refused_without_calling_the_ai`、`a_cancelled_run_puts_the_session_back_without_a_failure` | ok：第二个运行被拒绝，假 AI 调用次数没有重复 |
| 4 | 连续失败 3 次不再重试 | `a_session_that_keeps_failing_is_given_up_after_three_attempts`、`a_timeout_counts_as_a_failure_and_leaves_no_process_behind` | ok：3 次后 `failed`，首页卡片列出错误和重试按钮；超时不留子进程 |
| 5 | 跳过规则 | `sessions_that_must_not_be_digested_alone_are_left_out`、`sync.rs::the_digest_work_directory_is_always_excluded_from_the_index` | ok：30 分钟内有新消息的、子会话、被排除的、整理工作目录里的会话都不单独整理 |
| 6 | 跨轮次只发新消息和上一版笔记 | `a_session_that_goes_on_is_continued_from_its_notes_with_only_the_new_messages`、`a_long_session_is_sent_in_pieces_and_continues_next_round_when_the_cap_is_reached` | ok：检查假 AI 收到的输入 |
| 7 | 重建 | `rebuilding_the_index_brings_every_note_back_and_search_finds_them`、`sync.rs::a_reindex_restores_the_notes_from_their_files` | ok：笔记表清空后 `--kb-reindex`，笔记全部恢复，搜索结果一致 |
| 8 | 不碰源目录 | 步骤 4 的快照对比（`p4-evidence/snap.py`、`diff.py`） | 整理前后对 `~/.claude`、`~/.codex` 拍快照：**没有新会话文件，没有整理工作目录的项目目录/会话**；只有 CLI 自己的内务文件变了（claude 的 backups 滚动、cache/model-catalog、plugins 市场刷新、两个正在使用的会话的 jsonl 修改时间；Codex 的 state/logs/models_cache）。plugins 市场刷新（547 个文件被原样重写）我没法归因到整理还是你正在用的软件 |
| 9 | 不影响收集 | `a_collection_is_not_held_up_by_a_digest_that_is_working` | ok：整理在跑时 `--kb-sync` 照常完成 |
| 10 | 发送前脱敏 | `the_text_sent_to_the_ai_has_no_secrets`、`a_secret_in_the_answer_never_reaches_the_note_file` | ok：假 AI 收到的输入里查不到测试用的假密钥；答案里带密钥也不会进笔记文件 |
| 11 | `--kb-search` ≤ 300 ms；PLAN 第 6 节全部检查 | 见下 | 发布版二进制在真实库副本上：`WindowServer` 0.01 s，`充电功率` 0.09 s，`死锁` 0.09 s；调试版在样本库上最慢 164 ms，笔记没有增加耗时。门禁见下表 |

**门禁（完整输出 `scratch/p4/final/gates.txt`，未提交）**

| 命令 | 结果 |
|---|---|
| `pnpm tsc --build .` | 通过 |
| `pnpm vitest run` | 144 个文件、**1536** 个测试全部通过 |
| `pnpm lint` | 通过 |
| `pnpm run i18n:validate` | 通过（5 个语言） |
| `cargo test -- --test-threads=1` | 1385 通过，0 失败，1 个忽略（`providers::kilo::tests::real_store_smoke_scan_and_load`，原有的） |
| `cargo clippy --all-targets --all-features -- -D warnings` | 通过 |
| `cargo fmt --all -- --check` | 通过 |

## 5. 步骤 5：P3.1 的三条待办

1. **跳转后高亮命中的消息**：底座本来就有"目标消息高亮 3 秒后淡出"（`targetMessageUuid` + `shouldHighlightTarget`），没有另做一套。真正的问题是**跳转失败**：`openKbSession` 里 `await selectSession(...)` 返回时消息还没加载。打开消息视图会触发应用自己的"消息为空就重新选择会话"那段逻辑（`useAppInitialization.ts`），第二次选择顶替了第一次，第一次提前返回，于是 `scrollToFocus` 在空列表里找不到消息，提示"没能定位到具体那条消息"。现在 `whenMessagesAreLoaded`（订阅 store，最多等 15 秒）等加载完再滚动。用无头 Chromium + 模拟 IPC 复现并验证（`p4-shots/jump-highlight.png`：滚到第 30 条，黄色高亮；5 秒后高亮消失）。新增测试 `openSession.test.ts::waits for the messages when the open call returns before they are there`。**没有做"命中词标出"**（设计里写过，审核意见只要求"高亮命中的消息"）。
2. **⌘K**（方案 A）：`openKbSearch()`（`src/kb/openSearch.ts`）切到知识库首页的"会话"页并让搜索框取得焦点（`searchFocusPending`，取一次焦点就清掉）；WebUI 没有知识库，仍然走底座的全局搜索。`useAppKeyboard.ts` 一行；**顶栏的搜索按钮**（带 ⌘K 提示）也一起改了，否则按钮和快捷键会去不同的地方（`Header.tsx` 两处）。实测：按 Ctrl+K 后 `document.activeElement` 是知识库搜索框（`p4-shots/cmdk-focus.png`）。测试：`openSearch.test.ts`、`useAppKeyboard.test.ts`、`KbSearch.test.tsx`。
3. **状态栏**：没选项目时显示"已备份 N 个会话"（`kb.status.backedUpCount`，N 是 `kb_get_status` 里各来源会话数之和），选了项目行为不变。`KbRoot` 启动时先取一次状态，不用等打开首页。截图 `p4-shots/status-bar-backed-up.png`（模拟数据 40+228=268）。

截图都是模拟数据，不含真实内容。

## 6. 和设计不一致的地方

- **失败挂起**不再用 `digest_sessions.failed_at_seq` 列，改看 `digest_items` 里 `failed` 行的 `seq_to`。
- 打开笔记对应的消息：先按 `msg_key`，找不到再按"时间 + 消息类型"（`evidence.at`、`evidence.kind`，设计里没有）。查看器不认 `seq`，Codex 消息又没有 key。
- "整理全部未整理的"不受每轮上限约束（确认框已经给了总量）。
- 每段 5 万字符；Codex 默认 `low`。
- **首次同步会强制重建一次索引**：`KbEnv::from_config` 把整理工作目录永远加进 `exclude_cwd_prefixes`，`exclude_cwd_applied` 变了，所以升级后第一次同步会从镜像重建一遍索引（副本上 9 秒）。
- 覆盖率统计是整个会话一行加每段一行，不是按"话题"。
- 新增了 `kb.db.pre-v<n>-*` 和 `kb.db.before-twin-join-*` 两份复制（各约 180 MB），**不会自动删除**，确认没问题后可以手动删。

## 7. 真实库上的结果

装好 0.4.0 后，launchd 在 23:08 的定时同步（`sync_runs` 第 113 行，`trigger=schedule`，`ok=1`）用新版升级了真实库，用时 77 s（升级 + 复制两份 `kb.db` + 合并 + 重建索引）。结果（只读查询 `~/AgentKB/kb.db`）：

| 检查 | 升级前 | 升级后 |
|---|---|---|
| `PRAGMA user_version` | 3 | **4** |
| `sessions` | 523 | **309** |
| Codex `files`：`deleted` | 214 | **0**（`present`：32 个 `.jsonl` + 214 个 `.jsonl.zst`） |
| 全库 `deleted` 行 | 214 | **0**（首页没有"已删除"徽章） |
| `(source, session_id)` 重复 | 215 | **1**（Claude 子代理 `agent-a8b3c72952e6cfe40`，两个不同文件，见 §2，和 `.zst` 无关） |
| `raw/` 里两份镜像 | — | 都在（如 `2026/05/29/…019e7371….jsonl` 和 `.jsonl.zst`） |
| 同步摘要 | — | `joined_twins:214, errors:[], deleted_sources:0` |
| 备份副本 | — | `kb.db.pre-v4-20261015T150812Z`（v3，523 个会话）、`kb.db.before-twin-join-20261015T150820Z` |

`config.json` 的 SHA-256 升级前后都是 `172c4eb4…dee9`。没有对真实库做过整理（`digest.enabled=false`）。

## 8. 全部 35 次真实 AI 调用

上限 35，已用满，之后没有再调用。token 为 CLI 报告的数字（输入 = 输入 + 缓存写 + 缓存读，按轮数累加）。

**设计阶段 18 次**（明细在 `P4-design.md` §6、`p4-evidence/runs-usage.json`）：1–5 试参数和失败形状（haiku ×4，不存在的模型 ×1）；6–9 四个样本（Claude 默认）；10 launchd 里拉起后台调用；11–16 Codex 的参数、失败形状、工具清单、fail-closed 验证；17–18 Codex 样本（14.2k/551、35.6k/2,985）。其中 #13 可能没发出请求，按 1 次算。

**步骤 4 共 17 次**（`p4-evidence/step4-runs.json`，会话都是个人会话，没碰公司工程）：

| 标签 | 会话 | 模型 | 调用 | 用时 | 输入/输出 token | 笔记条数 | 用途 |
|---|---|---|---|---|---|---|---|
| 01 | `8e6f1373` WindowServer | Sonnet 默认 | 1 | 13.8 s | 26,959 / 1,151 | 2 | 样本 |
| 02 | `019ef243` 充电功率（Codex 会话，用 Claude 整理） | Sonnet | 1 | 11.0 s | 9,484 / 1,160 | 3 | 样本 |
| 03 | `ef228e77` P1 设计那次 | Sonnet | 2 | 25.2 s | 105,609 / 2,439 | 5 | 样本（长） |
| 04a | `15bf7c02` 长会话，10 万字符一段 | Sonnet | 1 | 9.4 s | 71,529 / 703 | 2（只覆盖 #1–122） | 验证"大段只出 1–2 条" |
| 04b | 同上，改 5 万字符，#504–1093 | Sonnet | 2 | 46.0 s | 74,485 / 5,087 | 9 | 验证 5 万字符 |
| 05 | `815cfbd5` | Sonnet | 1 | 17.7 s | 41,919 / 1,991 | 4 | 样本 |
| 06 | `01a0cc1c` | Sonnet | 1 | 10.1 s | 60,514 / 666 | 2 | 样本（部分） |
| 07 | `01a0860c` | Sonnet | 1 | 23.1 s | 60,698 / 2,538 | — | 样本（部分） |
| 08 | `019fb241` | Sonnet | 1 | 9.3 s | 23,197 / 877 | — | 样本 |
| 09 | `01a0aa7f` 重庆行程 | Sonnet | 1 | 6.1 s | 14,356 / 361 | — | 非技术会话是否硬写成技术笔记 |
| 10 | `01a09ecd` cloudflare | Sonnet | 1 | 15.7 s | 25,166 / 1,658 | — | 样本 |
| h1 | `8e6f1373` | haiku | 1 | 52.1 s | 103,728 / 11,728 | 5 | 小模型对比 |
| h2 | `815cfbd5` | haiku | 1 | 71.4 s | 131,396 / 13,869 | 8 | 小模型对比 |
| h3 | `15bf7c02` #504–756 | haiku | 1 | 82.3 s | 185,221 / 17,684 | 7（Sonnet 3） | 小模型对比（长会话） |
| c1 | `019ef243` | Codex `low` | 1 | 26.9 s | 14,462 / 599 | 2 | 低推理档位对比 |

合计 17 次。**结论（leader 已采纳）**：haiku 的 token 并没有更省（输入 3–5 倍、输出 7–12 倍、慢约 5 倍，笔记更碎更多）；默认留空用 Sonnet 级；每段 5 万字符；Codex 默认低档位。haiku 对比当时没有记 `turns` 和花费，现在日志里有了，但调用额度已用完，补不了。

## 9. 已知缺口和没有验证的东西

- **被 AI 漏掉的范围不会补**：一段整理完就记为已整理，段内笔记没指到的消息（例如 `15bf7c02` 的 #123–503）不会再回头。按 leader 的决定先不加"重做"规则，覆盖率已经在记，用一周后看。
- **还没有真实验证的错误形状**：额度用完、未登录时 CLI 真实返回什么，只有假 AI 和推断；skill 模板 v3 还没被真实的 Agent 读过；launchd 环境下的整理除了设计阶段那一次实验外没有跑过。
- `INDEXER_VERSION` 没变（6）；"重建"是因为排除目录变了。
- 步骤 4 的 `P4-samples.md` 只在本机。

## 10. 用户真机清单（验收 12–14）

0. **先看 §7**：新版装好后第一次定时同步会升级库（v3→v4）、合并 214 对重复、重建一次索引，约十几秒到几十秒；之后首页"已备份 N 个会话"应当是 309 左右，没有"已删除"徽章。`~/AgentKB` 里会多出两个约 180 MB 的 `kb.db.pre-v4-*` 和 `kb.db.before-twin-join-*`，确认没问题后可以删。
1. 重新打开 App（旧窗口还是 0.3.1 的界面）。**设置 → 每日整理**：选好 AI（Claude 或 Codex），点"测试"，能通过（验收 12）。测试通过前开关打不开。**自己决定要不要打开整理**：打开后第一轮会处理最近 7 天的会话，首页会先写出预计 token，用的是你自己的额度。
2. 打开整理后的第二天早上：首页的整理卡片显示上一轮已完结和用了多少 token，"笔记"页能看到昨天会话的笔记（验收 13）。
3. 在 Claude Code 或 Codex 里问一个昨天聊过的问题（需要 skill 更新到 v3：设置里点"更新"，**不会自动更新**），看回答里有没有用到笔记（验收 14）。
4. 顺手试：⌘K 现在进知识库搜索；点搜索结果里的消息，会滚到那条并高亮几秒。
