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
| 7 | 下载语义模型，"测试"通过，口语化搜索能找到 | |
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
