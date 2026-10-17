# 第 7 期：Windows 版

> 先读 `PLAN.md`（尤其第 3 节架构决定、第 4 节"第 7 期约定"、第 6 节通用约束），再读各期的验收文件，了解每个功能做到了什么程度。
> 遇到 **【检查点】** 必须停下，把产出交给用户转给 leader 审核，通过后才能继续。
> 这一期由**在 Windows 电脑上的执行 AI** 来做。结束时版本 `AGENTKB_VERSION = 0.7.0`。

## 1. 目标

Mac 版现有的功能，在 Windows 10/11（x64）上全部能用：后台定时收集、原样备份、关键词和语义搜索、skill 一键装进 Claude Code 和 Codex、每日整理、导出导入、换数据目录、移动硬盘备份、设置向导。同时 **Mac 版不能被弄坏**。

## 2. 和 Mac 那边同时开发的规则（避免冲突）

Mac 上的执行 AI 正在做第 6.1 期（搜索排序小修）。两边同时干活，按下面的规矩来：

| 规则 | 内容 |
|---|---|
| 分支 | 你在 **`agent-kb-win`** 上开发：从 `origin/agent-kb` 最新的提交拉出，推送到 `origin/agent-kb-win`。**不要推送到 `agent-kb`**，合并回 `agent-kb` 由 leader 在检查点通过后做 |
| 跟上 Mac | 每个检查点之前，以及开始一个新步骤之前，先 `git merge origin/agent-kb`，冲突自己解决并在报告里写明 |
| 不碰的文件 | 6.1 期验收之前，**不要改 `src-tauri/src/kb/search.rs` 和 `src-tauri/src/kb/semantic/rank.rs`**。真要改，先停下来提问 |
| 平台代码放哪 | 和系统有关的写法尽量集中到新文件，例如 `kb/platform/windows.rs`、`kb/platform/macos.rs`，原来的调用处只做最小改动，用 `#[cfg(target_os = ...)]` 分开。共用的逻辑不要复制一份 |
| Mac 不能坏 | 你没法在 Mac 上跑测试，所以每个改动都要确认 macOS 那一侧的代码没有被改变行为。leader 会在合并前在 Mac 上跑全部检查并重新打包 |

## 3. 安全约束（和 Mac 一样）

- 这台 Windows 电脑上如果有用户真实的 Claude Code、Codex 记录（`%USERPROFILE%\.claude`、`%USERPROFILE%\.codex`），**只读**；测试一律用临时目录当 HOME（`USERPROFILE`、`APPDATA` 等都指到临时目录）
- 不往真实的 skills 目录装测试用的 skill，不改用户真实的配置
- 遵守全局 skill `no-secrets-access`：不读密钥、凭据文件
- 不提交任何用户的个人数据（会话原文、真实路径里的用户名可以写，内容不行）
- 不联网，唯一例外和 Mac 一样：用户点"下载模型"。构建时下载依赖可以

## 4. 执行步骤

### 步骤 0：搭环境，摸底

1. 克隆 `origin`（`moqi777/claude-code-history-viewer`），切到 `origin/agent-kb`，拉出 `agent-kb-win`
2. 装好工具链：Rust（MSVC）、Node + pnpm、Tauri 的 Windows 依赖（WebView2、VS Build Tools）。记下各自的版本
3. 在 Windows 上跑 PLAN 第 6 节的 7 项检查，**原样记下哪些失败、失败在哪**。现在的代码从没在 Windows 上跑过，失败是预期的
4. 试着打一次包，记下结果

### 步骤 1：写调研和设计 `docs/agent-kb/P7-design.md` 【检查点】

每一条都写出依据的文件和函数：

1. **步骤 0 的结果**：工具版本；7 项检查各自的结果，失败的测试按原因分组
2. **这台电脑上的真实情况**（只看，不改）：Claude Code 和 Codex 有没有装、装在哪、会话文件在哪、`claude`/`codex` 命令的实际路径（npm 的 `.cmd`、WinGet、其他）；skill 目录在哪
3. **每个和平台有关的地方**，Windows 上怎么做。至少包括：
   - 定时收集：任务计划程序（建、改、删、查状态；用户没登录时怎么办；电脑睡眠后补跑）
   - 后台拉起子进程（整理、建向量）：不弹黑窗口、父进程退出后子进程照常跑完、能被取消
   - 找 `claude`/`codex` 命令；调用时的参数转义（Windows 的命令行引号规则）
   - 文件：打开着的文件不能改名或删除、`rename` 目标存在时失败、路径大小写不敏感、长路径、换行符；锁文件的写法（`kb.lock`、`digest.lock`、`embed.lock`）
   - 导出时对 `raw/` 建硬链接：NTFS 可以，exFAT、FAT32 的盘上怎么办
   - 移动硬盘识别（Mac 用 `/Volumes`）
   - 可用内存（Mac 把非活跃、可清除页算上）和降低优先级（Mac 用 `nice 19`）
   - ONNX Runtime 在 Windows 上：怎么链接、包会大多少
   - 数据和配置放在哪（Mac 的 `~/AgentKB`、`~/Library/Application Support/agent-kb`）
   - 安装包：NSIS 还是 MSI；没有代码签名时 SmartScreen 会怎么提示，怎么跟用户说明
4. **代码怎么组织**：哪些放进 `kb/platform/`，改动清单，新依赖
5. **测试计划**：哪些测试改成两个平台都能跑，哪些只在 Windows 上跑；真机怎么验证（临时 HOME，不碰真实数据）
6. **风险和要 leader 决定的事**

写完提交并推送到 `origin/agent-kb-win`，然后**停下**，等待审核。

### 步骤 2 起

按审核通过的设计实现，分几次检查点由 leader 在审核时定。

## 5. 验收（概要，审核设计时细化）

- Windows 上 7 项检查全部通过；Mac 上 7 项检查也全部通过（leader 跑）
- 第 1 节列出的功能在 Windows 上逐个有命令和输出作为证据，全部在临时目录里做
- 打好的 Windows 安装包，用户在这台电脑上真机确认：装好、设置向导、定时收集、搜索、skill、整理、语义搜索
- 不碰源目录：前后对 `%USERPROFILE%\.claude`、`%USERPROFILE%\.codex` 拍快照对比，没有变化
