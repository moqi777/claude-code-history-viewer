# P3 报告：全文搜索 + `--kb-search` / `--kb-show` + skill 一键安装

分支 `agent-kb`，已推送到 `origin`。全程没有向 upstream 推送，没有对原项目提 PR / issue / 评论。

主要提交：`c8c1a06` 首页说明换行 → `4573781` 按审核意见修订设计 → `401c188` 消息索引 / 分词 / 脱敏 / 搜索命令 / skill 安装后端 → `4778424` 首页搜索、技能设置、向导页和首页提示 → `5d1fa89` 检索测试集（**先于任何检索结果提交**）→ `b640d9f` 稳定分数、`AGENTKB_DATA_DIR`、`--kb-skill-print`、读取技能文件算查询本身 → 本报告所在提交。

## 1. 门禁（最终提交）

| 项 | 结果 |
|---|---|
| `pnpm tsc --build .` | 通过 |
| `pnpm vitest run` | 1492 passed（比 P2.1 多 28 个） |
| `pnpm lint` | 通过 |
| `pnpm run i18n:validate` | 通过（5 语言；ko/ja/zh-TW 的新文案是英文，按 PLAN 允许） |
| `cargo test -- --test-threads=1` | lib 1309 passed / 1 ignored（P2.1 是 1261），集成测试全绿 |
| `cargo clippy --all-targets --all-features -- -D warnings` | 0 警告 |
| `cargo fmt --all -- --check` | 通过 |

## 2. 审核意见逐条落实

| 审核意见 | 落实 |
|---|---|
| 1-1 输出加 `read_mode`，immutable 出错自动重试一次 | `search.rs`：`SearchResponse.read_mode`、`ShowResponse.read_mode`（`normal`/`immutable`）；`search_in` 和 `show_in` 遇到错误等 150 ms 重试一次。真跑验证：Codex 只读沙箱里（目录没有 `-wal/-shm`）输出 `"read_mode":"immutable"` 且结果正确，见 §4 验收 4 |
| 1-2 不改 `settings.json`，安装界面一句话说明 | 技能区块底部固定一句「第一次在 Claude Code 里用到它时会问一次，选“总是允许”即可」（`kb.skill.permissionNote`） |
| 1-3 skill 路径过期：只检测，点「更新」才写；mdfind 兜底保留 | `skill::status` 判 `stale`（版本 / 路径 / 程序已不存在）；首页横幅「Agent 技能需要更新」+「更新」按钮，点了才写（测试：`an out-of-date skill is flagged and only rewritten when the user clicks update`）；SKILL.md 末条是 `mdfind` 兜底 |
| 2.1 自己搜到自己不能整轮排除 | 按审核口径实现（设计 §5）：kb 调用及结果一律不索引（命令识别 + marker 双保险）；纯问答轮的提问和回答也不索引；做了别的事的轮照常索引；`seq` 给被跳过的消息占位。测试见 §4 验收 7 |
| 2.2 输出前脱敏 | 新文件 `kb/redact.rs`（第 4 期复用）：键值对（password/passwd/secret/token/api_key/apiKey/access_key/client_secret/private_key、`--password x`）、`Authorization: Bearer/Basic`、`sk-…`、`ghp_/gho_/…`、`github_pat_`、`AKIA…`、`xox…`、`AIza…`、JWT、私钥块、URL `user:pass@`；**建索引时**脱敏（`messages.text` 和 FTS 都没有原值），先脱敏后截断（比上限多 512 字节的窗口里脱敏，不会被截断点劈开）；搜索结果的标题/片段和 `--kb-show --full` 读回的原文再过一遍。单测 6 个覆盖每类 + 反例（`token_count`、`max_tokens`、`tokens`、`secret garden`、空值 `password:`、`passwordless`、`Version: 2.1.296`），`secrets_never_reach_the_index` 端到端确认库里和搜索里都找不到密钥原值 |
| 3 SKILL.md 加「不要加管道 / cd / 拼接」 | 已加（第一条注意事项），`the_two_templates_differ_only_by_the_permission_line` 断言它在两份模板里 |
| 3 测试集不能用测试会话 | 15 题的期望会话都是真实工作会话，不含 `scratch/…agent-test*`，也不含本项目自己的开发会话（`docs/agent-kb/P3-eval.md`） |
| 3 Agent 真实测试不装进真实 skills 目录 | 技能装在项目级目录（Claude：`.claude/skills/agentkb/`；Codex：`.agents/skills/agentkb/`），在 `scratch/p3/agent-test/` 下；结束后 `ls ~/.claude/skills ~/.codex/skills` 里没有 `agentkb` |
| 3 第一次在真实库建索引前备份 `kb.db`，报告里写大小和耗时 | 备份到 `~/AgentKB/kb.db.before-p3`（`sqlite3 .backup`，`integrity_check` ok，287 个会话）。真实库首次建索引见 §5 |
| 3 Codex 分叉会话重复结果，本期不处理 | 不处理 |

