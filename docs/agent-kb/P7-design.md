# P7 Windows 版调研与设计

> 分支：`agent-kb-win`，基线：`origin/agent-kb`（2026-10-17）。本文件是步骤 0 和步骤 1 的产出；代码实现等审核通过后进行。真实的 Claude Code/Codex 目录只做了路径存在性检查，没有读取会话、配置内容或凭据。

## 1. 步骤 0 结果

### 1.1 工具链与系统依赖

| 工具 | 结果 |
|---|---|
| Windows | 当前 Windows x64；未改变系统版本设置 |
| Node | `v24.16.0`，来自 `C:\\nvm4w\\nodejs` |
| pnpm | `10.13.1`，通过 Corepack 启用 |
| Rust | `rustc 1.99.1 (58cf8f9c3 2026-10-13)`，`stable-x86_64-pc-windows-msvc` |
| Cargo | `cargo 1.99.1 (5f94df478 2026-08-27)` |
| Just | `1.59.0`，步骤 0 中通过 WinGet 安装 |
| Visual Studio | Visual Studio Build Tools `18.10.2`，安装目录 `C:\\Program Files (x86)\\Microsoft Visual Studio\\18\\BuildTools`，vswhere 报告完整安装 |
| WebView2 | `C:\\Program Files (x86)\\Microsoft\\EdgeWebView\\Application` 存在 `155.0.4283.60` |

`pnpm install --frozen-lockfile` 成功。Cargo 首次下载依赖时出现两次 Schannel TLS 重试，但依赖下载和编译最终完成；这是构建依赖下载，不读取本机用户数据。

### 1.2 PLAN 第 6 节的 7 项检查

所有命令均在临时目录隔离的环境中运行：`USERPROFILE`、`HOME`、`APPDATA`、`LOCALAPPDATA`、`TEMP`、`TMP` 指向 `%TEMP%\\agentkb-p7-step0` 下的目录；Rust 的工具链缓存仍使用已安装的 MSVC toolchain。没有把真实 `C:\\Users\\moqing\\.claude` 或 `.codex` 设置成测试源。

| # | 命令 | 结果与原始输出摘要 |
|---:|---|---|
| 1 | `pnpm tsc --build .` | **通过，exit 0**，无输出。 |
| 2 | `pnpm vitest run` | **通过，exit 0**：146 个测试文件、1574 个测试全部通过。jsdom 打印了两次 `Error: Not implemented: navigation (except hash changes)`，没有使测试失败。总时长 110.15 秒。 |
| 3 | `pnpm lint` | **通过，exit 0**：运行 `eslint src`，无诊断。 |
| 4 | `pnpm run i18n:validate` | **通过，exit 0**：5 种语言各 1990 个键、无重复键；ja/ko/zh-CN/zh-TW 各报告 35 个“未翻译疑似” provider 名称，但脚本仍报告 `✅ 모든 검증 통과!`。 |
| 5 | `cd src-tauri; cargo test -- --test-threads=1` | **失败，exit 101**：1441 项中 1376 通过、59 失败、6 忽略，测试执行 52.83 秒。失败按原因见下。 |
| 6 | `cd src-tauri; cargo clippy --all-targets --all-features -- -D warnings` | **失败，exit 101**。第一次因 `webui-server` 的 `RustEmbed` 找不到 `../dist`；先运行 `pnpm build` 生成 dist 后再次运行，基线代码产生 23 个 clippy error（8 个 lib error，15 个 lib-test error），包括 `TERMINATED` dead code、`map_or_identity`、6 个 `needless_borrows_for_generic_args`，以及测试中的 `assert_empty`/`assert_is_empty`。 |
| 7 | `cd src-tauri; cargo fmt --all -- --check` | **通过，exit 0**，无格式差异。 |

`pnpm build` 作为 clippy/打包前置步骤也通过，Vite 报告已有的动态导入和大 chunk 警告，exit 0。

### 1.3 Rust 失败测试的原因分组

