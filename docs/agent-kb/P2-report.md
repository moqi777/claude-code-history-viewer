# P2 报告：配置向导、外置盘备份、独立应用身份

分支 `agent-kb`，已推送到 `origin`（`moqi777/claude-code-history-viewer`）。全程未向 upstream 推送，未对原项目提 PR/issue/评论。

提交（由旧到新）：`a881d09`/`c682ae5` 设计（含按评审修订）→ `ef3a04f` P1 遗留 → `5d4b215` 改名/去更新/反馈/字体 → `0066004` 配置 v1 + 备份 + 卷识别 → `3645d92` 命令/向导提交/换目录/定时自修复/设置继承 → `6e04d8a` 查看器不能删改已备份会话 → `c673637` 知识库首页/向导/设置前端 → `b482f3f` plist 带 HOME、`--kb-move-data`、仅桌面端入口。

## 0. 门禁结果（最终提交 `b482f3f` 之上全部重跑）

| 项 | 结果 |
|---|---|
| `pnpm tsc --build .` | 通过 |
| `pnpm vitest run` | 1456 passed |
| `pnpm lint` | 通过 |
| `pnpm run i18n:validate` | 通过（5 语言） |
| `cargo test -- --test-threads=1` | lib 1256 passed / 1 ignored，集成测试全绿 |
| `cargo clippy --all-targets --all-features -- -D warnings`（先 `pnpm build`） | 0 警告 |
| `cargo fmt --all -- --check` | 通过 |

## 1. 步骤 0：P1 遗留（`ef3a04f`）
第 1–4、6–9 项已修；第 7、8 项（首页占位/空会话展示）在 P2 首页中一并处理。

## 2. 评审条目逐项交代

**2.1 备份冲突**：`backup.rs`。检测到目标文件头 4KB 与源不一致（源被重写，如 `.prev-*` 轮转）时，把旧备份改名为 `<name>.conflict-<UTC时间戳>`（不以 `.jsonl` 结尾，不会被当作会话），再整份复制新文件；写 info 日志，不算错误。测试：模拟重写后，盘上同时有新文件、`.conflict-*`、`.prev-*`，再跑一次 `--kb-backup` 无冲突无工作。真机验证：copied 2 / set_aside 1 / errors []；第二次无任何动作。

**2.2 版本**：`package.json` 仍为 1.32.0；`src-tauri/src/kb/mod.rs::AGENTKB_VERSION = "0.2.0"`；后端 `agentkb_version`/`base_version` 命令，首页与关于处显示「AgentKB 0.2.0（基于 Claude Code History Viewer 1.32.0）」。

**2.3 状态区分**：`status.rs` 的 `activity` 为 `idle | collecting | backing_up`，首页分别显示「正在收集」「正在备份到外置盘（X%）」，按钮 tooltip 随状态变化。进度通过 `kb-progress` 事件与 meta 双通道。退出/拔盘后续传：目标文件长度即游标，按 append-only 追加；有单测，也有真机证据（见 §4：把最大备份文件截半后再备份，恰好追加 238,073,014 B，逐字节一致）。

**D1 字体 + CSP**：改为 `@fontsource/ibm-plex-sans`、`@fontsource/jetbrains-mono`（latin 子集，28 个 woff2 打入包），移除 `index.html` 里的 Google Fonts；CSP 仅 `'self'`（`style-src` 另含 `'unsafe-inline'`，`img-src` 含 `data: blob:`，`font-src` 含 `data:`，`connect-src 'self'`）。
**D2 邮件渠道删除**：FeedbackModal 去掉邮件按钮；ErrorBoundary 原 mailto（上游作者邮箱）换成本 fork 的 issues 链接；`send_feedback` 委托给 `branding.rs::ISSUES_URL`。
**D3=A 合成 `kb://` 项目**：`src/kb/openSession.ts`。**SessionCopyMenu 结论**：它只用到 `session.actual_session_id / provider / entrypoint` 与 `project.actual_path / path_status`，合成项目都提供（cwd 未知时 `path_status = "unavailable"`），无需改动该组件。顺带发现并修了两处安全问题：`delete_session` 原本接受任意绝对 `.jsonl` 路径、Codex 的重命名会写 `~/.codex` 的状态库，现在 `kb::paths::is_inside_mirror` 使两者拒绝镜像路径；前端在 `kb://` 视图中隐藏删除/重命名（`6e04d8a`）。
**D4** 向导自动弹出（配置缺失时，含本机）；配置 invalid 时进入设置页并给出明确说明，旧文件改名为 `config.json.corrupt-<ts>`，不覆盖。
**D5** `kb.db` 每天在线备份一次，保留 `kb.db` + `kb.db.prev`，快照 `integrity_check` ok。
**D6** `mainBinaryName` = `AgentKB`，二进制 `AgentKB.app/Contents/MacOS/AgentKB`。
**D7** 第二份备份仅外置盘；内置盘/网络盘/云盘拒绝；按卷 UUID 识别，未挂载时跳过且不在 `/Volumes` 留痕。
**D8 设置迁移**：仅复制 `com.claude.history-viewer/settings.json` → `io.github.moqi777.agentkb/`，只复制一次、不覆盖、原文件不动（`legacy.rs`，在 single-instance 之前执行）。真机验证用隔离 HOME：造 `com.claude.history-viewer/settings.json`（`language: zh-CN`），启动 AgentKB 二进制后新目录出现内容相同的 settings.json，原文件 SHA 不变。**差异说明**：这台机器上 `~/Library/Application Support/` 下既没有 `com.claude.history-viewer` 也没有 `io.github.moqi777.agentkb`，所以评审里「真实 settings.json 有简体中文」的前提在本机无法确认；WebKit 的 localStorage 不迁移（见限制）。
**D9** PLAN 已由 leader 修改，我未动 PLAN.md。
**hardenedRuntime**：未改（保持 true）。分发使用 ad-hoc 签名 `signingIdentity "-"`，得到带封印的签名，标识符 = bundle id，flags `adhoc,runtime`，应用可正常启动，launchd 也能运行。

