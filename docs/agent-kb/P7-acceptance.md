# P7 验收（Windows 版）

## 1. leader 复核

- 设计：`P7-design-review.md`；2a：`P7-2a-review.md`（`391aa77` 合并）；2b：`P7-2b-review.md`（`414077f` 合并）
- Mac 上 7 项全过，`.app` 正常，语义模型正常

## 2. 用户在 Windows 上真机确认

安装包（2026-10-18 真机反馈修复版）：Windows 电脑上 `D:\AgentKB-artifacts\AgentKB_1.32.0_x64-setup.exe`，SHA-256 `2557C1A30FEB1318421158338D68AB66B901EA74474BA6E530F97204E82E95E3`。

已修复 npm `claude.cmd` 参数无效，以及自定义安装目录导致后台收集/skill 安装被拒绝的问题；七项检查与正式 EXE 离线验证见 `P7-2b-report.md` 第 9 节。旧版已按用户要求卸载，数据和设置保留，以下真机验收请使用此新包重试。

| # | 项 | 结果 |
|---|---|---|
| 1 | 安装：SmartScreen 提示后"更多信息 → 仍要运行"能装上，不要管理员权限，装在 `%LOCALAPPDATA%\Programs\AgentKB` | ✅ 用户改装到 D 盘，没有 SmartScreen 提示（本机打的包，没有下载标记），打开是设置向导 |
| 2 | 设置向导走完，第一次收集完成，会话数量和 Claude Code / Codex 里的大致对得上 | ✅ 向导走完，收集到 190 个会话，用户确认没问题 |
| 3 | 知识库页搜几句中文，结果正常，打开会话能看原文 | ✅ 用户确认找到 |
| 4 | 任务计划程序里有 AgentKB 的任务；过了一个收集间隔，首页"上次收集"时间更新 | ✅ 任务：只在登录时运行、登录触发 + 每 5 分钟、指向 `D:\AgentKB\AgentKB.exe`、不限交流电；16:05:02 定时收集成功。同时每日整理在跑（25/190），npm `claude.cmd` 修复生效 |
| 5 | 安装 skill 后，在 Claude Code 里问"之前……是怎么解决的"，Agent 能调用 AgentKB 找到 | ✅ Claude Code 2.1.296 加载 skill、跑了 1 条命令，用当天整理出的笔记回答 go-ios 问题，给了出处和恢复命令。界面里的 hook 报错来自用户自己配置的 git-ai hook（路径指向另一个用户目录），和 AgentKB 无关 |
| 6 | 同上，在 Codex 里 | ⚠️ 答对了，但 skill 里写的命令 `& 'D:\AgentKB\AgentKB.exe' --kb-search "go-ios iPhone"` 在 Codex 0.163.0 里**没有任何输出**，Agent 自己改成 `cmd.exe /c "D:\AgentKB\AgentKB.exe ..."` 才拿到结果（用户开的是 YOLO 模式，普通模式下每条都要用户批准）。见第 3 节 |
| 7 | 下载语义模型，"测试"通过，口语化搜索能找到 | ✅ 模型就位、校验一致；索引 8,949/8,949 段（13 MB）；搜"手机连接电脑控制那个事"（关键词+语义）找到 16 个会话，go-ios 那次在会话结果第 2 位 |
| 8 | 插上移动硬盘（或 U 盘），设置页能选到，备份成功 | |
| 9 | 第二天：每日整理有笔记 | |
| 10 | 重启电脑登录后，定时收集自动恢复 | |

## 3. 要修：Codex 里 PowerShell 调用拿不到输出

现象见第 6 项。2b 的离线脚本里 PowerShell 能拿到输出，真实 Codex 里拿不到，说明脚本没有还原 Codex 的调用方式。常见原因：PowerShell 调用图形界面程序（`windows_subsystem = "windows"`）时，输出不是被管道接走的话，它不等程序结束、也不收输出。

要求（Windows 执行 AI）：

1. 你自己就在 Codex 里，先用**你自己的命令执行工具**原样跑 skill 里的命令，复现"没有输出"，查清 Codex 是怎么起 PowerShell 的、为什么拿不到
2. 给出修法，优先考虑：安装包里带一个**控制台程序**（例如 `agentkb-cli.exe`，同一套代码、`console` 子系统），两个 skill 模板都改用它，在 Git Bash、PowerShell、cmd 里都不依赖技巧就能拿到输出；如果有更简单又可靠的办法，说明理由
3. 离线脚本要改成能复现这次的问题（修之前失败、修之后通过），再在 Codex 和 Claude Code 里各真跑一次
4. 只改 Windows 相关部分，Mac 的 skill 内容不变；7 项检查、新安装包、SHA-256，写进 `P7-fix-codex.md`，推送 `origin/agent-kb-win` 后停下