失败都来自基线代码在 Windows 的平台假设或测试夹具，未修改源代码来“压过”失败。

1. **POSIX 路径和分隔符假设**：`kb::config` 的夹具把 `/data/kb`、`/x` 当作 Windows 绝对路径，导致 `data_dir must be an absolute path` 或 `exclude path ... must be absolute`；`kb::sync`、`kb::schedule`、`kb::paths` 的断言把 `/` 与 Windows 的 `\\` 混在同一个字符串中；`the_digest_work_directory_is_always_excluded_from_the_index` 也断言了 POSIX 路径。涉及 `kb/config.rs`、`kb/sync.rs`、`kb/schedule.rs`、`kb/paths.rs` 的测试。
2. **Unix shell 测试夹具不能在 Windows 执行**：digest 测试生成并执行 `fake-ai.sh`，Windows 返回 `%1 不是有效的 Win32 应用程序` 或找不到文件，所以大部分 `kb::digest::tests` 在真正进入 digest 逻辑前就报告 `config` 失败。涉及 `kb/digest/testutil.rs` 和 `kb/digest/engine.rs::spawn_and_wait`。
3. **Windows 文件锁和 fsync 语义**：镜像测试在临时文件上报告 `fsync ... 拒绝访问 (os error 5)`；迁移测试复制仍被打开的 `embed.lock` 时报告 `os error 33`。涉及 `kb/fsutil.rs::fsync_dir`、`write_atomic`、`copy_atomic`，`kb/lock.rs::SyncLock`，以及 `kb/migrate_dir.rs`、`kb/mirror.rs`。
4. **符号链接权限**：5 个 `commands::session::rename` 测试、`kb::discover` 的 symlink 测试和 `providers::cursor_agent` 的 symlink 测试收到 Windows `os error 1314`（客户端没有所需的特权）。测试没有以开发者模式或管理员权限运行。
5. **依赖真实可执行文件的测试夹具**：skill 状态测试的占位路径在 Windows 上不是可执行文件，得到 `missing_exe`/`Stale`；需要用 Windows `.exe` 或统一的 fake executable helper。涉及 `kb/skill.rs`。
6. **其余迁移/导入/digest 失败是上述夹具失败的连锁结果**：`migrate_dir`、`import_tests`、部分 `digest` 和 `sync` 测试不能据此判定业务逻辑已适配 Windows；步骤 2 将按平台 helper 修复后逐组重跑。

### 1.4 打包尝试

第一次 `pnpm tauri build` 失败：`beforeBuildCommand just frontend-build` 找不到 `just`。安装 Just 1.59.0 后，使用临时 `pnpm.cmd` wrapper 绕过本机 PowerShell 禁止运行 `pnpm.ps1` 的执行策略，再次运行同一 Tauri 打包命令成功：

```text
Finished `release` profile [optimized] target(s) in 11m 02s
Built application at: ...\\src-tauri\\target\\release\\AgentKB.exe
```

当前 `src-tauri/tauri.conf.json` 的 `bundle.targets` 是 `["app"]`，因此只得到可执行文件，没有 NSIS/MSI 安装器；`target\\release` 中同时出现 `DirectML.dll`。本次构建没有提交版本同步脚本产生的换行变化。

## 2. 这台 Windows 电脑的真实安装情况

只检查了命令解析结果、目录存在性和目录名，没有读取下面目录中的文件内容。

