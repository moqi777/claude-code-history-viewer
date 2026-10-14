# P2 设计说明：改名打包 + 配置 + 向导 + 知识库主页 + 移动硬盘备份

> 状态：**leader 有条件通过（`P2-design-review.md`），已按第 12 节修订，直接进入实现**。
> 步骤 0（P1 遗留）已先行完成，见第 0 节；其中第 7、8 项本质上是"主页"的设计，留到本文第 5 节一起审。
> 依据：我读过的文件和函数都标在各条后面；实测过的东西贴了命令输出。分支 `agent-kb`，基于 P1 验收后的 `e913b99` + 步骤 0 提交 `ef3a04f`。

## 0. 步骤 0 已完成（P1 遗留，提交 `ef3a04f`）

| # | 问题 | 处理 | 验证 |
|---|---|---|---|
| 1 | Codex 子代理会话没标题 | `session_meta.payload` 的 `agent_nickname` + `agent_path` 末段 → `Ohm · review_final_vip_fresh`（顶层字段优先，其次 `source.subagent.thread_spawn`）；有真实首条提示就用提示。`title_source=agent_meta` | 测试 `subagent_without_a_prompt_is_titled_…`；真实数据：4 个都有标题，例如 `Ohm · review_final_vip_fresh`、`Bernoulli · transaction_review` |
| 2 | Claude 子代理 104 条消息却无标题 | **原因**：该文件 38 条 user 记录**全是 `tool_result`**（一个被恢复的子代理，文件里根本没有任务提示），兜底规则无从取起。改为按 agent id 命名 `agent a8b3c729` | 测试 `subagent_without_any_user_text_is_titled_by_its_agent_id`；真实数据 `claude|agent a8b3c729` |
| 3 | 9 个空会话 | 后端 `list_sessions` 默认过滤 `message_count=0`，`include_empty` 才显示；界面加"包含空会话"开关 | 测试 `empty_sessions_are_hidden_from_the_list_unless_asked_for`；真实数据 8 个 Claude + 1 个 Codex 空会话（leader 当时数的是 9）|
| 4 | 新未知类型 | 9 种全部抽样确认是纯元数据（无 `message`），加入已知列表：Claude `bridge-session / atis-latch / cost-state / file-history-delta / artifact-autoreact-ledger / artifact-comment-monitor / frame-link`，Codex `inter_agent_communication_metadata / token_usage_record` | 测试 `new_metadata_record_types_are_known_…`；真实数据全量同步 `unknown_types: {}` |
| 6 | `[Image #1]` 占位符 | `fallback_title` 去掉 `[Image …]`、`[Pasted text …]`、`[Pasted image …]`，去完为空则取下一条消息。Codex 原生标题（`threads.title`）也套同一规则（去完为空则保持原文）| 测试 `fallback_title_drops_attachment_placeholders`、`native_titles_are_cleaned_but_never_emptied` |
| 9 | 消息数口径 | 不去改底座的查看器口径（它把工具调用也算进去）。知识库这一项改叫"**对话消息**"，悬停说明"只统计用户和助手的对话消息，不含工具调用" | i18n 5 种语言 + 前端测试 |
| — | 已入库的旧数据怎么受益 | 新增 `INDEXER_VERSION`：版本变了，下次同步**从镜像重建**所有会话行（镜像是唯一可信来源），失败会保持"待重建"状态下次重试。这也是以后改索引规则的通用机制 | 测试 `outdated_indexer_version_rebuilds_rows_from_the_mirror_once`、`a_failed_rebuild_is_retried_on_the_next_sync` |
| 7、8 | 弹窗→独立页面；打开会话后上下文不对 | **本文第 5 节设计，等你审**。我没有先动 | — |

检查：tsc / vitest（1408）/ lint / i18n / clippy / fmt 全绿；`cargo test --lib kb::` 84 个通过。

---

## 1. 改标识会影响什么

### 1.1 逐处清单

依据：`src-tauri/tauri.conf.json`、`src-tauri/src/lib.rs::run_tauri`、`tauri-plugin-store-2.4.3/src/store.rs::resolve_store_path`、`src/services/storage.ts`、`src/utils/…`。

| 依赖 identifier / productName 的地方 | 现状 | 处理 |
|---|---|---|
| **tauri-plugin-store 的存储位置** | `resolve_store_path` 用 `BaseDirectory::AppData` → `~/Library/Application Support/<identifier>/`。前端 `storageAdapter.load("settings.json")`（`useLanguageStore`、`settingsSlice`、`contexts/theme/utils`）存主题、语言、侧栏等 | **会丢**：新 identifier 是新目录。启动最早期（`setup` 之前）做一次性迁移：若新目录里没有 `settings.json`、旧目录 `com.claude.history-viewer/settings.json` 存在，则**复制**（不是移动，原版 App 继续可用）。迁移失败只记日志，不阻止启动 |
| **WebKit 存储**（localStorage / IndexedDB）| `~/Library/WebKit/<identifier>/`。例如过滤器、折叠状态等 | 会丢，**不迁移**（无法可靠复制 WebKit 数据库，且都是可重建的界面偏好）。写进报告 |
| **single-instance 插件** | socket/mutex 名由 identifier 派生（`lib.rs` 第 155 行起注册）| 好处：新标识让 AgentKB 和原版可以**同时运行**，互不拦截。无需处理 |
| **窗口标题** | `tauri.conf.json` 里 `"title": ""`，`titleBarStyle: Overlay`，标题由前端 Header 画 | Header 的 `t('common.appName')`、`<img alt="Claude Code History">`（`Header.tsx` 152/164/142 行）随 i18n 变；`index.html` 的 `<title>`、`public/manifest.json` 的 `name` 改成 AgentKB |
| **i18n `common.appName`** | `"Claude Code History Viewer"`，5 个 locale；用于 Header、`WebUILogin` | 5 个 locale 全改 `AgentKB`；`ErrorBoundary.tsx` 里 3 处硬编码文案同改 |
| **应用日志位置** | `tauri-plugin-log` 在 `Cargo.toml` 里但 `lib.rs` 没注册（grep 无 `tauri_plugin_log`）| 无影响。KB 自己的日志在 `<数据目录>/logs/`，与 identifier 无关 |
| **Caches / Preferences / Saved Application State** | 系统按 identifier 建 `~/Library/Caches/<id>`、`Preferences/<id>.plist` | 新 ID 新目录，无需处理 |
| **`~/.claude-history-viewer/`**（`lib.rs` 924/1011 行、`mcp_presets.rs`、`unified_presets.rs`、`claude_settings.rs` 731 行）| 硬编码路径，与 identifier 无关 | **不改**（leader 已定）。注意：原版和 AgentKB 会共用 `user-data.json`、`archives/`，同写时和原来"一个 App 开两份"的风险一样，不新增 |
| **应用包名与可执行文件名** | Cargo 包名 `claude-code-history-viewer` → 可执行文件同名；`productName` 控制 `.app` 名 | `.app` = `AgentKB.app`。可执行文件名：加 `mainBinaryName: "AgentKB"`，使 macOS"登录项与扩展 / 后台活动"通知里显示 AgentKB 而不是 `claude-code-history-viewer`（P1 里这个通知已经出现过）。**Cargo 包名、`package.json` name 保持不变**，减少合并上游时的冲突。具体产物路径以步骤 2 实际 `tauri build` 为准，我会在报告里贴 `ls` |
| **launchd Label / plist** | 已是 `io.github.moqi777.agentkb.sync`（`schedule::LAUNCHD_LABEL`）| 不变。只有 `ProgramArguments[0]` 变，见第 8 节 |
| **自定义 URL scheme** | `cli.rs` 里解析 `claude-code-history-viewer://…`，但 `tauri.conf.json` 没有 deep-link 插件、没有 `CFBundleURLSchemes`，**从未真正注册** | 不改 |
| **单元测试里的路径字符串** | `codex.rs` 等测试里的 `claude-code-history-viewer` 只是示例项目名 | 不改 |
| **`bundle.createUpdaterArtifacts: true`** | 构建时要求签名私钥，没有会报错 | 改 `false`（见第 2 节）|
| **`bundle.targets: "all"`** | 会生成 dmg，dmg 打包依赖 Finder AppleScript，无界面环境可能卡住 | 改 `["app"]`；`createUpdaterArtifacts` 同步关闭。分发 = 拷贝 `.app` |