## 3. 身份与打包
`tauri.conf.json`：productName `AgentKB`，identifier `io.github.moqi777.agentkb`，`bundle.targets ["app"]`，`createUpdaterArtifacts false`。launchd Label `io.github.moqi777.agentkb.sync`。更新器整体移除：`tauri-plugin-updater`（Cargo、lib.rs、4 个 `updater:*` 权限、opener scope 里的 mailto）、`SimpleUpdateManager` 挂载、「检查更新」菜单项；`useUpdater.ts` 保留但不被调用。依赖变化：新增 `plist`、rusqlite `backup` feature、两个 `@fontsource/*`；移除 `tauri-plugin-updater`。

### 「不访问原项目」四层证明
1. 依赖：`cargo tree -i reqwest` → 无输出（没有 HTTP 客户端）。
2. 二进制字符串：`strings` 搜 `jhlee0409`、`api.github.com`、`latest.json`、`fonts.googleapis`、`relee6203`、`feedback@claude-history-viewer` 均 0 命中；唯一的 GitHub 链接是 `github.com/moqi777/claude-code-history-viewer/issues/new`。
3. 前端产物：`dist` 除文档链接外无外部引用；字体全部本地。
4. CSP：`connect-src 'self'`，webview 本身无法发起外部请求。
未能做的第 5 层（运行时 `lsof`/`nettop` 抓包）见限制。

## 4. 真机端到端证据（隔离 HOME，真实 `~/.claude`、`~/.codex` 只读；用 8GB 稀疏 APFS 镜像模拟外置盘，Internal=false，已全部清理）
- 首次 `--kb-sync`：收集 270 文件 / 2,969,033,934 B（约 4 s，整次 10.4 s）；备份 270 文件 6.0 s + `kb.db` 快照（integrity ok，270 会话）；`diff -rq raw backup/raw` 完全一致；之后只追加变化文件（活跃会话 +9,999 B）。
- 定时任务：从 `/Applications` 安装，plist 的 `ProgramArguments[0]=/Applications/AgentKB.app/Contents/MacOS/AgentKB`，StartInterval 600，env 带 HOME/CLAUDE_CONFIG_DIR/CODEX_HOME；launchd RunAtLoad 产生 `sync_runs` 行 `trigger=schedule`、退出码 0（未签名分发的应用在 launchd 下可运行）。
- 自修复（`--kb-schedule ensure`，与 GUI 启动 hook 同一函数）：原位 → not_needed；debug 构建 → `skipped not_in_app` 且不改 plist；应用被移动 → 修复为新路径并记 meta `schedule_repaired_*` 与日志；移动副本被删 → 状态 `program_exists:false`；从 `/Applications` 再 ensure → 修回。
- 外置盘拔出：`--kb-sync` 退出 0，状态 ok，备份 `skipped/not_connected`，无错误，`/Volumes` 前后一致；系统盘上伪造同 UUID 目录 → 跳过且不写入。重新挂载 → 追加补齐；把最大文件（476,146,028 B）截半后 `--kb-backup`，追加 238,073,014 B，逐字节一致（续传成立）。
- 换数据目录 `--kb-move-data`：274 文件 / 2,969,262,582 B 全部 SHA-256 校验，14.0 s；配置原子切换；旧目录前后哈希一致；写 `migration-report.json`；`.agentkb-migrating` 标记清除；新 `kb.db` integrity ok；plist 日志路径随新目录。（唯一不同的 raw 文件是本会话正在写的 transcript，切换后只在新目录增长，旧副本是它的精确前缀。）
- 重写冲突：见 2.1。
- 清理：`launchctl bootout` 完成，`launchctl list` 无 agentkb 任务，隔离 HOME/镜像删除，卷已弹出，真实 `~/AgentKB` 未触碰。