| 项目 | 观察结果 |
|---|---|
| Claude 命令 | `Get-Command claude -All` 找到 `C:\\nvm4w\\nodejs\\claude.cmd`、同目录的 `claude.ps1` 和无扩展名 launcher；后台调用应优先保存 `.cmd` 的绝对路径。 |
| Claude 会话 | `C:\\Users\\moqing\\.claude\\projects` 存在。 |
| Claude skill | `C:\\Users\\moqing\\.claude\\skills` 存在。 |
| Codex 命令 | `C:\\Users\\moqing\\AppData\\Local\\Programs\\OpenAI\\Codex\\bin\\codex.exe`。 |
| Codex 会话 | `C:\\Users\\moqing\\.codex\\sessions` 存在；`archived_sessions` 当前不存在，收集器应把缺失目录视为空而不是报删除。 |
| Codex skill | `C:\\Users\\moqing\\.codex\\skills` 存在。 |
| 用户配置根 | `%APPDATA%`、`%LOCALAPPDATA%` 存在；`.claude\\settings.json` 和 `.codex\\config.toml` 只做了存在性检查，没有读取。 |

`claude`/`codex` 的实际路径不能依赖后台任务的 PATH。设置页应把经过校验的绝对路径写进 `DigestConfig::cli_path`；当前配置模型和加载入口在 `kb/config.rs::DigestConfig`、`KbConfig::load`。

## 3. Windows 平台设计

### 3.1 代码组织和不变的边界

新增 `src-tauri/src/kb/platform/mod.rs`，并新增 `windows.rs`、`macos.rs`。平台模块提供小接口：`Scheduler`、`ProcessLauncher`、`CliResolver`、`VolumeQuery`、`AtomicFile` 和 `SystemResources`。共用代码通过接口调用，平台选择集中在 `#[cfg(target_os = "windows")]`/`#[cfg(target_os = "macos")]`。

预计最小接入改动：

- `kb/schedule.rs`：保留 `ScheduleStatus`、修复逻辑和 `install_now/status_now/uninstall_now` 的共用部分，把当前 `Launchctl`/plist 后端移入 macOS，把 Windows 后端接到同一接口。
- `kb/digest/engine.rs`：保留 `claude_args`、`codex_args`、解析和超时状态；`resolve_cli`、`find_via_login_shell`、`spawn_and_wait`、`kill_group` 改用平台 helper。
- `kb/digest/mod.rs::spawn_background` 和 `kb/semantic/build.rs::spawn_background`：只保留日志、参数和错误处理，进程创建交给 helper。
- `kb/semantic/sys.rs`：保留 `available_memory`/`lower_priority` 的抽象，实现 Windows 分支。
- `kb/volume.rs`：保留 `Probe`/`Presence`/`VolumeInfo` 决策，实现 Windows 查询和驱动器枚举。
- `kb/fsutil.rs`、`kb/lock.rs`：把 Windows 的替换、重命名重试和锁诊断放到平台 helper；镜像、迁移、导出继续调用共用函数。
- `kb/export.rs`：保留现有 `hard_links_ok` 降级流程，只改成使用平台能力报告和 Windows 测试。
- `src-tauri/Cargo.toml`：增加显式 Windows API 依赖（建议 `windows` 0.61 的 `Foundation`、`System::Com`、`System::TaskScheduler`、`System::Threading`、`System::Memory`、`System::JobObjects`、`Storage::FileSystem` 特性；当前锁文件已有 0.61 系列 Windows crate）。不增加联网服务或遥测。

不改 `src/kb/KbSearch.tsx`、`src/kb/KbNotes.tsx`、`kb/search.rs`、`kb/semantic/rank.rs`，这些文件没有平台原因。`AGENTKB_VERSION` 在实现全部完成并审核前保持 `0.6.3`，最后一个版本提交再升到 `0.7.0`。

### 3.2 定时收集：Windows Task Scheduler

依据：当前 Mac 后端在 `kb/schedule.rs::Scheduler::install/uninstall/status/ensure_current`，命令入口在 `kb/cli.rs::run_schedule_command`，应用启动修复在 `schedule::ensure_current_now` 和 `lib.rs::run`。

Windows 后端创建任务名 `io.github.moqi777.agentkb.sync`，动作是当前已安装的 `AgentKB.exe --kb-sync`，工作目录设为 exe 所在目录，输出重定向到配置数据目录的 `logs`。用 Task Scheduler COM API 注册任务定义：