### 1.2 P1 数据原样保留的保证

| 数据 | 位置 | 为什么不受影响 |
|---|---|---|
| `~/AgentKB/`（`raw/`、`kb.db`、`logs/`）| 用户主目录下的固定默认路径 | 路径来自 `config.json` 的 `data_dir` 或默认值，**与 identifier、productName 无关** |
| `config.json`（目前不存在）| `crate::utils::config_dir()/agent-kb/config.json` = `~/Library/Application Support/agent-kb/` | **目录名是字面量 `agent-kb`**，不是 identifier（`kb/config.rs::config_path`）。新 identifier 的 `…/io.github.moqi777.agentkb/` 是另一个目录，互不相干 |
| launchd 任务 | `~/Library/LaunchAgents/io.github.moqi777.agentkb.sync.plist` | Label 不变 |

**验证方式**（步骤 2 做、报告里贴输出）：改名前后对 `~/AgentKB` 做 `find … -exec stat` 快照对比，只允许同步本身产生的变化；P2 所有开发期测试一律用 `HOME=$(mktemp -d)` 隔离，不碰真实目录。

---

## 2. 关闭自动更新、改反馈入口、证明不访问原项目

### 2.1 实际会联网的东西（我 grep 全了）

| 出口 | 依据 | 对原项目 |
|---|---|---|
| `tauri-plugin-updater` → `https://github.com/jhlee0409/claude-code-history-viewer/releases/latest/download/latest.json` | `tauri.conf.json` `plugins.updater`；`lib.rs` 注册；前端 `useUpdater.ts`（动态 `import('@tauri-apps/plugin-updater')` → `check()`）、`SimpleUpdateManager.tsx`（`AppLayout.tsx` 927 行挂载，监听 `manual-update-check`）| ✅ **是**，且签名公钥是原作者的 |
| 反馈 → 打开浏览器到 `…/jhlee0409/…/issues/new` | `commands/feedback.rs::open_github_issues` 第 74 行；`server/handlers.rs::open_github_issues` 第 979 行 | ✅ 是 |
| 反馈 → `mailto:feedback@claude-history-viewer.app` | `commands/feedback.rs::send_feedback` 第 50 行（环境变量 `FEEDBACK_EMAIL` 缺省值）；`FeedbackModal.tsx` 的"邮件"按钮 | ✅ 是（原项目的邮箱域名）|
| CSP `connect-src` 白名单 `api.github.com / github.com / *.githubusercontent.com` | `tauri.conf.json` `app.security.csp` | 只是**允许**，前端当前没有对应请求 |
| **Google Fonts** | `index.html` 第 17–22 行 `<link href="https://fonts.googleapis.com/css2?…">`，CSP 里 `style-src`/`font-src`/`connect-src` 放行 | 不是原项目，但**确实是启动时的外网请求**（IBM Plex Sans + JetBrains Mono），见决策 D1 |
| Rust 侧 HTTP 客户端 | `cargo tree -i reqwest` → **只**来自 `tauri-plugin-updater v2.10.1` | — |

### 2.2 怎么改

1. **Rust**：`Cargo.toml` 删 `tauri-plugin-updater`；`lib.rs` 删 `.plugin(tauri_plugin_updater::Builder::new().build())`；`capabilities/default.json` 删 4 条 `updater:*`；`tauri.conf.json` 删整个 `plugins.updater`，`createUpdaterArtifacts: false`。
2. **前端**：`AppLayout.tsx` 删 `<SimpleUpdateManager />` 那一处挂载（约 3 行）；`SettingDropdown/index.tsx` 删"检查更新"菜单项（`manual-update-check` 派发处，约 20 行）。`useUpdater.ts` 只剩读版本号的用途——**保留文件不动**（减少合并冲突），因为删掉挂载后没人再调用 `checkForUpdates`；`@tauri-apps/plugin-updater` npm 依赖也保留（不会被执行到），并用测试钉住"不会被调用"。
3. **反馈**：新增一个常量 `ISSUES_URL = "https://github.com/moqi777/claude-code-history-viewer/issues/new"`（放 `src-tauri/src/branding.rs`，两处共用）；`feedback.rs` 和 `server/handlers.rs` 改引用它。**邮件通道**我建议整条删除（决策 D2）：原默认地址属于原项目，我不能编一个新地址。
4. **CSP**：删掉 github 相关域名。字体见 D1：若选"内置字体"，CSP 里所有外部域名都能删，变成 `default-src 'self'`，连"允许"的出口都没有。

### 2.3 怎么证明运行时不会再访问原项目（四层，每层都在报告里贴输出）

