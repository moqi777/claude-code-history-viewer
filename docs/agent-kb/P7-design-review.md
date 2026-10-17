# P7 设计审核意见（leader）

**结论：设计通过，可以开始步骤 2。** 步骤 0 做得实在：每项检查都在临时目录里真跑了，失败按原因分了组，打包也试过了。实现分两次检查点（第 3 节）。

## 1. 对第 5 节问题的答复

| # | 问题 | 决定 |
|---|---|---|
| 1 | 定时任务用 S4U（没登录也能跑） | **不用 S4U，改成"只在用户登录时运行"**，和 Mac 的 launchd（LaunchAgent）一样：用户登录后才收集。理由：S4U 会碰到"作为批处理作业登录"的组策略，拿不到用户的网络凭据，整理调用 `claude`/`codex` 时也可能出问题。错过的时间靠 `StartWhenAvailable` 补跑 |
| 2 | `.cmd` 怎么调用 | **接受 `.cmd`**，但**不要自己写引号转义函数**。直接用 Rust 标准库的 `Command::new("x.cmd").args(...)`：标准库（1.77 起）会按 cmd.exe 的规则转义参数，遇到没法安全转义的参数会直接报错，不会把它交给 shell（这就是 BatBadBut / CVE-2024-24576 的修法）。找命令时，如果同时有 `.exe`，优先用 `.exe`（Claude 的原生安装会在 `%USERPROFILE%\.local\bin` 放 `claude.exe`） |
| 3 | exFAT/FAT32 移动盘 | **正式支持备份**，导出时硬链接失败就按现有逻辑退回复制。卷的身份用 GUID；GUID 变了就当作一块新盘，提示用户重新选，不自动迁移 |
| 4 | ONNX Runtime | **只用 CPU，不带 DirectML**，和 Mac 一致，安装包也小。尽量和 Mac 一样静态链接；做不到就带一个与 `ort` 版本匹配的 `onnxruntime.dll`，只从安装目录加载。报告里写明安装包大小和许可证 |
| 5 | 安装包 | **NSIS**，按用户安装（`%LOCALAPPDATA%\Programs`），不需要管理员权限；MSI 以后再说。**不签名**，安装说明里写清 SmartScreen 会提示、怎么"仍要运行"，以及安装包的 SHA-256 |
| 6 | Windows 上 clippy 的 23 个错误 | **这期里修掉。** 两个平台的 7 项检查都要全绿，这是 PLAN 第 6 节的硬要求。它们多半是只在 Windows 才编译的代码，Mac 上看不到 |
| 7 | 没有特权时的符号链接测试 | 测试检测到 `os error 1314` 时**跳过并打印原因**，不算通过也不算失败。**但安全检查本身不能跳过**：Windows 上的目录联接（junction）和其他重解析点，也要和符号链接一样被挡住（`discover.rs` 遍历目录时、`safe_join` 时）。为此加一个用 junction 的测试，junction 不需要特权 |

## 2. 设计里没有提到、需要补上的

1. **命令行输出**：`AgentKB.exe` 是图形界面程序（`windows_subsystem = "windows"`）。skill 让 Agent 调用 `AgentKB.exe --kb-search ...` 并读它的输出。要验证：在 **Claude Code 和 Codex（Windows 版）里各让 Agent 真跑一次** skill，能拿到完整的 JSON 输出，中文不乱码。拿不到的话，在设计里提办法，例如附带一个控制台版的小程序给 skill 用
2. **skill 里的程序路径**：Windows 的安装路径可能有空格、中文用户名。skill 模板里的路径要在这种情况下也能用，测试里用带空格和中文的临时目录
3. **编码**：读写 `claude`/`codex` 的输出、日志、`--kb-search` 的输出，一律按 UTF-8 处理，不依赖系统代码页（中文 Windows 默认是 GBK）
4. **合并 Mac 的新代码**：`agent-kb` 上已经有 P8a（0.6.4，`kb/dedupe.rs`、`sync.rs`、`backup.rs` 有改动）。开始步骤 2 前先 `git merge origin/agent-kb`
5. **版本号**：设计里写的是"保持 0.6.3"，合并后应以 `agent-kb` 上的 0.6.4 为准，最后升到 0.7.0

## 3. 实现分两个检查点

- **步骤 2a【检查点】：Windows 上 7 项检查全绿。** 修测试夹具（路径、fake CLI 改成 Rust 写的小程序）、`fsutil`/`lock` 的 Windows 写法、clippy，以及 junction 安全检查。这一步基本不加新功能。交付后由 leader 在 Mac 上跑全部检查并合并回 `agent-kb`
- **步骤 2b【检查点】：功能。** 定时任务、子进程、找命令、移动硬盘、内存和优先级、ONNX Runtime、NSIS 安装包、第 2 节的 skill 输出验证。交付后 leader 在 Mac 上复核并合并，再由用户在 Windows 上做真机确认

每个检查点写一份报告（`P7-2a-report.md`、`P7-2b-report.md`），贴出命令和输出。

## 4. 记下来的事

- 真实目录只做了存在性检查、没读内容，做得对，后面也保持
- PowerShell 执行策略挡住 `pnpm.ps1` 的问题：报告里写清楚绕过的方法，不要改用户的系统执行策略
