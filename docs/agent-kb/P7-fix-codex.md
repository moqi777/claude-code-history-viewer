# P7 真机第 6 项修复：Codex / PowerShell 没有输出

## 1. 基线和复现

- 2026-10-18，在 `agent-kb-win` 执行 `git fetch origin`、`git merge origin/agent-kb`，快进到 `31f5f789`，再开始修复。
- 当前 Codex app-server 为 0.163.0。通过本轮命令工具内的进程父子关系确认：`codex.exe → pwsh.exe -Command ...`，PowerShell 7.6.6，前置命令只设置 UTF-8 输出编码。
- **直接用本轮 Codex 的 `exec_command` 原样执行**：

  ```powershell
  & 'D:\AgentKB\AgentKB.exe' --kb-search "go-ios iPhone"
  ```

  退出码 0，输出 **0 字节**。这一条是按审核要求对真实记录的只读查询；后续全部测试使用 D 盘假数据。
- 又编译了两个只有“等待 300 ms、打印一行”的临时 Rust 程序，只改 PE 子系统。用同一个 Codex 工具执行：GUI 版没有输出；console 版输出 `console-probe-ok`。因此不涉及搜索算法、技能权限、中文编码或安装目录。

### 为什么旧离线测试漏掉了

`AgentKB.exe` 的 release 入口指定 `windows_subsystem = "windows"`。PowerShell 对没有管道或重定向的 GUI 命令不等待结束，而控制台命令会等待并捕获输出。Codex 以 shell 的生命周期收集结果；GUI 命令完成时，shell 可能已经返回。

旧脚本使用 `subprocess.run(pwsh, capture_output=True)`：Python 的 `communicate()` 会继续等继承输出句柄的 GUI 子进程关闭管道，掩盖了 shell 提前退出。之前“PowerShell 自测通过”的结论范围不足，本次补上这个条件。

本机查到的进程关系、两种子系统对照、旧正式程序和新回归脚本是主要证据。上游实现参考：