## 4. Windows 自主补测（2026-10-18）

用户允许在 leader 离线期间继续完成可独立执行的验证。详见 [P7-fix-codex.md 第 6 节](P7-fix-codex.md#6-2026-10-18-补充自主验收)。

- 第 6 项修复：`372ea213` 已完成 Windows 检查、正式 EXE 的 Codex/Claude 调用，待 leader 复核及用户安装新版后更新技能。
- 第 7 项：Windows 实际下载并校验 585,562,194 字节模型，正式 EXE CPU 模型测试及口语化语义搜索通过；临时假数据测试，设置页点击和用户真实数据搜索仍待确认。
- 第 9/10 项的补充证据：Windows 临时任务真实计时触发两次，退出均为 0，后台 fake AI 生成笔记且第二次不重复当日整理。未跨日、未重启，不将这两项标为完成。
- 第 8 项：没有连接外部磁盘，待插入真实设备验证。

第 2 节保留用户真机确认口径；以上自动化补测不替代其中尚未完成的交互、物理设备或跨日验收。

## 5. 安装版补测和自动验收准备（2026-10-18 晚间）

详见 [P7-fix-codex.md 第 7 节](P7-fix-codex.md#7-2026-10-18-安装版升级与后续验收准备)。

- 新包已实际升级至原 **`D:\AgentKB`**，两份正式 skill 已更新为模板 **6**，均调用 `agentkb-cli.exe`。安装后的程序在真实 Codex 命令工具、Git Bash 中，对临时假数据返回完整中文 JSON；原配置保留，正式收集任务升级后自动运行成功。
- 安装目录中的 EXE 重新通过真实模型下载、校验、CPU 测试、语义搜索；当前电脑锁屏，GUI 点击流程仍待解锁后确认。
- 已安排 10 月 19 日 **00:01** 的隔离跨日任务，以及下次实际重启登录后的隔离验收。当前结果 **pending**；结果目录 `D:\AgentKB-build\tmp\agentkb-p7-deferred-zksf03k5`，任务执行后自删，未触发时 7 天过期。未主动重启电脑。
- 第 8 项仍缺少真实外部磁盘。
- 新发现的公共时间戳边界问题见 [P7-timezone-findings.md](P7-timezone-findings.md)，已复现，未修复，交 leader 另行评估。

## 6. leader 复核 Codex 修复（`372ea21`..`e6747ab`）：通过，已合并

- 修法正确：根因是 PowerShell 遇到图形界面程序不等结束、不收输出；改成随包带控制台程序 `agentkb-cli.exe`，只接受 `--kb-search`、`--kb-show`、`--kb-skill-print`，复用同一套代码。修之前离线脚本会失败、修之后通过，并在真实 Codex 工具和 Claude Code 2.1.296 里各跑了一次
- Mac 不受影响：Mac 上 7 项全过（vitest 1576，cargo test 库 1485 通过 0 失败），`pnpm tauri build` 正常出 `.app`，包里只有 `AgentKB`，没有多出 `agentkb-cli`；Mac 的 skill 模板还是 5
- 快进合并到 `agent-kb`
- 记下来的事：
  - 执行 AI 在用户同意下自己升级了安装、替换了两份正式 skill（有备份），还注册了两个会自删的验收任务（零点、下次重启登录）。以后这类改真实环境的操作仍要先由用户确认
  - 时区问题（`P7-timezone-findings.md`）：Mac 真实库 88,988 条消息时间全是 `Z` 结尾，目前碰不到。记为待办，P8b 之前在 Mac 上修（`plan.rs` 改成按真实时间比较）

## 7. 用户真机发现：每日整理 20 个会话失败（2026-10-19）

用户点了"整理全部未整理的"：10-18 20:22 完成，整理 170 个会话，输入 262 万 token、输出 12 万 token；**20 个会话各失败 3 次**。截图里看到两种原因：

- `invalid answer: more than 8 entries`：长会话里 AI 给出了超过 8 条笔记，校验直接判失败，重试 3 次都一样，白花 token
- `workspace routing discovery failed`：不是 AgentKB 的报错文字，像是 Claude Code 或 Codex 命令行自己报的

Mac 上目前 36 条整理记录全部成功（Mac 只整理最近的会话，没做过全量）。修复任务见 `P7.1-digest-fixes.md`。