1. **编译层**：`cargo tree -i reqwest` 应报 *did not match any packages*（目前只有 updater 引入它）；`cargo tree | grep -i updater` 无输出。**二进制里没有 HTTP 客户端**。
2. **源码层**：`grep -rniE "jhlee0409|api\.github\.com|latest\.json|feedback@claude-history-viewer" src src-tauri/src src-tauri/tauri.conf.json src-tauri/capabilities index.html public` → 除注释/测试外应为空；单测断言 `ISSUES_URL` 以 `https://github.com/moqi777/` 开头。
3. **前端测试**：渲染设置下拉菜单，断言没有"检查更新"项；渲染 `AppLayout`，断言没有 `manual-update-check` 监听；mock `@tauri-apps/plugin-updater` 并断言 `check` 从未被调用。
4. **运行时抓包**：启动打包后的 `AgentKB.app` 并操作 2 分钟（打开主页、收集、设置、反馈弹窗），期间每 2 秒采样 `lsof -nP -i -a -p <pid>`（和 `nettop -P -L 1 -p <pid>`）并汇总所有对端地址。预期：除 WebKit 的 `com.apple.WebKit.Networking` 子进程外无出站；若选 D1-B（保留 Google Fonts）则只出现 `fonts.googleapis.com` / `fonts.gstatic.com`，**不得出现 github.com**。无 sudo 抓不了 tcpdump，所以用 lsof 采样；我会在报告里写明这个方法的局限（极短连接可能漏采，所以第 1 层的"二进制里没有 HTTP 客户端"才是主证据）。

---

## 3. 配置结构

### 3.1 单一读取入口

界面和无界面**走同一个 Rust 加载器** `kb::config::KbConfig::load()`（P1 已是这样：`cli.rs::run_sync_command` 和 `commands.rs::env()` 都调它）。前端**不缓存也不自己解析** `config.json`：只通过 `kb_get_config` / `kb_save_config` 命令读写；设置页每次进入都重新读。这样"界面改了，下一次 launchd 的 `--kb-sync` 一定读到"。

### 3.2 `config.json` v1 完整字段

位置：`crate::utils::config_dir()/agent-kb/config.json`（P1 已定，不变）。

```jsonc
{
  "version": 1,                         // 整数。缺省 = 0（P1 写出的旧文件）
  "data_dir": "~/AgentKB",              // 绝对路径或 ~/ 开头；加载时展开。不能是空串
  "interval_secs": 600,                 // 60..=86400。0 或越界 → 校验失败（不再静默用默认值）
  "schedule_enabled": true,             // 向导/设置页的"启用定时收集"。false 时 App 不会自动修复定时任务
  "sources": {
    "claude": { "enabled": true, "root": null },   // root=null → 自动检测（CLAUDE_CONFIG_DIR 优先，其次 ~/.claude）
    "codex":  { "enabled": true, "root": null }    // root=null → 自动检测（CODEX_HOME 优先，其次 ~/.codex）
  },
  "exclude_prefixes": [],               // 字符串数组；以这些前缀开头的源路径不收集；每项必须是绝对路径
  "mirror": {                           // 移动硬盘备份
    "enabled": false,
    "target_dir": null,                 // 例如 "/Volumes/Backup1/AgentKB-backup"
    "volume_uuid": null,                // 设置时记录的卷 UUID（识别"哪块盘"的唯一依据）
    "volume_name": null                 // 仅用于界面显示，不参与判断
  }
}
```

- **不放进配置的东西**：上次收集/备份时间、备份结果、定时任务修复记录——这些是**运行状态**，放 `kb.db` 的 `meta` 表（键 `backup_last_at`、`backup_last_result`、`schedule_repaired_*` 等），不用改表结构、不需要迁移。
- **取值校验**（`KbConfig::validate()`，保存和加载都过）：`interval_secs` 范围；`data_dir` 绝对、不位于任何来源根之内、也不包含它们；`exclude_prefixes` 绝对路径；`mirror.enabled` 为真时 `target_dir` 和 `volume_uuid` 必填，且 `target_dir` 不能位于 `data_dir` 之内。
- **未知字段保留**：结构体带 `#[serde(flatten)] extra`，读到未知键保存时原样写回，避免新旧版本互相抹掉对方的字段。

### 3.3 版本与升级

- 加载流程：读文本 → 解析成 `serde_json::Value` → 按 `version` 依次套 `migrate_v0_to_v1` 等函数（纯函数，`Value → Value`）→ 反序列化 → `validate()`。
- **v0 → v1**（P1 的旧文件，字段平铺）：`claude_root` → `sources.claude.root`，`codex_root` → `sources.codex.root`，补 `version: 1` 与其余默认值。
- **读时只在内存里升级，不回写**：`--kb-sync` 是 launchd 反复启动的进程，读时写盘会有并发风险。升级结果在下一次**显式保存**（设置页、向导）时落盘。
- **文件版本比程序新**：拒绝加载并提示"配置来自更新版本的 AgentKB，请升级 App"，**不覆盖**。
- **原子写**：`write_atomic`（临时文件 + `fsync` + `rename`，同目录），P1 的 `schedule.rs` 已有同样的模式，抽成 `kb::fsutil::write_atomic` 共用。
- **文件损坏**（不是合法 JSON，或校验失败）：
  - 无界面 `--kb-sync`：退出码 1，stderr 写明原因和文件路径，**不回退默认值**（回退会把备份写到意外位置，P1 已是这个行为）。
  - 界面：`kb_get_config_state` 返回 `{ state: "invalid", message, path }`，主页顶部红色横幅"配置文件损坏：…"，按钮**"恢复默认值"**：把损坏文件**改名**为 `config.json.corrupt-<时间戳>`（不删），然后回到"没有 config.json"状态（即重走向导）。绝不崩溃。

### 3.4 设置页的改动副作用

| 改了什么 | 自动做什么 |
|---|---|
| `interval_secs` | 保存成功后立即重装定时任务（`schedule::install_now`）；失败则配置已保存但提示"定时任务未更新，点这里重试" |
| `schedule_enabled` | 开→安装；关→卸载 |
| `sources.*.enabled` / `root` | 下一次收集生效；**被关掉的来源既不扫描也不做"已删除"判定**（`SourceRoots` 里置 `None`，`discover` 已有的"根不完整就不判删"逻辑天然成立）；已收集的会话保留，列表里仍可浏览 |
| `exclude_prefixes` | 同 P1：新增排除不会把已有会话标成已删除（`reconcile` 已处理）|
| `mirror.*` | 保存时必须通过"移动硬盘测试"才允许启用 |
| `data_dir` | **不是普通保存**，走第 7 节的迁移流程 |

---

## 4. 向导

### 4.1 入口与状态

- `App` 启动时调 `kb_get_config_state`，得到 `missing | valid | invalid | newer`。`missing` → 自动切到 AgentKB 页面并打开向导；设置页里有"重新运行向导"。
- 向导的所有选择只存在**前端内存里的草稿**；**只有走到最后一步并成功完成首次收集后才写 `config.json`**。所以"中途关 App"自然就是"什么都没写"，下次从头来；也不会出现一半配置。
- 向导对话框可点"稍后"关闭（不写任何东西）；下次启动仍会弹。后台的 launchd 任务如果已存在（比如这台机器），它继续按默认值运行，不受影响。

### 4.2 每一步