## 3. 步骤 0（P2 遗留）
首页说明文字加了 `text-wrap: balance`，不再让句号单独落到第二行。**只是改了样式，没有在真实窗口里看效果**（无头渲染里看起来正常）。

## 4. 验收标准（自己验证的 8 项）

### 4.1 检索测试集前 5 条命中率 ≥ 80%
15 题在检索之前就写好并提交（`P3-eval.md`，提交 `5d1fa89`）；问法按回想口吻写、不照抄原话。每题两种查询：整句（Agent 没提炼关键词的最差情况）和关键词（SKILL.md 教的用法）。命令：`python3 scratch/p3/eval.py`（同一份脚本对真实库再跑一遍：`eval_real.py`，结果相同）。

| # | 整句 | 关键词 | 期望会话 |
|---|---|---|---|
| 1 | 第 1 | 第 2 | codex `019ef330`（502） |
| 2 | 第 1 | 第 1 | codex `019ef243`（充电功率，有干扰会话） |
| 3 | 第 2 | 第 2 | codex `019ef2b4`（Dubbo） |
| 4 | 第 1 | 第 1 | codex `01a0aa7f`（重庆攻略） |
| 5 | 第 1 | 第 1 | codex `019fb241`（Claude 订阅） |
| 6 | 第 1 | 第 1 | codex `019f79e1`（图片转 Excel） |
| 7 | 第 1 | 第 1 | codex `019fbd9b`（agent-browser） |
| 8 | 第 1 | 第 2 | codex `019ef305`（访达右键新建） |
| 9 | 第 1 | 第 1 | codex `019f640f`（熄屏 / 睡眠） |
| 10 | 第 1 | 第 1 | codex `01a0424c`（tedis） |
| 11 | 第 1 | 第 1 | claude `00d3c5de`（提醒渠道） |
| 12 | 第 1 | 第 1 | claude `a5731e79`（giftInfo） |
| 13 | 第 1 | 第 1 | codex `019ef25a`（Ghostty 目录） |
| 14 | 第 1 | **未命中** | codex `01a03daf`（fastjson 评审） |
| 15 | 第 1 | 第 1 | claude `630f9b1b`（红手指） |

**整句 15/15 = 100%，关键词 14/15 = 93%**，都在前 5 内、都 ≥ 80%。
未命中的 #14（关键词 `fastjson 升级 空指针`）逐条分析：期望会话排第 7。原因是**词汇不匹配 + 通用词**：会话里讲空指针用的是 `NPE` / `null`，从没出现「空指针」；「升级」是个通用词，几个很长的会话靠它和别的词累计拿了更高的分。同一题用整句查询第 1 就命中（整句多出 `评审`、`风险` 等词，这些词在期望会话里出现；这是对原因的推断，没有逐词核对）。Codex 真跑那题时先后搜了 `fastjson 升级`、`fastjson 空指针`、`JSON升级 NPE`，换词后命中（`codex-3.txt`）。**没有调排序系数**（审核允许调一轮，结果已达标，不拿测试集过拟合）。
局限：只有 15 题；本机 Codex 会话占绝大多数（12 题 Codex、3 题 Claude）。

