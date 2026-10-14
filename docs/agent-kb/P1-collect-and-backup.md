# 第 1 期：后台收集 + 原样备份 + 会话索引

> 先读 `PLAN.md`，第 3 节的架构决定和第 6 节的通用约束都适用于本期。
> 遇到 **【检查点】** 必须停下，把产出交给用户转给 leader 审核，通过后才能继续。

## 1. 本期目标

做完后，用户的电脑上会：

1. 每隔 N 分钟（默认 10 分钟）由 launchd 自动运行一次 `--kb-sync`，App 窗口没开也照常运行
2. 把 Claude、Codex 的会话文件**增量、原样**镜像到 `~/AgentKB/raw/`
3. 在 `~/AgentKB/kb.db` 里维护文件游标和会话索引
4. App 里能看到收集状态，有"立即收集"按钮，能浏览已经备份的会话，**包括源文件已被删除的会话**

## 2. 范围

**本期要做：** Claude 和 Codex 两个来源的增量镜像、游标、会话索引、`--kb-sync` 无界面命令、launchd 定时任务的安装 / 卸载 / 状态查询、简单的状态面板和"立即收集"按钮、在界面中浏览备份的会话、测试。

**本期不做（不要提前做）：** 全文搜索、AI 整理、设置向导、第二备份位置、压缩、Windows 定时任务、Claude Cowork 来源、导出迁移。

## 3. 已核实的数据事实（leader 在用户本机查过，可以直接用）

**Claude Code**
- 位置：`~/.claude/projects/<cwd 编码>/<sessionId>.jsonl`；子代理会话在 `<sessionId>/subagents/*.jsonl`；同一目录下还有 `memory/` 等非会话目录，要跳过
- 目录名是 cwd 的有损编码，项目路径要读记录里的 `cwd` 字段
- 记录 `type` 包括：`user`、`assistant`、`attachment`、`ai-title`（字段 `aiTitle`，用作标题）、`last-prompt`、`mode`、`permission-mode`、`file-history-snapshot`、`summary` 等；`user`、`assistant` 记录带 `uuid`、`parentUuid`、`sessionId`、`timestamp`、`cwd`、`gitBranch`、`entrypoint`、`isSidechain`
- resume 或 fork 会把旧消息复制进新文件，同一个 `uuid` 可能出现在多个文件里（本期只做镜像和会话级索引，暂不影响；第 3 期建消息表时要去重）
- 默认 30 天后删除旧文件（用户本机已改成 3650 天，但程序不能依赖这个设置）

**Codex（CLI、VS Code 插件、Codex Desktop 都写同一个地方）**
- 位置：`~/.codex/sessions/YYYY/MM/DD/rollout-<时间>-<sessionId>.jsonl`；归档后**移动**到 `~/.codex/archived_sessions/`
- 第一行 `type=session_meta`，`payload` 里有 `id`、`cwd`、`originator`（如 `codex-tui`、`Codex Desktop`）、`source`（`cli`、`vscode`，子代理会话是一段 JSON，带 `parent_thread_id`）、`git`、`timestamp`
- 正文在 `type=response_item`（message、function_call、custom_tool_call 及其输出、reasoning）；另外有大量 `event_msg`、`token_count`、`turn_context`、`world_state`、`compacted`
- 标题：`~/.codex/state_5.sqlite` 的 `threads.title`（**只读打开**；注意文件名里的版本号以后可能会变，比如 `state_6`），或者 `~/.codex/session_index.jsonl` 里的 `thread_name`
- 用户本机规模：Codex 227 个会话、2.5GB；Claude 37 个文件、320MB

## 4. 执行步骤

### 步骤 0：准备
- 从 `main` 拉出分支 `agent-kb`
- 防止误推原项目：执行 `git remote set-url --push upstream DISABLED`（只禁止推送，`fetch` 不受影响）
- `pnpm install`，然后跑一遍 PLAN 第 6 节列出的全部检查，**记录底座本身原有的失败项**，免得之后把它们算到我们头上
- 把 `docs/agent-kb/` 作为第一个提交，推送到 `origin/agent-kb`