| 步 | 显示 | 测试按钮 | 能否跳过 | 后端命令 |
|---|---|---|---|---|
| 1 欢迎 + 检测 | 找到的 Claude / Codex 目录、会话数、占用空间；每个来源一个开关（默认都开）| "重新检测" | 否（至少选一个来源才能继续；只检测到一个也行）| `kb_probe_sources` |
| 2 数据目录 | 默认 `~/AgentKB`，可"选择…"（目录选择器）；显示"预计需要 ≈X（来源总大小）/ 剩余 Y"；目录里已有 `kb.db` → 绿色提示**"检测到已有数据（N 个会话，Z GB），将继续使用"** | "测试目录" → "可写入，剩余 120 GB，现有数据 3.1 GB" | 否（有默认值，直接下一步即可）| `kb_test_data_dir` |
| 3 收集频率 | 5 / 10 / 15 分钟 / 自定义（≥1 分钟）| "测试定时任务环境" → 检查 `launchctl` 可用、`~/Library/LaunchAgents` 可写、当前 App 路径稳定（见 4.4）| 否（默认 10 分钟）| `kb_test_schedule_env` |
| 4 移动硬盘 | 列出当前已连接的外接卷（名称、UUID 前 8 位、剩余空间），或"选择文件夹…"；文件夹默认 `AgentKB-backup`| "测试" → "已连接，可写入" / "未连接" / "这是系统内置盘，不支持" / "网络盘/云盘不支持" | **可以跳过**（默认关）| `kb_list_external_volumes`、`kb_test_mirror` |
| 5 确认 | 全部选择的汇总。点"开始" → ①首次收集（进度条：已处理文件数/总数、已复制字节）→ ②写 `config.json` → ③安装定时任务 → ④结果页 | — | 否 | `kb_wizard_commit`（见下）|

每一步都有"上一步"。结果页：成功 → "进入知识库"；某一阶段失败 → 红色提示 + **"重试此步骤"**，已完成的阶段不重做（收集是增量的，重跑安全）。

### 4.3 `kb_wizard_commit` 的顺序与失败语义

1. 用**草稿**构造 `KbEnv`，在进程内跑 `run_sync`（需要加进度回调，见 9.1）。失败 → 返回错误，**不写配置**。
2. 收集成功 → `write_atomic(config.json)`。
3. 若 `schedule_enabled` → `install_now`；失败 → 配置已落盘（有效），结果页提示"定时任务安装失败：…"和重试按钮（这是可恢复的，不回滚配置）。
4. 若 `mirror.enabled` → 立即做一次首次备份（可能很久：2.7 GB 拷到 U 盘），放在第 5 步**之后异步进行**，向导结果页提示"备份在后台进行"，进度显示在主页。

### 4.4 已有 `~/AgentKB` 但没有 `config.json`（这台机器现在就是这样）

- `kb_get_config_state` 返回 `missing`，同时 `kb_test_data_dir("~/AgentKB")` 的结果里有 `existing: { sessions: 268, bytes: 2.9e9, has_db: true }`。
- 向导第 2 步显示"检测到已有数据，将继续使用"，**不提供也不执行任何清空/覆盖**；第 5 步的"首次收集"实际是增量同步（几百毫秒～几秒）。
- 若检测到 `kb.db` 的 `user_version` 比程序新：第 2 步红字拒绝并说明，不能继续（P1 `db::open` 已拒绝）。
- 若 `~/AgentKB` 里只有 `raw/` 没有 `kb.db`（库丢了）：提示"检测到备份文件但没有索引，将重建索引"，收集时会照常重建（P1 的"新文件但镜像已存在"逻辑 + `INDEXER_VERSION` 重建）。
- 若该目录已有 launchd 任务（这台机器可能）：第 5 步安装时 `install` 是幂等的，会替换成新路径。

### 4.5 稳定路径检查（会影响 3 步的测试和后面的自动修复）

App 运行时若可执行文件路径满足下列任一条，定时任务**不应**指向它：

- 不在 `….app/Contents/MacOS/` 里（`cargo run`、`target/debug/…`、`tauri dev`）→ 开发期，**不自动改 plist**，避免开发者一次 `pnpm tauri dev` 把正式任务劫持到 debug 路径；
- 路径含 `/AppTranslocation/`（macOS 对"从下载文件夹直接打开的未签名 App"做的随机只读副本，路径是临时的）→ 提示"请先把 AgentKB 拖到 /Applications 再启用定时任务"；
- 路径在 `/Volumes/…` 下的磁盘镜像里 → 同上提示。

第 3 步的测试按钮对这三种情况分别给出明确文案。

---

## 5. 知识库主页（含 P1 遗留第 7、8 项）

### 5.1 视图接入：新增 `AnalyticsView = "kb"`

依据：`src/types/analytics.ts`（`AnalyticsView` 联合类型）、`hooks/analytics/useAnalyticsNavigation.ts`（`switchToArchive` 等）、`useAnalyticsComputed.ts`、`layouts/AppLayout.tsx`（主区按 `computed.isXxxView` 分支，第 ~760–870 行；`isTranscriptView` 判定第 ~320 行；非消息视图的内容头第 ~600 行）、`layouts/Header/Header.tsx`。

P1 我为了不动底座选了弹窗；P2 leader 要独立页面，所以**接受**对这几个文件的小改动（约 35 行，列在第 9 节）：

- 类型加 `"kb"`；`switchToKb`（和 `switchToArchive` 并列）；`computed.isKbView`；
- `AppLayout`：`isTranscriptView` 排除 kb；非消息视图内容头里 kb 视图**不画**（主页自己有页头）；主区加一个分支 `computed.isKbView ? <KbHome/>`；
- `Header`：右侧工具栏加一个 AgentKB 图标按钮（`aria-label="AgentKB"`），同时保留设置菜单里的"知识库"项，两处都进入同一个视图；
- 移动端 `BottomTabBar` **不加**（知识库本身是桌面功能，`DesktopOnly`）。

### 5.2 页面布局草图

