# 第 2 期验收结论（leader）

**结论：通过。** 第 1–8 项由 leader 独立复核，第 9、11 项由用户真机确认。第 10 项（注销重登）暂缓，理由和替代证据见第 4 节。第 5 节的小问题作为 P2.1 修完后再进入第 3 期。

## 1. leader 独立复核（2026-10-14，用户本机，隔离 HOME，用的是已安装的 `/Applications/AgentKB.app`）

| 验收项 | 复核方式 | 结果 |
|---|---|---|
| 7 门禁 | 在 `7a2ed91` 上重跑全部 7 项 | 全部通过：vitest 1456，cargo lib 1256（1 ignored），clippy 0 警告 |
| 1 打包与身份 | `codesign -dv`、Info.plist | bundle id `io.github.moqi777.agentkb`，ad-hoc 签名并开启 runtime；二进制构建时间 17:46，晚于最后一个代码提交 `b482f3f`（17:45），装的就是最新代码 |
| 2.2 版本 | `package.json`、`kb/mod.rs` | 上游版本号 1.32.0 没有改；`AGENTKB_VERSION = "0.2.0"` |
| 3 不访问原项目 | `strings` 二进制、`cargo tree -i reqwest`、`dist/`、CSP | `jhlee0409`、`api.github.com`、`latest.json`、两个 Google Fonts 域名、原作者邮箱都是 0 命中；二进制里唯一的 GitHub 业务链接指向 `moqi777/.../issues/new`；没有 HTTP 客户端；CSP 只允许 `'self'` |
| 4 备份 | 2GB 稀疏 APFS 镜像模拟外接盘，源数据是 2 个真实会话的副本（180MB） | 首次同步顺带备份，用时 0.9s，`diff -rq` 完全一致；拔盘后同步退出码 0，备份 `skipped/not_connected`，`/Volumes` 没有多出东西；另挂一块**同名但 UUID 不同**的盘 → 跳过且没有写入；插回原盘 → 只追加了 68 字节 |
| 2.1 重写冲突 | 把源会话改短重写 | 备份盘上出现 `.conflict-*`、`.prev-*` 和完整的新文件，新文件与主目录逐字节一致，`errors` 为空；第二次运行 0 动作 |
| 5 换数据目录 | `--kb-move-data` | 5 个文件 SHA-256 全部一致；旧目录哈希不变；之后的收集写到新目录 |
| 2 定时任务自修复 | `--kb-schedule install / ensure` | 安装后 launchd 立即补跑，`sync_runs` 出现 `trigger=schedule`；从复制出来的 App 运行 ensure → `repaired (the app moved)`，plist 指向新路径，日志有记录；回到 `/Applications` 再 ensure → 修回来。测完已卸载，`launchctl list` 里没有残留 |
| 配置损坏 | 写入半截 JSON 后运行 `--kb-sync` | 给出明确错误，退出码 1，不会悄悄用默认值 |
| 真实数据 | 复核前后检查 `~/AgentKB`、`~/Library/LaunchAgents` | 没有被动过，没有残留的定时任务 |

复核时我犯了一个错：第一次测自修复时测试配置没有打开 `schedule_enabled`，得到 `not_needed`。这是正确行为（没启用定时就不应该安装），改好配置后重测通过。

## 2. 对报告的更正

1. **第 8 节 A 的配置路径写错了。** 配置实际在 `~/Library/Application Support/agent-kb/config.json`（`kb/config.rs::config_path`），不是 `~/.claude-history-viewer/config.json`。本机目前根本没有 config.json，所以打开 App 会直接进入向导，**不需要改名**。
2. **D8 的前提是我的判断错误。** 本机没有 `com.claude.history-viewer` 目录，我在审核意见里说"本机有简体中文设置"是没有核实就下的判断。迁移逻辑本身已经在隔离 HOME 里验证过，本条不再作为本机验收项。首次打开如果是英文，在设置里切换成简体中文即可。

## 3. 不影响验收、记下来的事