- 时间触发器使用 `interval_secs` 的重复间隔；保留一次登录/启动后的立即触发语义。
- `StartWhenAvailable=true`，机器睡眠、关机或暂时不可运行时，恢复后补跑一次；`MultipleInstances=IgnoreNew`，由 `kb.lock` 再做最终互斥。
- 主体使用当前用户的 S4U/batch 方式，任务在用户未登录时仍可执行；不把密码写入任务。若系统策略不允许 S4U，`install` 返回可翻译的设置错误，并在设置页提示用户启用“登录时运行”或改用系统策略，不悄悄注册一个没有用户目录权限的 SYSTEM 任务。
- `install` 是幂等的：读取同名任务、比较 exe、间隔、触发器和主体；有差异就更新。`uninstall` 删除同名任务并可重复调用；`status` 返回存在、启用、运行中、上次运行结果和当前 exe 是否仍存在。
- `ensure_current_now` 在 app 启动时只为稳定安装路径修复任务；开发运行 (`cargo run`/`tauri dev`) 不接管已安装任务，沿用 `ExeStability` 的保护思想。
- 任务动作隐藏窗口，并把启动失败写进 `logs`；`--kb-sync` 仍由 `lib.rs::run` 最前面的 headless 分发执行后退出，不启动 Tauri 窗口。

需要用 Windows 临时用户配置做集成测试：创建、更新、查询、删除任务，模拟未登录主体的注册结果，并把触发间隔设为最小测试值确认睡眠恢复后的 `StartWhenAvailable` 状态。测试不得操作用户真实任务；正式真机确认由审核者在临时 profile 完成。

### 3.3 后台整理和建向量的子进程

依据：`kb/digest/engine.rs::spawn_and_wait`、`kill_group`、`kb/digest/mod.rs::spawn_background`、`kb/semantic/build.rs::spawn_background`。

- Windows 前台调用使用 `CREATE_NO_WINDOW`，不弹黑窗口；stdin/stdout/stderr 仍使用管道和 `logs` 文件。
- 整理的前台子进程放进 Job Object，设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`；取消和超时调用 `TerminateJobObject`，确保 CLI 的 helper 进程一起结束，然后等待句柄关闭。`digest.lock` 仍是跨进程状态事实来源。
- 后台 `--kb-digest`/`--kb-embed` 使用 detached process 和 `CREATE_NO_WINDOW`，不把它放进父进程退出即关闭的 kill-on-close Job Object；父进程返回后子进程继续运行并自行写日志，任务计划程序只负责拉起它。
- 所有句柄用 RAII 关闭；取消路径先终止 Job、等待进程、回收 stdout/stderr reader，再释放 `digest.lock`/`embed.lock`。不会通过 PID 猜测并杀掉无关进程。

### 3.4 找到 `claude`/`codex` 和 Windows 参数

依据：`kb/config.rs::DigestConfig` 的绝对路径字段，`kb/digest/engine.rs::resolve_cli`、`candidate_paths`、`cli_version`。

Windows 候选顺序为已保存的绝对路径、用户 npm bin（`%APPDATA%\\npm\\claude.cmd`/`codex.cmd`）、`%USERPROFILE%\\.local\\bin`、Claude 自己的 local bin、PATH 中的 `.exe`/`.cmd`，最后再使用 Windows 的 `where.exe` 仅在设置页探测，不在后台依赖 shell。只接受常规文件，保存前展示实际路径和 `--version`。

`.exe` 直接使用 `Command::new(...).args(OsString)`；不经过 shell，模型名和系统提示中的空格、引号、中文不会被重新解释。`.cmd` 通过 `cmd.exe /d /s /c` 的专用 wrapper 调用，使用单独的 Windows 命令行 quote 函数处理 `\\`、`"` 和末尾反斜杠；禁止把用户可编辑文本拼接进未转义的 shell 字符串。`.ps1` 不作为后台 CLI，设置页提示选择同目录的 `.cmd`/`.exe`。

### 3.5 文件、路径、换行和锁