### 步骤 1：写设计说明 `docs/agent-kb/P1-design.md` 【检查点】

先读代码，再回答下面这些问题。每个问题都要写出你依据的文件和函数名：

1. **展示备份**：底座的 Claude、Codex provider 是怎么确定根目录的？能不能让它们直接读 `~/AgentKB/raw/claude`、`~/AgentKB/raw/codex`（镜像保持和源目录一样的结构，就是为了这个）？给出改动最小的方案，要求用户在界面上能看到源文件已被删除的会话
2. **复用解析**：提取会话元数据（会话 id、cwd、标题、开始和结束时间、消息数、父会话）时，底座里哪些函数可以复用？哪些需要新写？增量解析（只解析新追加的行）怎么做？
3. **无界面入口**：`--kb-sync` 等命令放在 `lib.rs::run` 的什么位置分发？怎么保证不启动窗口、不被 single-instance 拦截？
4. **并发锁**：定时任务和界面里的"立即收集"可能同时运行，用什么方式加锁？进程崩溃后留下的锁怎么处理？
5. **数据库**：表结构草案和以后升级表结构的方式
6. **改动清单**：要新增和修改哪些文件，每个底座文件改动大约多少行
7. **新依赖**：需要加哪些 crate，为什么需要（例如计算哈希用的 `sha2`）

写完后提交并推送，然后**停下**，等待审核。

### 步骤 2：增量镜像（核心）

**扫描范围**
- `~/.claude/projects/**/*.jsonl`（包括 `subagents/`）
- `~/.codex/sessions/**/rollout-*.jsonl`、`~/.codex/archived_sessions/**/rollout-*.jsonl`
- 如果底座已经支持 `CLAUDE_CONFIG_DIR`、`CODEX_HOME` 这类自定义目录，就沿用底座的定位逻辑
- 配置里的"排除路径前缀"列表要生效（第 4 期会把 AI 整理用的专用目录加进去）

**每个文件的处理规则**

| 情况 | 处理方式 |
|---|---|
| 大小和修改时间都没变 | 跳过，不读文件内容 |
| 文件变大，开头 4KB 的哈希没变 | 从游标位置读到**最后一个换行符**为止，追加到镜像文件；没写完的半行留到下次 |
| 文件变小，或开头 4KB 的哈希变了（被重写了） | 把旧镜像改名为 `<原文件名>.prev-<时间戳>`（**不要用 .jsonl 结尾**，避免被当成会话读进来），然后完整复制一份新的 |
| 源文件消失了 | 镜像**永远保留**，会话标记为"源文件已删除" |
| Codex 文件从 `sessions/` 移到了 `archived_sessions/` | 按会话 id 认出是同一个会话，只更新源路径；镜像路径不变，不能出现两份 |

**崩溃安全（必须做到）**
- 顺序是：先追加镜像并 fsync，再更新游标
- 每次追加前检查镜像长度是否等于游标位置：如果镜像比游标长，说明上次在两步之间崩溃了，先把镜像截断到游标位置再追加。这样无论在哪一步崩溃，重跑都不会产生重复内容
- 游标和会话索引的更新放在同一个数据库事务里

**会话索引**（也要增量更新，只解析新追加的行）

字段：会话 id、来源（claude / codex）、客户端（cli / vscode / desktop 等）、cwd、项目名（本期先取 cwd 的最后一级目录名）、标题、开始时间、最后更新时间、消息数（只统计 user 和 assistant）、父会话 id、源路径、镜像路径、源文件状态。

标题按优先级取：Claude 用最新一条 `ai-title`；Codex 用 `threads.title`，取不到再用 `session_index.jsonl`；都没有就用第一条用户消息的前 50 个字。