### 4.2 性能

| 指标 | 验收线 | 实测（本机真实数据，2.8 GB、302 个会话、84,117 条消息） |
|---|---|---|
| 一次查询（`--kb-search` 整个进程） | ≤ 300 ms | **80–135 ms**（15 题 p50 119 ms，max 135 ms；在真实库上 80–90 ms）；其中 jieba 加载约 75–85 ms |
| 增量索引额外耗时 | ≤ 2 s | **量不出来的小**：真实数据副本上连续同步，有 1 个活跃会话在增长时整次同步 13–125 ms，无变化时 6 ms（这是整次同步而不是「额外」耗时） |
| 首次建索引 / `--kb-reindex` | — | 副本上 7.2–14.0 s（整次同步），真实库首次 6.8 s（含 17 个有变化的文件） |
| 索引大小 | — | `messages` + FTS 约 101 MB；真实库 `kb.db` 109 MB（原始数据 2.8 GB 的 3.9%） |
| 二进制 | — | 29.0 → 32.7 MB（+3.7 MB），`.app` 32 MB |

### 4.3 `--kb-reindex` 后与增量结果一致
- 真实数据副本上：先同步到稳定（`changed_files: 0`），记录 `messages` 的 `(file_id, seq, kind, byte_off, ts, msg_key, text, truncated)` 的 SHA：`efdf93d3a7f4fc37`（84,052 行）；`--kb-reindex`（14.0 s）后 SHA 同为 `efdf93d3a7f4fc37`，**逐行一致**，FTS 行数 = 消息行数。
- 过程中发现一个问题并修了：FTS5 的 contentless 表删除行后不归还统计信息，连续重建 bm25 分数会漂移（43.36 → 45.06），排名不变但分数变。现在重建前先 `DROP` 再建表（`db::reset_message_index`），连续两次重建分数逐位相同（`[36.86, 19.95, 15.15, 15.15, 15.15]`）。单测 `messages_are_searchable_and_an_incremental_build_equals_a_rebuild` 覆盖行与分数。

### 4.4 在 Claude Code 和 Codex 里各真跑 3 个问题
环境：Claude Code 2.1.299、codex-cli 0.162.1；技能装在项目级目录（没碰真实 skills 目录）；用 `AGENTKB_DATA_DIR` 指向真实库的**副本**（真实库当时还没建索引）；问题只用大白话、不提 AgentKB。完整记录在 `docs/agent-kb/p3-transcripts/`。

| 工具 | 问题 | Agent 的动作 | 结果 |
|---|---|---|---|
| Claude | 我之前是不是问过怎么看 Mac 当前实际的充电功率？ | 自己调用 Skill → `--kb-search "充电功率 Mac"` → `--kb-show … --around 6` | 给出两条命令、测得 140W/74W，附会话标题、日期 2026-06-23、项目 JavaCode、`codex resume 019ef243-…`，还指出另一个相关会话 |
| Claude | 之前让你做过去重庆旅游的攻略，航班是哪几天？ | Skill → 搜索 → show → 再搜一次确认有无改动 | 9/17 去、9/21 回及航班表，附会话、日期和 resume 命令 |
| Claude | 以前有没有碰到过 Dubbo 提示找不到可用 provider？ | Skill → 搜索（33 个会话）→ show ×3 | 汇总 3 个会话的排查顺序，诚实指出「记录里没有看到最终是哪一条解决的」，附三个 resume 命令 |
| Codex | 我之前问过 Mac 访达右键新建文件，推荐了什么工具？ | `cat SKILL.md` → 搜索 → show | New File Menu / iBoysoft MagicMenu，附标题、日期、`codex resume …` |
| Codex | 之前讨论过自己掏钱订阅 Claude 值不值？ | 同上，show ×2 | 混合工作流的结论，附出处与命令 |
| Codex（只读沙箱） | 以前评审 fastjson 升级，发现过哪些空指针风险？ | 搜索 ×7（换词、查类名）+ show ×2 | `JsonUtil` 把空白串解析成 null、`SignFlowManagerImpl`/`LoanOrderServiceImpl` 直接解引用，附出处与命令 |
| Codex（只读沙箱，目录没有 `-wal/-shm`） | 之前问过本机多久熄屏、多久睡眠？ | 搜索 → show | 输出里 `"read_mode":"immutable"`，答案正确，没有创建 `-shm` |