依据：`kb/fsutil.rs::write_atomic/copy_atomic/append_tail/walk_files`、`kb/lock.rs::SyncLock`、`kb/paths.rs::safe_join`、`kb/config.rs::KbConfig::validate`。

- 数据库、JSON、JSONL 和笔记内容按字节读写，保留源文件的 LF/CRLF；镜像相对路径内部继续统一成 `/`，只在 `Path` 边界转换成 Windows 路径。
- `Path::canonicalize`、`eq_ignore_ascii_case` 和卷身份比较统一处理大小写不敏感；`safe_join` 继续拒绝绝对路径、驱动器前缀、`..` 和反斜杠逃逸。所有测试夹具使用真正的 `PathBuf`，不再拿 `/data/...` 当 Windows 路径。
- 对超过 MAX_PATH 的用户路径，Windows helper 在 Win32 调用前使用绝对 `\\?\\`/UNC 形式；UI 和数据库仍保存可读的普通绝对路径。逐项测试 260 字符以上的源、数据和日志路径。
- Windows 不能安全地对打开着的目标做普通 `rename`/`remove_file`，目标存在时 `std::fs::rename` 也不会替换。`write_atomic`/`copy_atomic` 改为同目录临时文件、flush、用 `ReplaceFileW`/`MoveFileExW(MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH)` 原子替换；若目标被其他程序以共享拒绝方式打开，短暂重试并报告明确错误，不先删除目标。
- `kb.lock`、`digest.lock`、`embed.lock` 继续用 `fs4` 的 `LockFileEx` 语义，锁由打开的文件句柄持有，崩溃时由系统释放；诊断 JSON 只在句柄持有期间写入。Windows 的 `process_alive` 使用 `OpenProcess(SYNCHRONIZE)`/`WaitForSingleObject`，不能把锁文件中的旧 PID 当作锁所有权。
- 关闭目标文件和数据库连接后才进行替换或迁移；迁移遇到共享冲突时等待持有者退出并给出可恢复错误。不会强删用户打开的会话文件。

### 3.6 导出 raw 的硬链接

依据：`kb/export.rs` 中 `hard_links_ok`、`std::fs::hard_link` 和锁释放逻辑。

NTFS 同一卷支持硬链接；导出 staging 与 AgentKB 数据目录不在同一卷、或目标卷是 exFAT/FAT32 时，`CreateHardLinkW`/`hard_link` 会失败。保留现在的策略：第一次失败后把所有 raw item 改回原文件，并在整个打包期间持有 `kb.lock` 和 `digest.lock`，完成后才释放；报告 `paused_collection=true`，不用破坏导出的一致性。对 NTFS 成功链接的情况，链接只存在于临时 staging，打包完成后随 staging 清理。添加 NTFS、exFAT、FAT32、跨卷和链接失败的临时盘测试。

### 3.7 移动硬盘识别

依据：`kb/volume.rs::probe`、`presence`、`same_device`、`list_external`，以及 `kb/backup.rs::ensure_on_disk/copy_all`。

Windows 版本通过 `GetLogicalDrives` 枚举盘符，用 `GetDriveTypeW` 排除网络盘和系统固定盘；用 `GetVolumeNameForVolumeMountPointW` 获取卷 GUID，用 `GetVolumeInformationW` 获取卷名、文件系统和可写状态。设置保存卷 GUID（显示卷名），不把盘符或文件夹名当作身份。`probe` 只接受可写的可移动/外置卷；网络盘返回 `Unsupported`；`presence` 比对 GUID，路径即使在系统盘上残留也返回 `NotConnected`。每个文件前继续调用 `ensure_on_disk`，在 Windows 用卷 GUID/卷序列再次确认，移动硬盘拔出时停止写入并留下可重试的 summary。

exFAT/FAT32 可以作为备份目标，但不提供 raw 导出的硬链接能力；备份本身使用现有逐文件复制、追加和冲突旁路逻辑。NTFS 与 exFAT 的路径大小写、文件名和长路径都要在测试中覆盖。

