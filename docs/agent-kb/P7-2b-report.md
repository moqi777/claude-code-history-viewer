# P7 步骤 2b 报告（Windows）

日期：2026-10-18
分支：`agent-kb-win`

## 1. 基线和合并

- 开始前已在 `agent-kb-win` 执行 `git fetch origin`、`git merge origin/agent-kb`，快进到 `origin/agent-kb` 的 P8a 合并提交 `768daa9d`。
- `AGENTKB_VERSION` 已从 0.6.4 升到 0.7.0；底座 `package.json`/Tauri 产品版本仍为 1.32.0。

## 2. 实现内容

### 定时收集

- 新增 `kb/platform/windows.rs` 的 Windows 平台层。
- `kb/schedule.rs` 在 Windows 使用 `schtasks.exe /Create /XML` 注册 `io.github.moqi777.agentkb.sync`。
- 任务使用 `InteractiveToken`，只在用户登录时运行；`StartWhenAvailable=true`，登录触发器负责登录后启动，重复触发器按配置间隔补跑；不使用 S4U，也不需要管理员权限。
- 任务参数为 `--kb-sync --scheduled`，因此收集完成后会按既有逻辑拉起整理和向量任务。
- `schtasks.exe` 调用设置 `CREATE_NO_WINDOW`，任务状态/删除/安装入口统一走 `schedule::{status_now,install_now,uninstall_now}`。
- `classify_exe` 只接受真实的 `%LOCALAPPDATA%\Programs\AgentKB\AgentKB.exe`，不再接受下载目录、U 盘或测试文件名。

### 子进程、命令和编码

- `lock.rs` 和 `status.rs` 的 Windows 进程存活检查改为 `OpenProcess` + `GetExitCodeProcess`，不再调用 `tasklist`。
- 整理、向量构建、skill 检查和语义测试子进程使用 `CREATE_NO_WINDOW`；后台整理/向量进程使用 detached 标志，不依赖父进程存活。
- Claude/Codex 命令发现按 `.exe`、`.cmd` 顺序检查 `%USERPROFILE%\.local\bin`、Claude 本地目录、`%APPDATA%\npm` 和 `PATH`；调用均使用 `Command::new(...).args(...)`，不拼接 shell 字符串。
- Rust JSON stdout/stderr 继续按 UTF-8 字节读取；skill 模板在 Windows 对包含空格或中文的安装路径加双引号。

### 移动硬盘、内存和优先级

- Windows 通过 Win32 卷 API 读取卷 GUID、卷名、文件系统类型、可写状态和可移动盘；备份仍保留硬链接失败后的复制回退，卷 GUID 变化会被视为新盘。
- `GlobalMemoryStatusEx` 提供可用物理内存；`SetPriorityClass` 映射默认 below-normal、background 为 idle、none 不变，失败不影响索引结果。

### ONNX Runtime 和安装包

- `ort` 改为只启用 `std`、`ndarray`、`download-binaries`，关闭默认 `copy-dylibs`；Windows 运行时静态链接 CPU 版，不启用 DirectML。
- NSIS 已启用（`src-tauri/tauri.conf.json`），默认用户安装位置由 Tauri NSIS 为 `%LOCALAPPDATA%\Programs\AgentKB`，无需管理员权限。
- 打包命令：`pnpm build`；随后以空 `beforeBuildCommand` 配置运行 `pnpm tauri build --bundles nsis --config '{"build":{"beforeBuildCommand":""}}'`（本机未安装 `just`，未修改系统执行策略）。
- 产物：`src-tauri/target/release/bundle/nsis/AgentKB_1.32.0_x64-setup.exe`，大小 16,232,067 字节，SHA-256：`43E933067E65677CEF3955B635255AE7209A8CC7B5EECA949D46A8158470B064`。发布目录不再有 `DirectML.dll`；未签名安装包的 SmartScreen 提示及“仍要运行”说明留给发布说明补充。

### skill 输出验证

- Windows 临时目录测试覆盖了带空格和中文的安装路径（`用户 with space\Local AppData`），验证 skill 安装、状态和卸载只触碰临时 `.claude\skills\agentkb`。
- `--kb-search` 的 JSON 输出沿用现有 UTF-8 stdout；所有自动化测试均使用临时 `CCHV_TEST_HOME`，没有调用真实 Claude/Codex skill，也没有读取真实记录或凭据。Claude Code/Codex Windows 客户端中的真实 Agent 调用需用户安装后做最后真机确认。

## 3. Windows 验证

所有测试使用临时目录，例如 `%TEMP%\agentkb-p7-2b-tests-full`、`%TEMP%\agentkb-p7-2b-front`，没有写入真实 `%USERPROFILE%\.claude`、`%USERPROFILE%\.codex` 或真实 skill 目录。

1. `pnpm tsc --build .`：通过。
2. `pnpm vitest run`：146 个文件、1575 个测试通过。
3. `pnpm lint`：通过。
4. `pnpm run i18n:validate`：5 种语言各 1990 个键，验证通过；已有 provider 名称未翻译警告仍为 warning-only。
5. `$env:CCHV_TEST_HOME=$env:TEMP\agentkb-p7-2b-tests-full; cargo test --manifest-path src-tauri/Cargo.toml -j 1 -- --test-threads=1`：库 1438 通过、0 失败、6 忽略；blocking_offload 6、kimi 5、scan_all 4（6 忽略）、session_count 3 全部通过。
6. `cargo clippy --manifest-path src-tauri/Cargo.toml -j 1 --all-targets --all-features -- -D warnings`：通过。
7. `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`：通过。

额外检查：`git diff --check` 通过；NSIS 打包成功。第 2a 审核意见第 2 节的三条均已处理；没有保留到下一检查点的实现项。
