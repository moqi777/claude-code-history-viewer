# Agent 知识库（AgentKB）总体方案

> 本文件由方案负责人（leader）维护，执行 AI 只读不改。每一期的具体任务见同目录下 `P<n>-*.md`。

## 1. 目标

在本项目（claude-code-history-viewer，下称"底座"）基础上二开一个桌面 App，统一管理 **Claude Code** 和 **Codex**（CLI / VS Code / 桌面端）的本地历史会话：

1. **自动收集**：后台定时增量收集新会话，App 窗口没开也要照常运行
2. **原样备份**：原始 JSONL 完整镜像到 App 自己的数据目录，源文件被删除（Claude 默认 30 天清理）也不丢
3. **每日整理**：调用本机的 `claude -p` / `codex exec`，把会话提炼成知识笔记（问题、原因、解决办法），每条可追溯到原会话
4. **随时能查**：App 内可浏览、搜索原始会话和笔记；提供一个 skill，一键装进 Claude Code 和 Codex，让两个 Agent 用自然语言查历史
5. **好配置、好迁移**：首次设置向导、所有参数可配置可测试、一键导出导入到另一台设备

## 2. 底座评估结论（第 0 期，已完成）

| 项 | 结论 |
|---|---|
| 许可证 | MIT，可以二开 |
| 活跃度 | 1322 次提交，8 月以来 297 次，维护活跃 |
| 技术栈 | Tauri 2 + Rust；React 19 + zustand + Tailwind；已有 `rusqlite`（bundled）、`zstd`、`notify` 依赖 |
| 已有能力 | Claude、Codex 等多 Agent 会话解析与展示（`src-tauri/src/providers/`）；消息搜索（`commands/session/search.rs`，实时扫描文件，**没有持久索引**）；手动归档（`commands/archive.rs`，复制到 `~/.claude-history-viewer/archives/`）；文件监听（`commands/watcher.rs`）；无界面模式 `--serve`、`--export`；i18n 5 种语言，已含简体中文 |
| 缺失能力 | 不依赖窗口的定时收集、持久数据库与索引、增量游标、每日整理、skill 安装、设置向导、迁移 |
| 本机环境 | cargo 1.97、node 20、pnpm（corepack）可用；`just` 已安装（`pnpm exec tauri dev` 的 beforeDevCommand 依赖它） |

## 3. 关键架构决定（执行 AI 必须遵守）

1. **新代码集中放置，尽量不改底座**
   - Rust：`src-tauri/src/kb/`；前端：`src/kb/`（组件、store、页面）
   - 对底座已有文件只做最小接入改动（注册命令、加菜单入口、加路由），方便以后合并上游更新
   - 不删除底座的其他 provider（Gemini、Cursor 等），保持原样即可；知识库功能只覆盖 Claude 和 Codex
2. **一个程序，两种运行方式**
   - 有界面：正常打开 App
   - 无界面：同一个可执行文件带参数运行，例如 `--kb-sync`、`--kb-digest`、`--kb-search`，执行完就退出。定时任务和 skill 都调用这种方式
   - 无界面模式**不能**启动窗口，也不能被 single-instance 插件拦截（参考底座已有的无界面模式 `--export`：在 `lib.rs::run` 最前面分发，执行完直接退出）
3. **定时由操作系统负责**：macOS 用 launchd（`~/Library/LaunchAgents/`），Windows 用任务计划程序（第 7 期）。App 只负责生成和注册定时任务，不在后台常驻
4. **数据目录（默认 `~/AgentKB/`，可配置）**
   ```
   ~/AgentKB/
   ├── raw/claude/projects/...      # 原样镜像 ~/.claude/projects，目录结构保持一致
   ├── raw/codex/sessions/...       # 原样镜像 ~/.codex/sessions 和 archived_sessions
   ├── kb.db                        # SQLite（WAL 模式）：游标、会话索引，后续加消息、全文索引、笔记
   └── logs/
   ```
   配置文件固定放在 `dirs::config_dir()/agent-kb/config.json`，里面记录数据目录等设置，这样无界面模式也能读到
5. **原始镜像是唯一可信来源**：数据库可以随时从 `raw/` 完整重建（`--kb-reindex`）
6. **只读源目录**：对 `~/.claude`、`~/.codex` 只读，绝不写入、改名、删除。Codex 的 sqlite 只能以只读方式打开
   - **唯一例外**（第 3 期起）：用户在 App 里点"安装 skill"时，可以写 `~/.claude/skills/agentkb/` 和 `~/.codex/skills/agentkb/`；卸载时只删这两个目录
7. **解析逻辑优先复用底座**：Claude、Codex 的解析先看 `providers/` 里现有实现能不能复用，不行再写，不要重复造一套
8. **知识库只在桌面版提供**：WebUI（`--serve`）模式不提供 `kb_*` 命令和入口。tauri-axum 对等检查遇到 `kb_*` 命令时视为白名单例外，不算不一致
9. **App 身份**：显示名 `AgentKB`，标识 `io.github.moqi777.agentkb`，定时任务 Label `io.github.moqi777.agentkb.sync`。自动更新关闭，反馈入口指向 `moqi777/claude-code-history-viewer`

## 4. 分期计划与验收标准

