# P7 验收（Windows 版）

## 1. leader 复核

- 设计：`P7-design-review.md`；2a：`P7-2a-review.md`（`391aa77` 合并）；2b：`P7-2b-review.md`（`414077f` 合并）
- Mac 上 7 项全过，`.app` 正常，语义模型正常

## 2. 用户在 Windows 上真机确认

安装包：Windows 电脑上 `src-tauri\target\release\bundle\nsis\AgentKB_1.32.0_x64-setup.exe`，SHA-256 `B4B22F6BFB7AE5D2AAD65E132BFF6D2F1FF38B52CB2F197FA897C3037122672A`

| # | 项 | 结果 |
|---|---|---|
| 1 | 安装：SmartScreen 提示后"更多信息 → 仍要运行"能装上，不要管理员权限，装在 `%LOCALAPPDATA%\Programs\AgentKB` | |
| 2 | 设置向导走完，第一次收集完成，会话数量和 Claude Code / Codex 里的大致对得上 | |
| 3 | 知识库页搜几句中文，结果正常，打开会话能看原文 | |
| 4 | 任务计划程序里有 AgentKB 的任务；过了一个收集间隔，首页"上次收集"时间更新 | |
| 5 | 安装 skill 后，在 Claude Code 里问"之前……是怎么解决的"，Agent 能调用 AgentKB 找到 | |
| 6 | 同上，在 Codex 里 | |
| 7 | 下载语义模型，"测试"通过，口语化搜索能找到 | |
| 8 | 插上移动硬盘（或 U 盘），设置页能选到，备份成功 | |
| 9 | 第二天：每日整理有笔记 | |
| 10 | 重启电脑登录后，定时收集自动恢复 | |