### 3.8 可用内存和降低优先级

依据：`kb/semantic/sys.rs::available_memory/lower_priority`、`kb/semantic/build.rs::run`，以及 `kb/semantic/rank.rs` 的查询内存门槛。

Windows 使用 `GlobalMemoryStatusEx` 的 `ullAvailPhys` 作为可用物理内存；未知时返回 `None`，沿用现有“不因未知内存而拒绝”的分支。向量构建在 `MIN_AVAILABLE_BYTES` 门槛下保持 postponed 语义。`lower_priority` 使用 `SetPriorityClass`：默认 `BELOW_NORMAL_PRIORITY_CLASS`，`background` 使用 `IDLE_PRIORITY_CLASS`，`none` 不改变优先级；失败只写日志，不让索引任务失败。保留 `AGENTKB_EMBED_AVAILABLE_MB` 和 `AGENTKB_PRIORITY` 测试覆盖。

### 3.9 ONNX Runtime

依据：`src-tauri/Cargo.toml` 的 `ort = "=2.0.0-rc.10"`、`kb/semantic/embed.rs::OrtEmbedder`、`kb/semantic/model.rs` 和 `kb/semantic/build.rs`。

先沿用 ort 当前 Windows 构建方式，避免复制一套 embedder。步骤 0 的 release 目录实际出现 `AgentKB.exe` 约 54 MB 和 `DirectML.dll` 约 18.5 MB，没有单独的 `onnxruntime.dll`；步骤 2 要在干净临时目录启动一次 `--kb-embed`，确认 ort 的运行时 DLL 搜索路径。若 Windows 目标需要动态 runtime，则把与当前 ort 版本匹配的 `onnxruntime.dll` 放入 NSIS 安装目录并在启动时只从安装目录加载，记录安装包增量；不在运行时联网下载。CPU provider 作为无 DirectML/不支持 GPU 的回退，语义搜索的数据库和模型目录仍遵守 `DataPaths`。

### 3.10 数据、配置和安装包

依据：`kb/config.rs::default_data_dir/config_path`、`src-tauri/src/utils.rs::home_dir/config_dir`、`kb/paths.rs::DataPaths`、`src-tauri/tauri.conf.json`。

- 默认数据目录继续是用户 home 下的 `AgentKB`，Windows 即 `%USERPROFILE%\\AgentKB`，包含 `raw`、`notes`、`kb.db`、`logs`。
- 配置固定为 `dirs::config_dir()\\agent-kb\\config.json`，Windows 解析为 `%APPDATA%\\agent-kb\\config.json`；无界面模式与 GUI 共用这个文件。测试构建继续使用 `CCHV_TEST_HOME`，运行时用临时 `USERPROFILE/APPDATA/LOCALAPPDATA`。
- P7 版本完成时把 `kb::AGENTKB_VERSION` 升为 `0.7.0`；底座 `package.json` 版本仍由项目发布流程管理，不能借 Windows 期随意改动。
- 推荐 Tauri NSIS：它适合单一 Windows x64 安装器、可把 `AgentKB.exe`、WebView2 检查和 ort runtime 放在同一安装目录，并支持卸载；MSI 作为企业部署的后续选择。把 `tauri.conf.json::bundle.targets` 从当前仅有的 `app` 改为审核后决定的 `nsis`，先在临时 Windows 用户下安装、卸载和升级验证。
- 没有代码签名时，Windows SmartScreen 可能显示“Windows 已保护你的电脑”并要求用户点“更多信息/仍要运行”；安装说明应明确这是未签名的个人构建，提供发布页哈希和来源，不把 SmartScreen 警告隐藏或绕过。正式发布是否购买代码签名证书由 leader 决定。

## 4. 测试计划

### 4.1 两个平台共用的测试