```
┌ Header：[AgentKB]  / 当前项目   …………………   [搜索] [知识库●] [⚙]  ┐
├──────────┬────────────────────────────────────────────────────────────┤
│ 左侧栏   │  AgentKB 知识库                  [立即收集] [立即备份] [设置]│
│ (原项目  │ ┌ 状态 ─────────────────────────────────────────────────┐  │
│  树，保留)│ │ ● 上次收集  2 分钟前 · 成功 · 3 个文件有变化             │  │
│          │ │ ● 定时任务  已启用，每 10 分钟     ［已失效→修复］        │  │
│          │ │ ● 会话      Claude 40 · Codex 228   源文件已删除 2       │  │
│          │ │ ● 移动硬盘  Backup1 · 上次备份 3 小时前   ［7 天未备份⚠］ │  │
│          │ │ ● 数据目录  ~/AgentKB · 占用 2.9 GB                      │  │
│          │ │ ⓘ 定时任务已更新到新位置（/Applications/AgentKB.app） ✕  │  │
│          │ └────────────────────────────────────────────────────────┘  │
│          │ 筛选：[来源 ▾][客户端 ▾][项目 ▾][时间 ▾] ☐只看源文件已删除    │
│          │       ☐包含子代理 ☐包含空会话                    搜标题…     │
│          │ ┌ 会话列表（分页，每页 100，滚动加载）──────────────────┐   │
│          │ │ Fix login bug          Claude·cli  proj   18 条  今天  │   │
│          │ │ Ohm · review_…  [子代理] Codex·exec …                  │   │
│          │ │ …原文件已不存在       [源文件已删除]                    │   │
│          │ └────────────────────────────────────────────────────────┘  │
└──────────┴────────────────────────────────────────────────────────────┘
```

- **状态区**：数据来自 `kb_get_status`（P1 已有，P2 扩充：备份状态、占用空间、定时任务指向的可执行文件是否存在、修复通知）。长耗时操作（收集/备份/迁移）用"进行中 + 进度条"，进度来自 Tauri 事件 `kb-progress`（第 9.1 节）。所有错误都转成 toast 或横幅里的人话，不只写 console。
- **筛选**：来源（Claude/Codex）、客户端（cli/vscode/desktop/exec/subagent…，选项从库里 `DISTINCT client` 取）、项目（`DISTINCT project_name`）、时间范围（最近 7 天/30 天/自定义，按 `last_activity_at`）、"只看源文件已删除"、"包含子代理"、"包含空会话"、标题子串。全部在后端 SQL 过滤（`status::list_sessions` 扩参数），**不做全文搜索**（第 3 期）。
- **定时任务失效**：`kb_get_status` 读 plist 的 `ProgramArguments[0]`（`plist` crate 解析）与 `launchctl print` 的 `program`，可执行文件不存在 → 状态区显示红色"定时任务已失效"+ **修复**按钮（调 `kb_schedule_install`）。

### 5.3 P1 遗留第 8 项：从列表打开会话后，上下文必须是对的

**问题根因**（读代码得出）：查看器的"中间栏"和顶部标题都来自 `store.selectedProject` / `store.sessions`（`Header.tsx` 第 ~145 行显示 `getProjectDisplayName(selectedProject)`；`SessionsPane` 显示 `sessions`）。P1 只调了 `selectSession`，`selectedProject` 仍是之前选的 JavaCode/Claude，于是显示错乱。

**方案（推荐，决策 D3）：打开时构造一个合成项目，复用整套三栏布局**

1. 前端在打开会话时：
   - 调 `kb_list_sessions`，按 `(source, cwd)` 取出同一项目下的全部备份会话（含"源文件已删除"的）；
   - 构造合成 `ClaudeProject`：`provider = 来源`，`name = project_name`，`actual_path = cwd`，`path = "kb://<source>/<cwd>"`，`session_count`/`last_modified` 取自列表；
   - 直接 `useAppStore.setState({ selectedProject, sessions, sessionsTotal, hasMoreSessions:false })`，再 `setAnalyticsCurrentView("messages")` + `selectSession(session)`。
   - 结果：中间栏显示**该项目的备份会话**（带"已删除"标记），顶部显示 "AgentKB / <项目名>" 和该会话标题，Codex 会话显示 Codex 来源（`provider` 驱动图标与文案）。
2. **必须加的底座守卫**（约 6 行，`projectSlice.ts::reloadProjectSessions` 开头）：`if (project.path.startsWith("kb://")) return;`。原因：文件监听（`watcher.rs` 触发的 `reloadProjectSessions`）会用 `project.path` 去 `load_provider_sessions_page`；`kb://…` 不是真实路径，不加守卫会把列表刷成空或报错（我读过 `selectProject`/`reloadProjectSessions` 的实现，第 681–750 行）。
3. Header 里当 `selectedProject.path` 以 `kb://` 开头时，加一个"← 知识库"按钮（约 8 行）回到 `kb` 视图；`SessionCopyMenu`（复制 resume 命令）对合成项目要用 `cwd`，我会读 `SessionCopyMenu` 确认它依赖哪些字段后再定，若依赖真实路径就在 kb 上下文里隐藏该菜单——写进报告。
4. 分页：该项目的备份会话一次取 500 条，足够；超过的项目极少，超出时 `hasMoreSessions=false` 并在列表底部提示"仅显示最近 500 条"（后续期再分页）。

**备选（D3-B）**：在 kb 视图内自己渲染 `MessageViewer`，不借用 `selectedProject`。优点是不碰底座 slice；缺点是要把 `AppLayout` 里传给 `MessageViewer` 的 ~10 个 props（搜索、导航器…）在 KB 页再接一遍，重复且易与底座漂移。我推荐 A（合成项目），改动更小且行为与原查看器完全一致。

### 5.4 P1 遗留第 7 项：弹窗 → 页面

`KbPanel.tsx`（P1 的对话框）整体替换为 `KbHome.tsx`，`useKbStore` 保留并扩充（P1 的 15 个前端测试迁移到新组件）。设置菜单里的"知识库"项改为 `switchToKb()`。

---

## 6. 移动硬盘备份

### 6.1 识别"哪块盘"：用卷 UUID，不信盘名

我在这台机器上用 `hdiutil` 挂了个 20 MB 的 APFS 镜像做了实测：

```
$ diskutil info -plist /Volumes/KBProbe | plutil -extract VolumeUUID raw -     → D62F7387-D951-4AF4-9536-94103B9E94E9
                                        Internal → false ; RemovableMediaOrExternalDevice → true
$ diskutil info -plist /Volumes/KBProbe/AgentKB-backup                          → 报错：无 VolumeUUID（必须传挂载点本身）
$ df -P /Volumes/KBProbe/AgentKB-backup   → /dev/disk9s1 … /Volumes/KBProbe      （df 对任意路径返回它所在文件系统的挂载点）
$ df -P /Users/mo7/Dev                    → /dev/disk3s5 … /System/Volumes/Data   （普通系统盘文件夹）
```

探测算法 `kb::volume::probe(path) -> Probe`：

1. 取 `path` 本身或**最近的已存在祖先**；**不创建任何目录**。
2. `df -P <该路径>` 取挂载点（最后一栏，按列解析以支持含空格的挂载点）。
3. `diskutil info -plist <挂载点>`（用 `plist` crate 解析）→ `VolumeUUID`、`Internal`、`RemovableMediaOrExternalDevice`、`VolumeName`、`FilesystemType`。
4. 返回 `Connected{ uuid, mount_point, external, name }`，或 `NotConnected`（路径的挂载点是系统盘/任何 UUID 与记录不符的卷）。