Claude 的 6 次查询运行（3 次 + 自搜测试 3 次）**permission_denials 全为空**（第 7 次「搜完写文件」的运行里 `Write` 没有放行，被拦了，那是我测试参数的疏忽，与技能无关；技能用 `--allowedTools "Skill(agentkb)"` 预先放行，模拟用户第一次选了「总是允许」；不预先放行时第一次 `Skill(agentkb)` 会被拦，已在设计里说明）。

### 4.5 resume 命令真能回到那个会话
- Claude：搜索结果给出 `cd '<cwd>' && claude --resume aebde981-…`，原样执行并追加 `-p` 提问，会话回答出这个会话里才有的暗号 `TANGERINE-7`。
- Codex：`codex resume 01a13b88-…`（在 `/Users/mo7` 下执行，不是会话所在目录），Codex 显示 “Resuming session…” 并询问用会话的工作目录（`…/agent-test/P3X`）还是当前目录——会话按 id 找到了。（交互界面用 pty 驱动，没有进一步进入对话。）

### 4.6 安装 / 卸载前后 `~/.claude`、`~/.codex` 除 `skills/agentkb/` 外零变化
Rust 单测 `install_status_update_and_uninstall_touch_only_the_skill_folder`：临时 HOME 里放好 `projects/…`、`settings.json`，拍文件清单快照 → 安装 → 对比（只多出 `.claude/skills/agentkb/SKILL.md` 一个文件，原有文件全部不变）→ 卸载 → 对比（回到原样，`skills/agentkb` 不存在）。另有：别人写的同名技能不覆盖不卸载、文件夹里有别的文件时卸载只删 `SKILL.md`、符号链接拒绝、debug / 临时路径拒绝写入、更新后状态回到 current。**没有对真实目录做过任何写入**（`ls ~/.claude/skills ~/.codex/skills` 无 `agentkb`）。

### 4.7 自己搜到自己
用最终二进制，在三个独立会话里对 Claude、Codex 各连续问同一个问题（纯问答轮），再同步一次：
- 最终二进制下 Claude、Codex 各 3 次相同提问产生的会话，以及前几轮真跑留下的纯问答会话（共 14 个：Claude 7、Codex 7），在 `messages` 里的行数都是 **0**（问题、回答、`--kb-search`/`--kb-show` 调用和结果全不在索引里）；
- 第 4 次搜「充电功率」「订阅 值不值」时，这几个会话都没有出现；唯一出现的一个 `P3C` 会话是我最早那次**没放行 Skill** 的尝试（Agent 因被拦而去 `ls`、`Read`、`which`，这些是真正的工具调用，所以那一轮按规则被索引了），这是规则的预期行为；
- 「搜完之后接着改代码」的轮：用户提问、助手回答和 `Write` 调用都能搜到（`TANGERINE-7` 命中该会话的 user / assistant / tool_use），而 kb 调用和结果搜不到。
- 自动化：`searching_the_history_does_not_make_it_searchable`（两个纯问答轮 + 一个搜了又提交的工作轮）、`a_turn_that_searched_and_then_did_work_keeps_the_work`、`the_decision_survives_a_sync_boundary_in_the_middle_of_a_turn`（问答轮被撤回后，下一次同步追加了真实工具调用，被撤回的行补回来）、`incremental_and_one_shot_runs_give_identical_rows`、`codex_reading_the_skill_file_is_part_of_the_lookup_not_work`。
- 真跑中发现并修了一个问题：Codex 加载技能是先 `cat …/skills/agentkb/SKILL.md`，这条命令原先被当成「真正的工具调用」，让纯问答轮整轮被索引；现在读取技能文件也算查询本身（`INDEXER_VERSION` 升到 5）。
- 已知边界：如果 Agent 在一次查询中还做了别的探索命令（`ls`、`which agentkb` 之类），这一轮就算「做了别的事」，会被索引。