## 5. 对基础文件的改动（非 `src/kb`、`src-tauri/src/kb`）
`AnalyticsView` 增 `'kb'`；`switchToKb`/`isKbView`；`AppLayout`（KbHome 分支、KbRoot 挂载）；`Header`（桌面端知识库按钮、kb 项目的「返回」按钮并隐藏视图标签）；`SettingDropdown`（`onOpenKb`，仅 Tauri）；`projectSlice`（`reloadProjectSessions`、`refreshAllConversations` 对 `kb://` 直接返回）；`useSessionEditing`（kb 视图隐藏删除/重命名）；`delete_session`/`rename_session_native` 拒绝镜像路径；FeedbackModal、ErrorBoundary、`main.tsx`、`index.html`、manifest、common/error 等 locale 的 appName；`lib.rs`、`Cargo.toml`、capabilities、`tauri.conf.json`；`src-tauri/tests/*.test.ts` 与 `updateFlow.integration.test.tsx` 中钉死上游行为的断言改为新行为。WebUI 不提供 `kb_*` 命令（按 PLAN）。

## 6. 限制与未验证项（如实）
1. **没有跑过打包版 GUI。** 你正在运行 `pnpm exec tauri dev`（单实例 socket 被 dev 进程占用），`/Applications/AgentKB.app` 启动后立刻因 single-instance 退出，所以：GUI 启动时的自修复 hook、窗口内真实界面、运行时网络抓包（`lsof`/`nettop`）都没做。CLI 路径（同一函数）已验证。
2. 界面截图只来自无头 Chromium + 模拟 Tauri IPC（首页、备份中、通知、向导、配置损坏、设置，中文，无控制台错误），不是真实窗口；我没有授予任何屏幕录制权限。
3. WebKit localStorage（部分界面偏好）不随标识符迁移；语言等已在 settings.json 的会迁移。
4. 本机无旧版设置目录，D8 只能用隔离 HOME 验证。
5. ko/ja/zh-TW 的新增 kb 文案是英文占位（en、zh-CN 为真翻译）。
6. 未做：全文搜索、skill、AI 摘要、导出/迁移、Windows（P2 范围外）。

## 7. 未签名应用首次打开
应用为 ad-hoc 签名、未公证。首次打开：在 Finder 中右键 `AgentKB.app` → 打开 → 再点「打开」；或先双击一次，到「系统设置 → 隐私与安全性」底部点「仍要打开」。之后可直接启动。

---

## 8. 需要你在真机上做的检查（验收 9–11）

先决条件：**停掉你的 `pnpm exec tauri dev`**（否则打包版会因单实例立刻退出）。`/Applications/AgentKB.app` 已安装（构建 4，ad-hoc 签名），按 §7 首次打开。

**A. 新用户向导（验收 9）**
1. 退出 AgentKB。终端执行：`mv ~/.claude-history-viewer/config.json ~/.claude-history-viewer/config.json.bak`（只改名，不删；数据目录 `~/AgentKB` 不会被动）。
2. 打开 AgentKB。预期：自动弹出 5 步向导；语言为简体中文（你若有旧版设置，见 C）。
3. 每步点各「测试」按钮：来源检测应显示 Claude/Codex 会话数与大小；数据目录应显示可写、剩余空间，并识别到已有数据（268 会话左右）；定时任务环境应显示通过；外置盘（若插着盘）应列出该盘并测试通过，盘不可用则给出明确原因。
4. 计时：全程应 ≤5 分钟；任何一步失败都要有一句明确的话说明原因和怎么办。最后一步才写配置。
5. 还原：退出后 `mv ~/.claude-history-viewer/config.json.bak ~/.claude-history-viewer/config.json`（若向导已写出新 config，先把新文件改名保存再还原）。

**B. 登录后定时任务（验收 10）**
1. 在设置里启用定时（向导最后一步会安装）。
2. 注销再登录（或重启）。
3. 执行：`sqlite3 ~/AgentKB/kb.db "select started_at,trigger,ok from sync_runs order by id desc limit 3"`。预期：出现登录后的一行，`trigger = schedule`。

**C. 打开 Codex 会话（验收 11）**
1. 在 AgentKB 首页列表中点一条 Codex 来源的会话。
2. 预期：会话视图顶部显示来源 Codex 与项目（cwd）；滚动到中段后，来源/项目信息仍然正确，无「未知」；点左上「返回」回到首页。
3. 同时确认：该视图里没有删除/重命名入口。

**D. 语言迁移（D8）**
本机检查到 `~/Library/Application Support/` 下没有旧版 `com.claude.history-viewer` 目录。若你之前用过旧版并有简体中文设置，请 `ls ~/Library/Application\ Support/com.claude.history-viewer/` 看是否存在 settings.json；存在则首次打开 AgentKB 应为简体中文，且 `~/Library/Application Support/io.github.moqi777.agentkb/settings.json` 出现一份拷贝（原文件不动）。不存在则默认语言跟随系统。

**E. 外置盘（可选）**：首页会显示「备份到外置盘」状态；插入你选的盘后点「立即备份」，应显示百分比进度，完成后显示时间；备份中拔盘，再插上，再点一次应继续而不是从头来。