| 期 | 内容 | 验收标准 |
|---|---|---|
| 0 | 底座评估 | ✅ 已完成，见第 2 节 |
| 1 | 后台收集 + 原样备份 + 会话索引 | ✅ 已验收，见 `P1-acceptance.md` |
| 2 | 设置向导 + 配置 + 知识库主页 + 移动硬盘备份 + 正式打包（AgentKB） | ✅ 已验收，见 `P2-acceptance.md`（注销重登一项待补） |
| 3 | 全文搜索（中文分词）+ `--kb-search` 命令 + skill 一键安装到 Claude 和 Codex | ✅ 已验收，见 `P3-acceptance.md`、`P3.1-acceptance.md` |
| **4** | **每日整理成笔记 + 完结标识 + 补跑 + 手动总结按钮** | 见 `P4-digest.md` 和下方"整理规则" |
| 5 | 一键导出 / 导入迁移 | 新设备导入后会话数、笔记数、搜索结果与原设备一致 |
| 6 | 语义搜索（本地向量） | 不用记准关键词也能搜到 |
| 7 | Windows 版 | 见下方"第 7 期约定" |

### 第 4 期整理规则（已和用户确认，先记在这里）

- 统一的整理时间点，由收集用的同一个定时任务顺带检查
- 完结标识两层：**会话**整理完打"已整理"；当轮所有会话都完成，再给**这一轮**打"完结"
- 每次定时扫描都检查上一轮有没有"完结"，没有就补跑，只补没做完的会话
- 以整理时间点为界划分轮次；跨天继续聊的会话，新内容归下一轮
- 30 分钟内还有新消息的会话先跳过，留到下一轮
- 额度不足：1 小时后再试；其他失败最多重试 3 次，然后在 App 里提示
- 同一时间只允许一个整理任务在跑，手动和自动互斥
- 手动操作："整理全部未整理的"按钮，以及在单个会话上"立即整理这个"
- 调用 AI 时：在专用目录里运行，收集时排除这个目录；关掉 AI 的文件读写和命令执行权限；发送前对密码、Token、密钥做脱敏
- 验收：关机一整晚，第二天开机后 1 小时内整理完；中途额度用完，恢复后只补剩下的；自动任务运行中点手动按钮不会重复执行

### 第 7 期约定：Windows 版（已和用户确认）

- **一套代码、一个分支**：继续在 `agent-kb` 上开发，不另开 Windows 分支。只有和系统有关的部分按平台分开写（`#[cfg(target_os = ...)]`）：定时任务（Windows 用任务计划程序）、后台拉起和停止子进程、找 `claude`/`codex` 的路径、移动硬盘识别、安装包。其他代码两边共用
- **在 Windows 电脑上开发**：Windows 部分由在 Windows 电脑上的执行 AI 来写，并且在那台电脑上真跑测试。在 Mac 上写的 Windows 代码不算验证过
- **验收方式**：leader 照旧写任务文件、做验收；Windows 上的复核靠执行 AI 贴出命令和输出，再加用户在 Windows 上做真机确认
- **不能把 Mac 版弄坏**：每期结束时要在 Mac 上再跑一遍第 6 节的全部检查，并重新打包确认 Mac 版能用
- **开始时间**：等第 5、6 期完成、代码稳定后再开始，避免 Mac 和 Windows 两边的执行 AI 同时改同一块代码

## 5. 协作方式

1. 每一期开始前，leader 写好该期任务文件 `P<n>-*.md`
2. 执行 AI 按任务文件执行。遇到任务文件里标了 **【检查点】** 的地方，必须停下来，把产出交给用户转给 leader 审核，通过后才继续
3. 每期完成后，执行 AI 写 `P<n>-report.md`，包括：改了哪些文件、怎么验证的（贴命令和输出）、没做完或偏离任务的地方
4. leader 按验收标准逐项验收，不通过就列出问题打回

## 6. 通用约束

- **Git**
  - `origin` 是用户自己的复刻（`moqi777/claude-code-history-viewer`），`upstream` 是原项目（`jhlee0409/claude-code-history-viewer`）
  - 在分支 `agent-kb` 上开发（从 `main` 拉出），提交可以推送到 `origin`
  - **绝不推送到 `upstream`，不向原项目提 PR、issue 或评论**
  - 同步原项目更新只在用户要求时做：`git fetch upstream` 后合并到 `main`，再合并进 `agent-kb`
  - 每个提交只做一件事，提交前检查必须通过，不用 `--no-verify`
- 新增的界面文字必须走 i18n，5 个语言文件都要加键（`pnpm run i18n:validate` 会校验）：`en`、`zh-CN` 认真写，`ko`、`ja`、`zh-TW` 可以先填英文
- Rust 测试按底座要求用 `cargo test -- --test-threads=1` 运行
- 每期结束时必须通过：`pnpm tsc --build .`、`pnpm vitest run`、`pnpm lint`、`pnpm run i18n:validate`、`cargo test -- --test-threads=1`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check`
- 不引入需要联网的服务，不加任何遥测
- 底座 CLAUDE.md 里的代码质量清单（原子写入、路径校验、错误要给用户反馈等）同样适用；底座的 specs / harness 流程不强制，以 `docs/agent-kb/` 下的文档为准
