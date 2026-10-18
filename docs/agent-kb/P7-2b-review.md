# P7 步骤 2b 审核意见（leader）

**结论：不通过，打回修改。** Windows 部分的大方向对（平台代码集中到 `kb/platform/`、`OpenProcess`、`CREATE_NO_WINDOW`、只在登录时运行、ort 只用 CPU），但 **Mac 被改坏了**，另外有几处 Windows 功能问题和测试被关掉。第 1 节必须改，第 2、3 节也在这次一起改。

## 1. Mac 被改坏了（leader 在 Mac 上实测 `ab9ec79`）

| # | 问题 | 实测 |
|---|---|---|
| 1 | `tauri.conf.json` 的 `bundle.targets` 从 `app` 改成了 `nsis` | Mac 上 `pnpm tauri build` 只编出可执行文件，**没有 `.app`**（`target/release/bundle` 不存在） |
| 2 | `platform::no_window` 在非 Windows 上参数没用到 | Mac 上 `cargo clippy ... -D warnings` **报错**：`unused variable: command`（`platform/mod.rs:12`） |
| 3 | skill 测试把临时目录改成 `用户 with space/.claude`，但断言里还是 `.claude/skills/...` | Mac 上 `cargo test` **1 个失败**：`install_status_update_and_uninstall_touch_only_the_skill_folder` |

改法：
1. `tauri.conf.json` 保持 `["app"]`；Windows 的设置放进 `src-tauri/tauri.windows.conf.json`（Tauri 会按平台自动合并），`targets: ["nsis"]` 和 NSIS 的选项都写在那里。`tauriConfig.test.ts` 改回 `['app']`，另加一条检查 Windows 配置文件
2. `no_window` 在非 Windows 上和 `detached` 一样写 `let _ = command;`
3. 断言改成和临时目录一致

**以后改到公共文件时，要想着 Mac**：Windows 上跑不了 Mac 的检查，所以每处 `cfg(windows)` 都要看一眼非 Windows 那一支有没有落下东西，公共配置文件不要直接改成 Windows 的值。Mac 上的 7 项和打包由 leader 跑。

## 2. Windows 功能问题

1. **移动硬盘列不出来**：`list_external` 只认 `DRIVE_REMOVABLE`。U 盘是 removable，但**大多数 USB 移动硬盘和移动固态硬盘报告的是 `DRIVE_FIXED`**，现在既列不出来，还会被标成 `internal`。改成按总线类型判断（`IOCTL_STORAGE_QUERY_PROPERTY` 拿 `BusType`，USB / 1394 / SD / MMC 算外接），系统盘（`%SystemDrive%`）始终排除。加单元测试覆盖判断函数
2. **中文 Windows 上删除定时任务会报错**：`windows_uninstall` 靠错误信息里有没有 `cannot find` 判断"本来就没装"，中文系统的提示是中文。改成先 `/Query`，没装就直接返回状态，不看提示文字
3. **改了收集间隔，定时任务不会跟着改**：`windows_ensure` 只看"装没装"，不看程序路径和间隔。Mac 走的是 `Scheduler::ensure_current`，会比较这两项并重装。Windows 也要比较（`/Query /XML` 读回来比 `Command` 和 `Interval`），不一致就重装；状态里 `program`、`interval_secs`、`program_exists` 也要填上，设置页靠它们显示
4. **任务 XML 的编码**：`schtasks /XML` 对 UTF-8 文件的处理在不同系统上不一致，用户名有中文时路径可能读坏。写成 **UTF-16 LE 带 BOM**，XML 声明写 `encoding="UTF-16"`。`<Principal>` 加 `<UserId>`（当前用户），`LogonTrigger` 也加 `<UserId>`，只在这个用户登录时触发。`Repetition` 加 `<Duration>P1D</Duration>`
5. **找命令**：`PATH` 里不要再找没有扩展名的 `claude` / `codex`。npm 在 Windows 上装的那个是 sh 脚本，`CreateProcess` 起不来
6. **`MacQuery` 在 Windows 上做成指向 `WindowsQuery` 的别名和同名常量**：改成一个中性的名字（例如 `SystemVolumes`），两个平台各自实现

## 3. 测试被关掉了，要恢复

- `skill.rs` 的 3 个测试（只碰 skill 目录、旧版本提示更新、卸载不删多余文件）在 Windows 上被 `cfg(not(windows))` 关了，换成了一个更弱的。要恢复成两边都跑，Windows 上用 `test_exe` 提供路径
- `schedule.rs` 整个测试模块在 Windows 上被关了（`cfg(all(test, not(windows)))`）。Mac 的 launchd 测试可以只在 Mac 上跑，但 `classify_exe`、比较程序路径和间隔决定要不要重装的那部分逻辑，Windows 上也要有测试
- 2a 时 Windows 上库测试 1447 个，这次 1438 个。报告里说明少了哪些、为什么

## 4. skill 真机验证（设计审核第 2 节第 1 条）

报告里写了"留给用户真机确认"。用户确认之前，你先在 Windows 上做这一步，不需要真的装 skill：

1. 在临时 `CCHV_TEST_HOME` 里收集几条带中文的假会话
2. 把 skill 模板里的搜索命令**原样**分别在 **Git Bash**（Claude Code 用它跑命令）和 **PowerShell**（Codex 在 Windows 上默认用它）里跑一遍，程序放在路径带空格和中文的目录里
3. 贴出两边的完整输出，确认 JSON 完整、中文不乱码

**注意 PowerShell**：`"C:\...\AgentKB.exe" --kb-search x` 在 PowerShell 里是语法错误，要写成 `& "C:\...\AgentKB.exe" --kb-search x`。Codex 用的 skill 模板很可能要按 shell 分开写，你实测后给方案。

另外报告里写清楚：定时收集触发后在后台起的整理、建索引进程，收集进程退出后有没有被任务计划程序一起结束。用一个临时名字注册测试任务，跑一次，看完删掉。

## 5. 下一步

改完第 1–4 节，两边 7 项（Mac 至少能编译），重新打 NSIS 包，写在 `P7-2b-report.md` 后面，推送到 `origin/agent-kb-win` 后停下。版本号 0.7.0 不变。