- `kb/platform` 的路径、quote、可执行文件解析、状态模型、任务定义序列化、内存阈值、文件替换错误分类用纯 Rust 单测，输入只用 `tempfile`。
- `kb/fsutil`、`kb/lock`、`kb/mirror`、`kb/export`、`kb/transfer` 的行为测试统一使用 `PathBuf` 和临时目录；不再硬编码 POSIX 绝对路径。
- `kb/digest/engine` 的 fake CLI 改为 Rust/Windows `.exe` helper；命令参数仍检查不进入 argv 的 session 文本、stdin EOF、超时、取消和退出状态。
- `cargo test -- --test-threads=1`、`cargo fmt --all -- --check`、前端 tsc/vitest/lint/i18n 继续作为两平台共同门槛。

### 4.2 Windows 专项测试

在 `#[cfg(windows)]` 集成测试中：

1. 用临时 `USERPROFILE`、`APPDATA`、`LOCALAPPDATA` 建造 `.claude/projects`、`.codex/sessions` 和 AgentKB 数据目录；写入合成 JSONL，验证 raw 字节、CRLF、追加游标和数据库索引，不接触真实源。
2. 用临时 Windows 用户/Task Scheduler 测试任务的安装、变更间隔、禁用、卸载、状态、未登录 S4U 和睡眠补跑；测试失败时清理同名临时任务。
3. 启动真正的 `AgentKB.exe --kb-sync/--kb-search/--kb-show`，确认无窗口、执行完退出、日志在临时数据目录，且 app 窗口没有被 headless 参数启动。
4. 启动 fake `.exe` 验证 Job Object 取消会结束子进程树；验证 detached digest/embed 在父进程结束后仍能写完临时日志。
5. 建 NTFS、exFAT/FAT32（可用临时 U 盘或虚拟卷）测试硬链接和复制回退；拔盘后检查没有写到同名系统文件夹。
6. 对真实目录只做操作前后元数据快照（路径、大小、修改时间、文件数量），不读取内容；确认 `C:\\Users\\moqing\\.claude`、`.codex` 没有变化。skill 测试只使用临时用户目录，绝不向真实 skill 目录安装。

### 4.3 审核后的验证顺序

1. 先修复平台 helper 和 Windows 测试夹具，重跑失败分组。
2. 在 Windows 重新跑 7 项检查并贴出命令和完整汇总；补跑临时 profile 的任务、CLI、导入导出、skill、digest、semantic 真机流程。
3. leader 在 Mac 上合并前重跑 PLAN 第 6 节全部检查并重新打包，确认 macOS launchd、`/Volumes` 和 `nice` 行为没有变化。

## 5. 风险与需要 leader 决定的事项

1. Task Scheduler 使用 S4U 运行时是否满足目标电脑的公司组策略；若被策略禁用，应明确选择“用户登录时运行”还是提供安装时的用户授权流程，不能退回 SYSTEM 访问用户目录。
2. 是否接受 Windows `.cmd` CLI wrapper 的 quote 实现，还是要求设置页只允许真实 `.exe`；当前这台电脑的 Claude 安装实际提供 `.cmd`，所以若拒绝 `.cmd`，Claude digest 将不可用。
3. 是否把 exFAT/FAT32 移动盘列为正式支持（复制备份可用，硬链接导出自动降级），以及卷 GUID/序列号变化时的迁移策略。
4. ort Windows runtime 是继续随安装包带 CPU/DirectML 文件，还是锁定一个明确的动态 DLL 包；需要一次干净机器实测后定包大小和许可证说明。
5. NSIS 作为 0.7.0 默认安装器、MSI 是否延后；是否在无代码签名的情况下发布 Windows 安装器。
6. 现有 clippy 23 项基线诊断应由 Windows 期顺手清理，还是单独开质量修复提交；这不影响平台设计，但会影响 7 项检查的“全绿”门槛。
7. Windows 无开发者模式时符号链接测试应改为 junction/复制等 Windows 等价物，还是把 symlink 安全策略测试标为需要特权的专项测试。