### 4.8 PLAN 第 6 节的检查
见 §1，全部通过；报告已写，已推送。

## 5. 真实库首次建索引
- 先备份：`~/AgentKB/kb.db.before-p3`（950,272 字节，`integrity_check` ok，287 个会话）。
- 然后在安装新版 App 之后手动跑一次与定时任务相同的 `--kb-sync`：**6.8 s**，`status: ok`，302 个会话（其中本次新收 15 个，来自我的测试和这段时间的新会话），84,117 条消息，`kb.db` **109,174,784 字节**（`messages` + FTS 约 101 MB）；`user_version` 1 → 2；`indexer_version` = 5。
- 在真实库上用 `/Applications/AgentKB.app/Contents/MacOS/AgentKB --kb-search` 重跑了测试集（结果与副本一致：15/15、14/15），查询 80–90 ms。
- 旧版（0.2.0）读索引未建好的库：`--kb-search` 返回退出码 3 和 `{"error":"the search index is still being built…"}`。

## 6. 与设计的差异 / 新发现
见设计文档 §12，要点：去掉 `kb_turns` 列；`INDEXER_VERSION` 最终为 "5"；重建时 DROP 重建 FTS 表以稳定分数；新增 `AGENTKB_DATA_DIR` 和 `--kb-skill-print`（只读 / 只打印，没有加命令行安装入口）；向导里的技能一步放在完成页；首页「搜索标题」输入框被全文搜索取代。

## 7. 限制与未验证（如实）
1. **没有在真实窗口里看过首页搜索、跳到消息、技能设置**。界面只有 `src/kb` 下 98 个测试 + 无头 Chromium 模拟 IPC 的截图（`docs/agent-kb/p3-shots/`）。「点结果跳到那条消息」的实现（Claude 用 `uuid`；Codex 在完整消息列表里按时间戳 + 角色匹配，找不到就提示「没能定位到具体消息」）只有单测，**需要你真机验证（验收 11）**。
2. Codex 的 `untrusted` 审批策略没测；只测了 `workspace-write` 和 `read-only` 两种沙箱（`approval_policy=never`，你自己的配置是 `danger-full-access`）。
3. `codex resume` 只验证到「按 id 找到会话并询问工作目录」，没有进入对话。
4. 检索测试集只有 15 题；#14 关键词版未命中（见 4.1）。
5. 中文分词是词典切词，行话和内部系统名可能被切碎（英文标识符不受影响）。
6. immutable 回退只在「没有进程持有 kb.db」时才会走到，窗口很窄（设计 6.2 已说明）。
7. Claude 第一次调用 `Skill(agentkb)` 会问一次（审核已同意）。
8. 改写过的会话旧版本不进索引（设计 1.4）。