| # | 事项 | 处理 |
|---|---|---|
| 1 | 源文件被重写时，备份盘上 `.conflict-*` 和 `.prev-*` 的内容是同一份旧版本，占用两份空间 | 暂不处理；以后做空间统计时再考虑 |
| 2 | CSP 去掉了 `img-src https:`，会话里引用的网络图片不会再显示 | 这是"不访问外网"的预期代价，保留 |
| 3 | WebKit localStorage 里的界面偏好不随标识迁移 | 接受 |
| 4 | ko、ja、zh-TW 的新文案还是英文 | 按 PLAN 允许 |
| 5 | 打包版 GUI 和运行时抓包没跑（dev 进程占着单实例） | 由第 4 节的用户真机确认覆盖 |

## 4. 用户真机确认（2026-10-15）

- [x] 9 向导：自动弹出，5 步顺利走完，每个测试都给出了明确结果（Claude 43 个、331MB；Codex 231 个、2.45GB；数据目录可写，剩余 134GB，识别出已有的 268 个会话；后台任务指向 `/Applications/AgentKB.app`）。完成后 leader 核对过：config.json 内容正确，plist 指向正式路径，launchd 立即补跑了一次（`sync_runs` 第 15 行 `trigger=schedule`，状态 ok）。旧的 11 个 `[Image` 标题已经随这次同步修正
- [ ] 10 注销重登：**暂缓**，用户内存里有正在进行的工作，不方便注销。替代证据：leader 在隔离 HOME 里，以及向导安装后在本机，都看到 launchd 加载时立即补跑（RunAtLoad，和登录时走的是同一套机制）。等用户下次正常重启后，leader 查一下 `sync_runs` 补上这一项
- [x] 11 打开 Codex 会话：顶部显示 `AgentKB / basicbs-billingcenter_2.0`，中间栏是 Codex 的会话列表，带"Codex CLI"标签；有"返回知识库"按钮，没有删除、重命名入口

## 5. P2.1 小修清单（用户真机确认时发现）

| # | 问题 | 要求 |
|---|---|---|
| 1 | 左下角状态栏显示 `v1.32.0` | 改成显示 AgentKB 0.2.0 |
| 2 | 顶部副标题还是原项目的"探索和分析您的 Claude Code 对话历史" | 改成 AgentKB 自己的说明，5 种语言都要改 |
| 3 | 系统是简体中文（`AppleLanguages = zh-Hans-CN`），首次打开却是英文。原因：i18next 的检测器先把 WebView 报告的 `en-US` 缓存进 localStorage 的 `i18nextLng`，`useLanguageStore.loadLanguage` 优先读这个值，按系统语言（`plugin-os locale()`）设置的分支就走不到了 | 第一次启动（settings.json 里还没有 language）时按系统语言设置；向导第 1 页加语言切换 |
| 4 | 向导数据目录页"检测到已有数据，将继续使用"出现两遍 | 只保留一处 |
| 5 | 收集频率是 10 分钟时，选中的是"自定义"按钮，而不是"10 分钟" | 值等于 5、10、15 分钟时选中对应的按钮 |
| 6 | 移动硬盘没勾选，config.json 里还是写了 `target_dir`、`volume_uuid`（选的是本机挂着的"AweSun Installer"安装盘） | 没启用就不保存盘的信息；另外只读卷、安装镜像这类盘不应该出现在候选列表里 |
| 7 | 不少 Codex VS Code 会话的标题是 `Version: 2.1.296 Session name: … Session ID: …` 这种注入的上下文，不是用户真正的问题 | 取兜底标题时跳过这类前导内容；已有会话的标题要重新生成一次 |
| 8 | 报告第 8 节 A 写错了配置路径 | 不用改报告，以本文第 2 节为准 |

验收方式：门禁全部通过，并附上 #1、#2、#4、#5 的截图和 #3、#6、#7 的测试；重新打包并安装到 `/Applications`。

## 6. P2.1 验收（2026-10-15）

**结论：通过。** leader 在 `add120a` 上重跑门禁：vitest 1464 个、cargo lib 1261 个全部通过，clippy 0 警告。`/Applications/AgentKB.app` 的打包时间（00:28）晚于最后一个代码提交 `47098f1`。本机 `kb.db` 里已经没有 `Version:`、`[Image` 开头的标题；settings.json 的语言是 `zh-CN`。用户真机确认了状态栏、副标题和标题。

- 本机 config.json 里还留着 AweSun 那块盘的信息。这是修复之前写进去的，下次保存设置时会被清掉，不用处理
- 首页说明文字在窗口较窄时句号会单独掉到第二行：放到第 3 期顺手修