**"盘没插但 `/Volumes/<名>` 存在"的情况**：此时 `df` 返回的是系统盘挂载点 `/System/Volumes/Data`，UUID（`6550A3DF-…`）与设置时记录的外接盘 UUID **不一致** → 判定 `NotConnected`，**一个字节都不写**。同一逻辑也挡住"另一块同名盘"。

**写入时的二次保险**：每个批次写之前比较目标目录与挂载点的 `st_dev`（`MetadataExt::dev`），盘在拷贝过程中被拔掉时不会把后续文件写进系统盘上的空壳目录；而且我们**只在已确认的挂载点之下** `create_dir_all`，永远不为 `/Volumes/<名>` 这一级创建目录（普通用户本来也建不了，这里是显式保证）。

**接受哪些盘**：`Internal == false`（外接/可移除）。系统内置盘、网络盘（`FilesystemType` 为 `smbfs/nfs/afpfs/webdav`，或路径在 `~/Library/CloudStorage`、`iCloud Drive` 下）一律拒绝并给出明确文案。`hdiutil` 挂载的测试镜像 `Internal=false`，所以验收可以用它模拟。

### 6.2 备份内容与算法（`kb::backup`）

每次 `--kb-sync` 主流程**结束且成功释放收集后**（同一把锁内）：

1. `probe(target_dir)`：`NotConnected` → 记一条日志 `backup skipped: volume not connected`，更新 `meta.backup_last_skipped_at`，**不算错误**，直接返回。
2. **`raw/` 增量追加（只加不删）**：遍历 `<数据目录>/raw/**`（含 `.prev-*`）：
   - 目标文件不存在 → 复制（临时文件 + rename，`fsync`）。
   - 存在且 **长度更短**，且前 4 KB 的 SHA-256 与源一致 → 从目标长度处**追加**源的剩余字节，`fsync`。**目标文件长度本身就是游标**，所以崩溃/拔盘后重跑天然正确，不需要额外状态。
   - 长度相等 → 跳过。
   - **目标更长或前 4 KB 不一致（= 主目录里该文件被重写过，P1 镜像会把旧版改名为 `.prev-<ts>` 再按原名完整重拷）**：把备份盘上的旧文件改名为 `<原文件名>.conflict-<时间戳>`（**不以 `.jsonl` 结尾**，不会被当成会话），然后按新文件完整复制。记一条 info 日志，**不算错误**；只有改名或复制本身失败才进 `errors`。备份里依然什么都不删，再跑一次不会再报冲突（修订 2.1，修复"永远卡住"）。
   - `.prev-*` 是不可变文件，走"不存在才复制"。
   - **从不删除**：主目录里被删/被轮转的东西，备份里保留。
3. **`kb.db` 快照**：用 rusqlite `backup` feature（`Cargo.toml` 给 rusqlite 加 `"backup"`，不增加新 crate）做 **online backup** 到备份目录的临时文件，校验 `PRAGMA integrity_check` 后原子替换 `kb.db`，上一份改名 `kb.db.prev`（**始终保留一份上一版**，不堆积日期快照；`kb.db` 本身可由 `raw/` 重建，所以只需要"最近一致快照"，决策 D5）。**每天最多一次**：`meta.backup_last_db_snapshot_day` 记日期。
4. 结果写 `meta`：`backup_last_at`、`backup_last_result`（`ok`/`skipped`/`error:…`）、复制字节数；并放进 `--kb-sync` 的 JSON 摘要 `backup` 字段。
5. 进度：备份阶段通过 `kb-progress`（phase=`backup`，已拷字节/总字节）上报；`meta.backup_state` 在备份期间为 `running`（修订 2.3）。
6. 中途拔盘 → 拷贝报 I/O 错误 → 记为 `errors[]`（stage `backup`），已拷贝的部分有效，下次续传；**不影响收集本身的成功**。

**并发**：`kb_backup_now` 命令和收集共用 `SyncLock`（同一把 `kb.lock`），语义同 P1：等 3 秒，拿不到就 `skipped`。**不新增第二把锁**。

### 6.3 主页呈现

状态区区分"正在收集"和"正在备份到移动硬盘（已完成 X%）"；备份期间"立即收集/立即备份"按钮提示"备份进行中，完成后可用"（锁不拆，修订 2.3）。

"移动硬盘：<名称> · 上次备份 X"；**超过 7 天**（`now - backup_last_at > 7d`，且配置启用）→ 黄色提醒"已超过 7 天没有备份，请连接移动硬盘"；从未备份且已启用 → "尚未备份"。盘不在线时显示"未连接"（灰）。

---

## 7. 更换数据目录

依据：P1 `kb::paths::DataPaths`、`kb::lock`、`kb::db`；PLAN 第 6 节"绝不清空/覆盖用户数据"。

### 7.1 入口与前置校验（`kb_change_data_dir_check`）

设置页"更换数据目录…" → 选择目标 → **先检查并给出明确结果**：

- 绝对路径；不位于旧目录之内、旧目录不位于新目录之内；不在任何来源根（`~/.claude`、`~/.codex`）之内；
- 可写、剩余空间 ≥ 旧目录占用 × 1.1；
- 目标状态三选一：**空目录** → 迁移；**含 `.agentkb-migrating` 标记** → 上次中断的迁移，可续；**已有 `kb.db`** → 弹两个选项"改为使用这里已有的数据"（只改配置，不拷贝，等同向导里的"继续使用"）或取消；**其他非空** → 拒绝（避免混入）。

### 7.2 迁移流程（`kb_change_data_dir_run`，带进度事件）

1. 拿 `SyncLock` 并**持有到结束**（期间 launchd 的收集会重试 3 秒后 `skipped`，不报错；迁移时间可能几分钟）。
2. 在新目录写 `.agentkb-migrating`（含源目录、开始时间）。
3. **复制 `raw/`**：逐文件，目标不存在或大小不同才复制（所以可续），写临时文件再 rename；`.prev-*` 一并带上。
4. **`kb.db`**：用 online backup API 复制成**一致快照**（不是直接 `cp`，WAL 里可能有未落主文件的数据）。`logs/` 简单复制。
5. **逐字节校验**：对新旧两侧每个文件比较长度 + SHA-256（流式，不进内存）；全部一致才继续。校验报告写入 `migration-report.json`（文件数、总字节、耗时）。
6. 原子写 `config.json`（`data_dir` 改成新路径）→ 删除 `.agentkb-migrating` → 重装定时任务（plist 里的日志路径依赖数据目录）。
7. **旧目录原样保留，不删除**；主页提示"旧数据仍在 <旧路径>，确认无误后可手动删除"。之后的收集写新目录。

### 7.3 中途失败怎么办