## 8. 测试痕迹（请知悉）
为了真跑 Agent，我在真实的 `~/.claude/projects/-Users-mo7-Dev-JavaCode-claude-code-history-viewer-scratch-p3-agent-test-*` 和 `~/.codex/sessions/` 下产生了十几个测试会话（cwd 都在 `scratch/p3/agent-test/` 下，很好认），其中有的已经被收进你的真实库，会出现在搜索结果里（例如搜「死锁」会命中「我之前是不是碰到过一个死锁的问题」那几个）。我没有删除它们（不动源目录）。如果想让以后不再收这类会话，可以在设置里的「排除的目录」加一行：`/Users/mo7/.claude/projects/-Users-mo7-Dev-JavaCode-claude-code-history-viewer-scratch`（只能排除 Claude 这一侧；Codex 的会话文件按日期存放，没法按工作目录排除，数量很少可以忽略）。已收进库的行不会自动消失。

## 9. 改动清单与依赖
**新增（Rust）**：`kb/redact.rs`、`kb/search.rs`、`kb/skill.rs`、`kb/index/messages.rs`、`kb/index/tokenize.rs`。**修改（Rust）**：`db.rs`（迁移 v2、`apply_message_ops`、`purge_messages`、`reset_message_index`）、`index/mod.rs`（`Indexer` 带消息抽取器）、`sync.rs`（`INDEXER_VERSION` "5"、`force_reindex`、提交时写消息）、`cli.rs`（`--kb-search`、`--kb-show`、`--kb-reindex`、`--kb-skill-print`）、`commands.rs` + `lib.rs`（`kb_search`、`kb_skill_status/install/uninstall/test`）、`mod.rs`（版本 0.3.0）。
**前端（`src/kb/`）**：`KbSearch.tsx`、`SkillField.tsx`、`searchHit.ts`，改了 `KbHome.tsx`（搜索框、技能横幅）、`KbSettings.tsx`、`KbWizard.tsx`、`openSession.ts`（`focus`）、`useKbStore.ts`、`api.ts`、`types.ts`、i18n `kb.json` ×5。
**底座文件**：无新增改动（`navigateToMessage`、`fetchFullSessionMessages` 是已有接口）。
**新依赖**：`jieba-rs 0.7`（MIT，+约 3.7 MB 二进制）。SQLite FTS5 来自已有的 `rusqlite` bundled，前端无新依赖。

## 10. 请你真机确认（验收 9–11）

先决条件：`/Applications/AgentKB.app` 已是新版本（0.3.0）。你现在运行的那个实例是旧进程，请**退出后重新打开**。真实库的索引我已经建好了。

**9. 在 App 里安装技能，然后在 Claude Code 里问一个老问题**
1. 打开 AgentKB →「设置」→ 找到「让 Agent 能查历史」→ 点 Claude Code 一行的「安装」，Codex 也点「安装」。预期：状态变成「已安装」，弹出「技能已安装」。
2. 点「测试」。预期：显示「找到 N 个会话，用时 X 毫秒」。
3. 在**新开的** Claude Code 会话里（随便哪个目录）问一个你记得的老问题，用大白话，别提 AgentKB，比如「我之前是不是问过怎么看 Mac 的充电功率」。**第一次**它会问是否允许使用 `agentkb` 技能，选「总是允许」。预期：Agent 自己搜，回答里带会话标题、日期和 `resume` 命令。

**10. 在 Codex 里问一个**
在新开的 Codex 会话里问另一个老问题。预期同上（Codex 不会弹确认）。

**11. 首页搜两个字的中文词，点结果跳到那条消息**
1. 打开 AgentKB 首页，在搜索框里输入「死锁」（或你记得的另一个两字词）。预期：几百毫秒后列表换成搜索结果，每个会话下有带高亮的片段。
2. 点一条片段。预期：打开那个会话并滚动到那条消息（Claude 的会话直接定位；Codex 的会话按时间匹配，若没定位到会提示「已打开会话，但没能定位到具体那条消息」，请告诉我是哪一条）。
3. 清空搜索框，列表恢复成会话列表。

**可选**：如果不想保留技能，在设置里点「卸载」，预期两个目录里的 `agentkb` 文件夹消失。