**其他要求**
- 遇到未知的记录类型不能报错，按类型计数，写进日志
- 首次全量同步（约 2.8GB）必须流式处理，内存峰值 300MB 以内，并记录耗时
- 没有任何变化时，一次同步耗时在 2 秒以内
- 每次运行结束，往 stdout 输出一行 JSON 摘要：扫描文件数、变化文件数、复制字节数、新增会话数、错误列表、耗时（第 2 期的测试按钮要用）
- 日志写到 `~/AgentKB/logs/sync-YYYY-MM-DD.log`，保留 14 天

### 步骤 3：定时任务

- 命令：`--kb-schedule install | uninstall | status`
- `install`：写入 `~/Library/LaunchAgents/<应用标识>.kb-sync.plist`，参数如下：
  - `ProgramArguments`：当前可执行文件的绝对路径加 `--kb-sync`
  - `StartInterval`：取配置里的收集间隔，默认 600 秒
  - `RunAtLoad=true`：开机或重新登录后立即补跑一次
  - 标准输出和标准错误写到 logs 目录
- `status`：返回是否已安装、是否已加载、上次运行时间和结果
- 开发阶段用 debug 版可执行文件路径测试，在报告里注明。正式打包后的路径问题留到第 2 期处理

### 步骤 4：界面（最小可用，第 2 期再做完整的设置向导）

- 新增一个"知识库"面板，显示：数据目录、定时任务状态、上次收集的时间和结果、按来源统计的会话数、"源文件已删除"的会话数
- "立即收集"按钮：在进程内调用同一套同步逻辑，必须遵守并发锁。如果已经有同步在跑，按钮显示"收集中"
- 按步骤 1 审核通过的方案，在界面里浏览备份的会话；源文件已删除的会话要有明显标记

### 步骤 5：测试

**Rust 单元测试**（使用临时目录，模拟 HOME）必须覆盖以下场景：新文件；追加；半行到下次补齐；没有变化；文件被重写；源文件被删除；Codex 移到归档目录；崩溃恢复（镜像比游标长）；同时运行两次（第二次被锁挡住）；同步 3 次后镜像与源文件逐字节一致。

**真实数据验证**（在用户本机执行，命令和输出都写进报告）：
- 首次全量同步：记录耗时、内存峰值、镜像总大小
- 用 `cmp` 或哈希比对**所有**镜像文件和源文件，列出不一致的文件和原因（只允许是正在写入的文件）

## 5. 验收标准（leader 会逐项检查）

1. 镜像与源文件逐字节一致，只有当时正在写入的文件可以例外
2. 连续运行两次同步，第二次复制 0 字节，耗时 2 秒以内
3. 新开一个 Claude 会话和一个 Codex 会话并聊几句，一个收集周期内它们出现在镜像里，App 里能看到
4. 删除测试：复制一个会话到 `~/.claude/projects/kb-test-<随机名>/`，同步，删掉它，再同步。App 里仍然能看到这个会话，并标记"源文件已删除"。**禁止删除用户的真实会话文件**
5. 定时任务：`status` 显示已加载；日志显示每 N 分钟运行一次；注销后重新登录，会立即补跑一次
6. 源目录没有被动过：同步前后对 `~/.claude/projects` 和 `~/.codex` 做一次修改时间快照比对，除了 Agent 自身正常写入的文件，不能有任何变化
7. PLAN 第 6 节列出的全部检查通过（不包括步骤 0 里记录的底座原有失败项）
8. 写好 `docs/agent-kb/P1-report.md`：改了哪些文件、每条验收标准的验证命令和输出、偏离任务的地方和原因、遗留问题。所有改动已提交并推送到 `origin/agent-kb`

## 6. 注意

- 镜像不压缩，大约要多占 2.8GB 硬盘，这是已知并接受的，压缩以后再做
- 如果遇到本文件没有覆盖到、而且会影响设计的情况，先停下来提问，不要自己拍板