| 失败点 | 状态 | 恢复 |
|---|---|---|
| 步骤 2–5（拷贝/校验失败、磁盘满、拔盘）| `config.json` **仍指向旧目录**，旧目录完整；新目录留有 `.agentkb-migrating` 和部分文件 | 用户重试：同一目标会识别标记并**续传**；或改选别处；新目录里的残留不影响任何功能 |
| 步骤 6 写 config 前崩溃 | 同上 | 同上 |
| 写 config 成功后、重装定时任务失败 | 配置已指向新目录，数据完整 | 主页显示"定时任务需要修复"，一键修复；launchd 的 `--kb-sync` 读 config，自然已是新目录 |
| 进程被杀 | 锁由内核释放（P1 的设计），标记保留 | 同上 |

测试：用临时目录验证迁移完整性、`.agentkb-migrating` 续传、校验失败（故意改坏一个字节）不切换配置、旧目录不被改动、目标非空被拒。

---

## 8. 定时任务路径自动修复

### 8.1 谁来做、何时做

`lib.rs::run_tauri` 的 `setup` 钩子里 `std::thread::spawn(|| kb::schedule::ensure_current())`（不阻塞窗口）：

1. 读配置；`schedule_enabled == false` → 不动。
2. **不稳定的运行路径一律不改**（见 4.5：非 `.app` 内、`AppTranslocation`、`/Volumes/…`）；只记一条日志 `schedule repair skipped: unstable executable path`，并让主页提示"请先把 AgentKB 放到 /Applications"。
3. 用 `plist` crate 读 `~/Library/LaunchAgents/io.github.moqi777.agentkb.sync.plist` 的 `ProgramArguments[0]` 和 `StartInterval`；与 `current_exe()`（规范化后比较）和 `config.interval_secs` 对比，并检查 `launchctl print` 是否已加载。
4. 任一不一致（路径不同 / 可执行文件已不存在 / 间隔不同 / plist 缺失但配置要求启用 / 未加载）→ `install`（P1 已幂等）→ 日志一行 `schedule repaired: <旧路径> -> <新路径>` → 写 `meta`：`schedule_repaired_at`、`schedule_repaired_from`、`schedule_repaired_to`。
5. 主页读 `meta` 显示一次性横幅"定时任务已更新到新位置：…"，点 ✕ 后 `kb_dismiss_notice` 清除。

### 8.2 "已失效"与手动修复

`kb_get_status` 额外返回 `schedule.program_exists`（plist 里的可执行文件是否存在）。不存在 → 主页红色"定时任务已失效"+ **修复**按钮（`kb_schedule_install`）。手动修复**允许**用当前路径，即使是开发路径，但按钮旁会注明"当前运行的是开发版本"，避免用户误把正式任务指向 debug 二进制。

---

## 9. 改动清单与新依赖

### 9.1 新增（`kb` 命名空间为主）

| 文件 | 内容 |
|---|---|
| `src-tauri/src/branding.rs` | `ISSUES_URL` 等常量（两处反馈引用）|
| `src-tauri/src/kb/fsutil.rs` | `write_atomic`、`copy_atomic`、目录大小、`fsync_dir`（P1 里重复的原子写抽出来）|
| `src-tauri/src/kb/config.rs`（改）| v1 结构、`migrate_*`、`validate`、`extra` 保留、损坏处理 |
| `src-tauri/src/kb/volume.rs` | `probe`、`list_external`、`FsKind` 判定、`st_dev` 校验 |
| `src-tauri/src/kb/backup.rs` | 增量追加 + `kb.db` 快照 + `meta` 状态 |
| `src-tauri/src/kb/migrate_dir.rs` | 数据目录迁移 |
| `src-tauri/src/kb/probe.rs` | 来源检测、目录测试、调度环境测试（向导/设置页的"测试"）|
| `src-tauri/src/kb/schedule.rs`（改）| `ensure_current`、稳定路径判定、plist 读取、`program_exists` |
| `src-tauri/src/kb/sync.rs`（改）| 进度回调；来源开关；同步后调 `backup`；摘要加 `backup` 字段 |
| `src-tauri/src/kb/commands.rs`（改）| 新命令：`kb_get_config_state`、`kb_get_config`、`kb_save_config`、`kb_restore_default_config`、`kb_probe_sources`、`kb_test_data_dir`、`kb_test_schedule_env`、`kb_list_external_volumes`、`kb_test_mirror`、`kb_wizard_commit`、`kb_backup_now`、`kb_change_data_dir_check`、`kb_change_data_dir_run`、`kb_dismiss_notice`；事件 `kb-progress` |
| `src/kb/KbHome.tsx`、`KbSettings.tsx`、`wizard/*.tsx`、`useKbStore.ts`（扩）、`useKbConfigStore.ts`、测试 | 主页、设置页、向导 |
| `src/i18n/locales/*/kb.json` | 新键（en、zh-CN 认真写，其余先英文）|

### 9.2 修改底座（最小接入）

| 文件 | 行数 | 内容 |
|---|---|---|
| `src-tauri/tauri.conf.json` | ~15 | productName、identifier、`mainBinaryName`、删 updater、`createUpdaterArtifacts:false`、`targets:["app"]`、CSP |
| `src-tauri/Cargo.toml` | ~3 | 删 `tauri-plugin-updater`；rusqlite 加 `backup`；加 `plist` |
| `src-tauri/src/lib.rs` | ~25 | 删 updater 插件；`setup` 钩子（settings.json 迁移、`ensure_current`）；注册新命令 |
| `src-tauri/capabilities/default.json` | −4 | 删 `updater:*` |
| `src-tauri/src/commands/feedback.rs`、`server/handlers.rs` | ~12 | 引用 `ISSUES_URL`；删 mailto 通道 |
| `src/layouts/AppLayout.tsx` | ~15 | 删 `SimpleUpdateManager` 挂载；kb 视图分支；`isTranscriptView` |
| `src/layouts/Header/Header.tsx` | ~20 | AgentKB 工具栏按钮；kb 上下文的"← 知识库" |
| `src/layouts/Header/SettingDropdown/index.tsx` | ~−20/+5 | 删"检查更新"；知识库项改调 `switchToKb` |
| `src/types/analytics.ts`、`hooks/analytics/useAnalyticsNavigation.ts`、`useAnalyticsComputed.ts` | ~15 | `kb` 视图 |
| `src/store/slices/projectSlice.ts` | ~6 | `kb://` 守卫 |
| `src/components/modals/feedback/FeedbackModal.tsx` | ~−15 | 删邮件按钮 |
| `index.html`、`public/manifest.json`、`src/i18n/locales/*/common.json`、`ErrorBoundary.tsx` | ~10 | 名称改 AgentKB |
| `docs/agent-kb/PLAN.md` | — | **leader 维护**；P1 验收里要求的两处更正（"WebUI 不提供 kb_*"、"需要 just"）和 `tauri-axum` 白名单，我不改这个文件，只在报告里列出需要 leader 改的条目 |