- [PowerShell 7.6.6 NativeCommandProcessor](https://github.com/PowerShell/PowerShell/blob/v7.6.6/src/System.Management.Automation/engine/NativeCommandProcessor.cs)：`_isRunningInBackground = isWindowsApplication`；`CalculateIORedirection` 对 console application 分支处理输出。
- [Microsoft：Managing Processes in PowerShell](https://devblogs.microsoft.com/powershell/managing-processes-in-powershell/)：GUI 程序处于管道末尾且无重定向时在后台运行。
- [Codex 0.163.0 pipe runner](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/utils/pty/src/pipe.rs)：命令输出读流和 shell 退出通知分开处理。本报告不把离线脚本称为 Codex 内部实现的完整复制，实际 Codex 工具复测另列。

## 2. 修复方案

1. Windows 包新增 **`agentkb-cli.exe`，console 子系统**。直接调用现有 `kb::cli::try_run`，复用搜索、查看、JSON 和错误处理；不拼 shell、不用 `cmd /c`、不依赖管道，不复制查询实现。
2. 控制台入口只接受技能需要的 `--kb-search`、`--kb-show`、`--kb-skill-print`；其他参数向 stderr 输出用法并返回 2，不启动 GUI 或执行收集。原来的 GUI 和后台任务入口保持原样。
3. 两份 Windows 技能都改用同目录的 console 程序；Claude 仍用 Git Bash 引号，Codex 仍用 PowerShell `&`。Windows 模板升至 **6**，既有模板 5 显示待更新。安装技能前检查 companion 存在，缺失则报修复安装提示，保留旧技能。
4. `status` 比较 console 路径，技能测试未安装时的默认程序也改为 console。修正 Windows 模板里原来残留的 Mac `mdfind` 找程序提示。
5. `windows-cli` feature 仅由 `tauri.windows.conf.json` 启用，Cargo 声明二进制 `required-features`，入口放在 `windows/cli.rs`，不放进 Tauri 自动扫描的 `src/bin`。Tauri 的现有 binaries 列表负责安装、卸载；不再改 NSIS 模板。
6. Mac 模板保持 **5**、正文保持原样、仍调用 `.app/Contents/MacOS/AgentKB`；基础 bundle target 仍为 `app`。额外对比 `31f5f789` 与当前源码：展开 non-Windows 提示变量后，完整模板格式字符串逐字节一致（SHA-256 `1d307b69ef44e38e534c914ea82d80e8f85333adadbb489487801b505383945a`）。这是源码层面的核对，本机未声称完成 Mac 构建，交 leader 复核。

选择单独 console 入口，是为了让 shell 按正常控制台进程处理等待、stdout、stderr、退出码。模板里加 `| Out-String`、`Start-Process` 或 `cmd /c` 会继续依赖 shell 包装，也改变现有预授权命令前缀。

## 3. 回归与真机验证

### 3.1 修之前必须失败的离线用例

```powershell
python scripts/p7_windows_smoke.py D:\AgentKB\AgentKB.exe --legacy-gui
```

结果：旧程序收集 3 条临时会话成功；Git Bash 搜索成功；Codex 模板命令失败：

```text
AssertionError: codex: no output at shell exit (GUI subsystem regression)
```

新脚本在 shell 退出后只做有界的输出收尾，不再把等待 GUI 子进程的迟到输出当成功。查询时对临时数据库短暂加锁 2 秒，模拟收集写入期间的查询延迟，使旧 GUI 的提前退出稳定暴露。迟到输出只为释放测试句柄而排空。原始日志：`D:\AgentKB-build\codex-fix-before.log`。

### 3.2 修之后和实际代理

正式 release 上运行同一脚本（不带 `--legacy-gui`），通过：

- PE 头检查：`AgentKB.exe` 是 GUI（2），`agentkb-cli.exe` 是 console（3）。
- Git Bash、PowerShell 7.6.6、cmd 直接调用 console：数据库加锁期间会等待；返回相同、完整的 UTF-8 JSON，匹配 3 条假会话。
- 两份模板的 `--kb-show` 原样执行也通过；无效 CLI 参数在 stderr 返回用法，退出码 2。
- 用 Windows PowerShell 5.1 重跑整份脚本也通过。
- 所有假源文件哈希不变。临时定时收集退出后，后台 fake AI 仍完成 1 条笔记；`.cmd` 整理测试通过，未下载语义模型；临时任务已删除。
- 日志：`D:\AgentKB-build\codex-fix-after.log`、`codex-fix-ps51.log`。

**当前 Codex 真实工具复测**（不是嵌套 Python/PowerShell 捕获）：在工具进程里设置临时 `CCHV_TEST_HOME` 后，直接执行模板命令：

```powershell
& 'D:\AgentKB-build\tmp\agentkb-p7-用户 with space-hw6bx597\Local AppData\Programs\AgentKB\agentkb-cli.exe' --kb-search "数据库"
```

退出码 0，工具直接拿到完整 JSON，包含 `"matched_sessions":3`、`"returned":3`、`"query":"数据库"`，以及 `"snippet":"使用 UTF-8 编码，检查数据库搜索结果。"`。没有管道、输出文件重定向或 `cmd /c`。

**真实 Claude Code 2.1.296：通过。** 在临时项目中用 `--print --no-session-persistence` 启动真实 CLI，允许 Read 和模板的只读 Bash 搜索前缀；`--permission-mode dontAsk`，没有 bypassPermissions。显式关闭 hooks 和 MCP，不读/复制凭据文件。CLI 先 Read 临时项目的模板 6，再调用 Bash **一次**，实际 tool_use 为：

```text
'D:/AgentKB-build/tmp/agentkb-p7-用户 with space-hw6bx597/Local AppData/Programs/AgentKB/agentkb-cli.exe' --kb-search "数据库"
```

工具结果 `is_error:false`，返回完整 JSON、`matched_sessions:3`；最终结果 `is_error:false`、`num_turns:3`，确认中文输出完整。CLI 退出码 0。完整工具调用及结果存于 `D:\AgentKB-build\codex-fix-claude-live.jsonl`，摘录在 `codex-fix-claude-live-summary.log`。

首次请求在执行技能前遇到 `API Error: Connection dropped (ECONNRESET)`，失败记录另存 `codex-fix-claude-live-attempt1.jsonl`，没有把它算通过；第二次让 CLI 正常加载原有用户连接配置（`--setting-sources user,project`），仍显式关闭 hooks、MCP 和会话持久化，完成上述验证。未修改用户连接设置。

## 4. 七项检查

| 检查 | 结果 |
|---|---|
| `pnpm tsc --build .` | 通过 |
| `pnpm vitest run` | 146 文件、1576 测试通过；包括 Windows 打包 feature 只在平台配置启用的检查 |
| `pnpm lint` | 通过 |
| `pnpm run i18n:validate` | 通过；已有 provider 名称提示不影响结果 |
| `cargo test --manifest-path src-tauri/Cargo.toml -j 1 --features windows-cli -- --test-threads=1` | 库 1456 通过、0 失败、8 忽略；集成 6 + 5 + 4 + 3 通过，已有 6 个真实数据集成用例继续忽略；doc tests 通过 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml -j 1 --all-targets --all-features -- -D warnings` | 通过 |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | 通过 |

另已通过 `pnpm build`。日志存放于 `D:\AgentKB-build\codex-fix-*.log`。初次 Clippy 指出新入口的 `single_match_else`，改为 `if let` 后全过，没有忽略该 lint。库测试比上一版多 1 个，新增用例覆盖旧 GUI 模板待更新、console 缺失时不覆盖技能、恢复文件后正常更新。

## 5. 安装包与交付

在 D 盘运行 `pnpm tauri build --config D:\AgentKB-build\package-config.json`。覆盖配置仅将 `beforeBuildCommand` 置空（机器没有 `just`，已先独立完成 `pnpm build`）；没有强制 `--bundles`，Windows 配置自动选 NSIS 并开启 `windows-cli`。release 编译 27m49s，NSIS 打包成功。

- 新安装包：**`D:\AgentKB-artifacts\AgentKB_1.32.0_x64-setup-codex-fix.exe`**
- 大小：**25,687,502 字节**
- SHA-256：**`EF0F905680AF517CF760F4F2D0AA946D7E4A596136D0298FF08D9BBB168FCA6C`**
- 原构建输出：`D:\AgentKB-build\target\release\bundle\nsis\AgentKB_1.32.0_x64-setup.exe`
- 生成的 NSIS 脚本已核对：`File /a /oname=agentkb-cli.exe` 从 release 目录安装 companion；卸载段只 `Delete "$INSTDIR\agentkb-cli.exe"`，复用原有用户级安装流程。
- companion 大小 31,442,944 字节；离线脚本测试的就是此次 release 的两个 EXE。
- 当前已安装的旧程序和用户现有技能未被替换，新包待审核。

升级后需要在 AgentKB 设置里对 Claude Code、Codex 两份技能点“更新”，使它们指向新 console 程序。没有自动覆盖用户当前安装或真实技能文件。

所有构建产物、假会话、测试 HOME、测试日志位于 D 盘。未读取密钥或凭据文件，没有对真实会话执行迁移、删除、测试收集，也没有主动再次运行真实每日整理。真实 Claude Code 验证只查询临时假数据，不开启会话持久化。

收尾复查：临时 `AgentKB-P7-real-*` / `AgentKB-P7-probe-*` 任务为 0；交付包与构建包 SHA-256 一致。C 盘剩余约 18.86 GiB。

自动安全审核以 `blocked by policy` 拒绝了两个临时目录的删除，没有给出更细原因，因此没有继续尝试删除。以下目录保留在 D 盘（只含本轮测试程序、假数据和临时 Claude 测试日志），不影响用户安装和任务：

- `D:\AgentKB-build\tmp\agentkb-p7-用户 with space-hw6bx597`
- `D:\AgentKB-build\tmp\p7-console-repro-_aqzylhe`

## 6. 2026-10-18 补充自主验收

用户要求在 leader 离线期间先完成本机可独立执行的验证。本节测试基于 `372ea213` 的正式 release EXE；仅增加验收脚本和文档，没有修改产品代码，安装包沿用第 5 节的 SHA-256。

### 6.1 真实模型下载、CPU 推理、口语化查询

新增 Windows 专用、默认忽略的集成验收 `src-tauri/tests/p7_windows_semantic.rs`。只在明确传入正式 EXE、执行 `--ignored` 时下载模型，普通测试不会下载。所有文件放在 D 盘 `tempfile` 目录，中文及空格路径；三个会话分别描述电池耗电、番茄炒蛋、数据库主键冲突。

```powershell
$env:TEMP = 'D:\AgentKB-build\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'D:\AgentKB-build\target'
$env:AGENTKB_ACCEPTANCE_EXE = 'D:\AgentKB-build\target\release\AgentKB.exe'
cargo test --manifest-path src-tauri/Cargo.toml -j 1 --features windows-cli --test p7_windows_semantic -- --ignored --nocapture
```

- 调用设置页使用的同一个 `model::download`，从 ModelScope 实际下载 **585,562,194 字节**，按内置长度和 SHA-256 校验两个模型文件，状态 `Missing → Ready`。下载部分直接调用库函数，没有声称点击过 GUI 下载按钮。
- 正式 `AgentKB.exe --kb-embed --test` 返回 `ok:true`、`dim:1024`；一次测量 **2.64 秒**。包含中文/英文相关句相似度对照，不只是文件存在检查。
- 正式 `--kb-embed --all` 生成 **6 个向量**，状态 idle、done=total=6。
- 正式 `agentkb-cli.exe --kb-search "为什么充满了没多久就得接电源" --verbose` 返回 `semantic.state=used`，首条为电池问题，`match=semantic`、`hit_messages=0`、`semantic_score=0.738`，证明来自语义索引。
- 假源文件逐字节不变。测试显式限定 `CCHV_TEST_HOME`、`AGENTKB_DATA_DIR` 和用户目录，避免继承外部数据目录覆盖。模型和临时数据由测试结束时删除。
- 首次脚本把 JSON 字段名写成 Rust 内部字段 `match_kind`，而序列化后的名称是 `match`，导致断言失败；修正验收脚本后通过。首次运行产品已正确完成下载、推理与排名，未把脚本失败算通过。失败日志 `p7-extra-semantic.log`，通过日志 `p7-extra-semantic-pass.log`；最终版本日志 `D:\AgentKB-build\p7-extra-semantic-final.log`。

这补齐了第 7 项的 Windows 后端真实模型链路；用户已安装应用中的设置页点击和真实数据搜索仍待确认。

### 6.2 Windows 定时器自行触发，而非手工 /Run

`scripts/p7_windows_smoke.py` 新增 `--timer`：沿用生产 XML 的 CalendarTrigger/Repetition，设置临时任务首次 15 秒后触发、每 60 秒重复；不调用 `schtasks /Run`。任务使用唯一 `AgentKB-P7-real-<UUID>` 名称，只执行显式携带临时环境的 wrapper。

```powershell
$env:TEMP = 'D:\AgentKB-build\tmp'
$env:TMP = $env:TEMP
python -u scripts/p7_windows_smoke.py D:\AgentKB-build\target\release\AgentKB.exe --timer
```

结果：两次自动触发退出码均为 **0**；第一次收集先退出，慢速 fake AI 在后台完成 **1 条笔记**；第二次收集后笔记仍为 **1 条**，没有重做已完成的当日轮次。两次语义后台任务均在模型缺失时进入 `paused_model`，没有偷偷下载。三种 shell 的搜索、两份模板的 show、错误退出码、源文件哈希检查一并通过。任务和本轮临时目录自动清理。

日志：`D:\AgentKB-build\p7-extra-timer.log`；最终版本 `p7-extra-timer-final.log`。这是当前登录会话内的真实计时触发，不等同于隔天或重启验收。

### 6.3 安装包、正式任务及剩余项目

- 重新计算交付包 SHA-256，与第 5 节一致。核对 NSIS 安装/卸载路径逻辑：console 随包安装并卸载，更新流程保留应用数据；本轮没有运行安装器覆盖用户安装，不能算安装升级实测。当前 `D:\AgentKB` 仍是旧版，尚无 console 程序。
- 只读检查正式任务：2026-10-18 **21:20:20** 最近运行返回 **0**；下一次 21:25:25；Interactive、Limited、StartWhenAvailable=true，不限制交流电，命令仍为 `D:\AgentKB\AgentKB.exe --kb-sync --scheduled`。
- 本机只有一块 NVMe 磁盘，C/D/E 均为内部 NTFS 分区，没有 USB/移动硬盘，不能以 D/E 分区代替第 8 项。
- 本轮未操作真实 Claude/Codex 记录、技能或凭据，未重启/注销电脑，未再次调用真实 AI 整理。

待完成：leader/Mac 复核；安装新版并更新两份技能后的 GUI/代理验收；真实移动硬盘；隔天自动整理；重启登录后自动恢复。七项原始检查结果见第 4 节，本轮未改变产品实现，不重复全套构建及打包。新增 Rust 验收另跑 Clippy/fmt，提交仍使用正常检查钩子。

最终版本实测：真实模型用例 **1 passed / 0 failed，41.17 秒**（模型测试 2.63 秒）；timer 用例通过，临时任务 `AgentKB-P7-real-6f084a15-9405-43fc-b0d9-2cbb5221cee2` 已删除；本轮语义模型目录、两轮 timer 目录均不存在，临时任务剩余 **0**。`cargo fmt --all -- --check`、新增用例的 `cargo clippy ... -- -D warnings`、`git diff --check` 均通过；Clippy 首次提示文档中的环境变量缺少反引号，修正后无警告。日志 `p7-extra-clippy-final.log`。C 盘剩余 **18.85 GiB**，所有新增构建产物在 D 盘。

## 7. 2026-10-18 安装版升级与后续验收准备

用户进一步要求继续完成可执行工作。本节更新第 5/6 节中“尚未覆盖安装”的历史状态。

### 7.1 实际升级及正式技能更新

- 23:13 左右在原目录 **`D:\AgentKB`** 执行第 5 节安装包，参数 `/S /UPDATE /NS /D=D:\AgentKB`，安装器退出 **0**，无需提权。安装前只暂时停用正式任务并关闭闲置 GUI，finally 恢复任务原启用状态。
- 旧 `AgentKB.exe`、卸载程序、两份旧 skill 和任务 XML 备份在 `D:\AgentKB-build\upgrade-backup-20261018`。备份不含会话或凭据。
- 注册表 InstallLocation 仍为 `D:\AgentKB`；安装后 GUI 为 56,249,856 字节，console 为 31,442,944 字节。
- GUI SHA-256：`242FCC837016B127F690CBEB562E1F07714E836D5402419860119E365AA7DEAB`。
- Console SHA-256：`E39570C70A807BEA460D104C8DAB3100CEE40406C9296577621AE34F8EA2852E`，与构建产物完全一致。
- 最初直接比较 GUI 与未打包 release 的哈希不一致，使验证脚本提前报错；安装器本身已成功。逐字节检查发现仅 3 个字节不同，即 Tauri 的 `__TAURI_BUNDLE_TYPE_VAR_UNK → __TAURI_BUNDLE_TYPE_VAR_NSS`。本地 `tauri-utils 2.9.2/src/platform.rs` 明确解释该标记由打包器修改；应用这一预期替换后全部字节一致。没有再次安装或替换二进制。
- AgentKB 配置仍为 847 字节，修改时间仍为 16:14:29；应用界面偏好文件仍为 25 字节，修改时间仍为 13:10:11。升级后的配置哈希 `2DE5B3B4C4B4E82FC0F93634BBD9F982453949FB340D1B86732FA8888A2E4B6F` 在后续操作中保持不变。
- 两份正式技能使用安装后的 console `--kb-skill-print` 生成模板 **6**；确认原文件与升级前备份一致且有 AgentKB marker 后原子替换，仅修改 `.claude/skills/agentkb/SKILL.md` 和 `.codex/skills/agentkb/SKILL.md`。当前均指向 **`D:\AgentKB\agentkb-cli.exe`**。这是程序生成模板的安装，不声称点击过设置页“更新”。

### 7.2 安装目录中的程序实测

- 本轮 Codex 的实际命令工具直接执行 `& 'D:\AgentKB\agentkb-cli.exe' --kb-search '数据库'`，在临时 `CCHV_TEST_HOME`/`AGENTKB_DATA_DIR` 下收到完整 JSON、中文、matched_sessions=1；无管道、cmd 包装或重定向。
- Git Bash 执行正式 Claude 模板中的 `'D:/AgentKB/agentkb-cli.exe' --kb-search '数据库'`，返回相同结果。本轮未再次调用付费 AI。
- 将 `AGENTKB_ACCEPTANCE_EXE` 改为 **`D:\AgentKB\AgentKB.exe`** 后重跑真实模型集成用例：**1 passed / 0 failed，35.23 秒**。实际下载并校验模型，安装版 CPU 测试 **2.59 秒**，6 个向量及口语查询排名均通过，临时模型随测试清理。日志：`D:\AgentKB-build\p7-installed-semantic.log`。
- 正式任务在升级后的 **23:25:25** 自动运行返回 **0**；仍每 5 分钟执行安装目录中的 GUI 后台入口。正式应用已恢复启动。
- 尝试用隔离 HOME 和独立 D 盘 WebView profile 打开安装版验证界面，发现 Windows 正处于锁屏（同会话 LogonUI 存在，截图为锁屏，无法取得可操作的设置控件）。因此 GUI 点击下载、测试、开关、技能更新流程仍未验收，不能用后端测试替代。

### 7.3 已安排真实跨日及重启后的检查

新增 `scripts/p7_windows_deferred.py`。`prepare <installed AgentKB.exe>` 在临时目录建立假会话、fake AI、独立配置，先验证当天生成 1 条笔记，再加入截点之后的第二条会话。仅使用用户级 InteractiveToken/LeastPrivilege 任务，不使用真实 AI、不改系统时钟、不主动重启或注销。

本机准备目录：**`D:\AgentKB-build\tmp\agentkb-p7-deferred-zksf03k5`**。runner 已复制到该目录，后续切换仓库不会改变任务执行的脚本。

| 任务 | 触发条件 | 实际检查 |
|---|---|---|
| `AgentKB-P7-deferred-881b8bf69eed4c5a9648c4bde684f867-midnight` | 2026-10-19 **00:01**，错过后 StartWhenAvailable | 正式安装版定时收集后，真实时钟进入新日期，假数据产生第二个 daily round 和第 2 条笔记 |
| `AgentKB-P7-deferred-881b8bf69eed4c5a9648c4bde684f867-logon` | 当前用户下次登录，延迟 2 分钟 | LastBootUpTime 必须变化；对正式任务只读检查本次启动后的成功运行，再做隔离收集，源哈希不变 |

若只是注销后登录但没有重启，logon 验收记录 pending 并继续等真正重启。任务完成检查后删除自身；均设置 7 天有效期和过期后自动删除。程序、假数据、结果保留在 D 盘供复核。不会创建新的常驻服务。

准备过程当天基线通过；两个 runner 的提前调用都返回 **pending**、未误执行或移除任务；`pythonw.exe` 的实际入口验证成功。这里仍未将跨日/重启项目标为通过。后续读取：

- `...\midnight-result.json`
- `...\logon-result.json`
- `...\state.json`（基线和触发信息）

### 7.4 新发现和清理边界

带 `+08:00` 的假消息暴露了每日整理公共候选筛选的时间戳字符串比较问题。最小复现、影响与建议见 [P7-timezone-findings.md](P7-timezone-findings.md)。原验收第 3 节要求只改 Windows，因此没有顺手修改 Mac/Windows 共用逻辑；跨日自动验收采用正常 UTC `Z` 假记录，不能视为该问题已解决。

本轮两个目录的清理由自动安全审核以 `blocked by policy` 拒绝，未给更细理由，没有换方式重试。保留：

- `D:\AgentKB-build\tmp\agentkb-installed-ui-用户 d7ifcmly`：隔离 GUI profile、假数据。
- `D:\AgentKB-build\tmp\agentkb-p7-deferred-xttn5goq`：时区问题的失败复现、fake AI、假数据；未注册任何任务。

此外，升级备份、工具依赖、一次性任务证据都在 D 盘。测试使用临时假会话，未对真实 Claude/Codex 记录执行测试写入或删除，未读取凭据文件；正式技能更新是本轮实际安装工作，正式收集任务按原配置继续运行。新增脚本语法检查和 `git diff --check` 通过；没有产品代码变化，不重新打包。