### 9.3 新依赖

| 依赖 | 为什么 | 备注 |
|---|---|---|
| `plist = "1"`（Rust）| 解析 `diskutil info -plist` 输出、读 launchd plist 的 `ProgramArguments` | **已在 Cargo.lock（1.7.2）里**（`tauri`、`tauri-plugin-os` 的传递依赖），升为直接依赖，不增加编译量 |
| rusqlite 的 `backup` feature | `kb.db` 一致快照（备份、迁移）| 同一个 crate 的一个 feature，无新 crate |
| `@fontsource/ibm-plex-sans`、`@fontsource-variable/jetbrains-mono`（npm）| **仅当决策 D1 选"内置字体"**：把 Google Fonts 换成本地字体文件 | 增加包体积约 1–2 MB；选 D1-B 则不加 |

**删除**：`tauri-plugin-updater`（及其引入的整个 reqwest/hyper 栈，`Cargo.lock` 会明显变小）。

### 9.4 测试计划

- **Rust**：配置读写/升级（v0→v1、未知字段保留、版本过新、损坏改名恢复）；`validate` 边界；`volume` 用 trait 假实现覆盖"UUID 一致 / UUID 不一致 / 盘没插但路径存在（返回系统盘 UUID）/ 内置盘 / 网络盘"，另加一个 `#[cfg(target_os="macos")]` 的真机集成测试用 `hdiutil` 建镜像（CI 外手动跑，报告贴输出）；`backup` 增量追加、`.prev-*` 整份复制、不删除、目标更长不覆盖、中途中断续传、`kb.db` 快照每天一次、盘未连接静默跳过；`schedule` plist 路径不一致自动修复、稳定路径守卫、间隔变更重装；`migrate_dir` 迁移完整性/续传/校验失败不切换/旧目录不动；进度回调。
- **前端**：向导各步前进/后退/跳过、测试按钮成功与失败文案、"已有数据"提示；设置页每个测试按钮；主页筛选、已删除标记、失效横幅、修复按钮；打开会话的上下文（合成项目）与 `kb://` 守卫；"检查更新"不存在、`check` 不被调用。
- **真机**（步骤 7）：用打包的 `AgentKB.app`（`HOME` 隔离 + 一次装进 `/Applications` 的真实验证），按验收 1–6 逐项走。

---

## 10. 风险与我没把握的地方

1. **未签名 App 在 Apple Silicon 上的启动**：`hardenedRuntime: true` + 无签名身份。Tauri 在 arm64 上通常会做 ad-hoc 签名，但**我还没有真的 build 过**，步骤 2 第一件事就是 `pnpm exec tauri build` 并在隔离环境里启动验证；若 hardened runtime 导致 WebView 起不来，我会改 `hardenedRuntime:false` 并在报告里说明。首次打开方式（右键→打开 / 系统设置→隐私与安全性→仍要打开）也在那时实测后写进报告。
2. **`mainBinaryName` 的实际产物名**：需要构建后确认 `Contents/MacOS/<名字>`，plist 路径以实际为准。
3. **CSP 收紧后 IPC 是否正常**：Tauri 会自动给 `connect-src` 追加 `ipc:`/`http://ipc.localhost`，但我要在真实构建里验证；若不是，就显式写入。
4. **`df`/`diskutil` 的输出依赖 macOS 版本**：已按列解析并用 plist 取值，未做 Windows/Linux。
5. **WebKit 存储丢失**（localStorage 等界面偏好）：已说明不迁移。
6. **向导对"这台机器"的体验**：它会在下次启动时自动弹出（因为没有 `config.json`），并识别 `~/AgentKB` 的已有数据。我认为合理，但这是决策 D4。

---

## 11. 需要你拍板的决策（回复时可直接写"同意"或指出修改）

- [ ] **D1 字体**：A（推荐）内置 IBM Plex Sans / JetBrains Mono，CSP 变成纯 `'self'`，App 零外网请求；B 保留 Google Fonts（现状，启动会请求 `fonts.googleapis.com`，离线时回退系统字体）。
- [ ] **D2 反馈邮件通道**：删除（推荐；原默认地址是原项目的域名，我不能替你编一个）；若你想保留，请给我邮箱地址。
- [ ] **D3 打开会话的上下文**：A（推荐）合成 `kb://` 项目 + 6 行守卫，复用三栏布局；B 在 KB 页内自带查看器。
- [ ] **D4 向导对已有用户**：没有 `config.json` 就自动弹向导（含这台机器），可点"稍后"，下次仍弹。
- [ ] **D5 `kb.db` 备份**：只保留 `kb.db` + `kb.db.prev` 两份（推荐），不做日期快照堆积。
- [ ] **D6 可执行文件改名** `mainBinaryName: "AgentKB"`（Cargo 包名不动）。
- [ ] **D7 备份盘只接受外接卷**（`Internal=false`）；系统内置盘、网络盘、云盘拒绝。`hdiutil` 镜像算外接，可用于验收。
- [ ] **D8 identifier 变更时只迁移 `settings.json`**（复制、不移动），WebKit 存储不迁移。
- [ ] **D9 `PLAN.md` 的两处更正由 leader 改**（见 9.2 末行），我在报告里列清单。
- [ ] **第 5 节整体**：新增 `kb` 视图、对底座 `AppLayout/Header/analytics/projectSlice` 约 60 行的接入，同意。

---

## 12. leader 审核后的修订（`P2-design-review.md`）

- 第 11 节全部按推荐项决定（D1=A 内置字体，CSP 收紧为只允许 `'self'` 并作为"不访问原项目"的证据；D2 删邮件；D3=A，`SessionCopyMenu` 先读代码再定，结论写报告；D4–D8 同意；D9 由 leader 已改 PLAN）。
- **2.1 备份冲突**：见 6.2——冲突时旧文件改名 `.conflict-<ts>`，重新完整复制，不算错误；补测试：重写后备份盘同时有旧版本与完整新版本，且再跑一次无冲突。
- **2.2 版本号**：`package.json` 保持 1.32.0 不变；`kb` 模块加 `AGENTKB_VERSION = "0.2.0"`（第 N 期 `0.N.0`）。关于页与主页显示"AgentKB 0.2.0（基于 Claude Code History Viewer 1.32.0）"。
- **2.3 首次备份进度**：见 6.2 第 5 点与 6.3。续传（中途退出/拔盘）要有测试。
- 实现注意：未签名启动若需改 `hardenedRuntime` 写明原因；D8 验证项——本机 `com.claude.history-viewer/settings.json` 含简体中文设置，迁移后 AgentKB 首次打开应直接是中文；真机确认清单放报告末尾。
