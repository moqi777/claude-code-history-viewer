# P7 步骤 2a 审核意见（leader）

**结论：方向对，改 3 处后合并。** Windows 上 7 项全绿，测试夹具、fake CLI、junction 检查都做了。但有 3 处为了让测试过而放松了正式代码，或改变了 Mac 的行为，要先改掉（第 1 节）。第 2 节的留到 2b。

## 0. leader 在 Mac 上复核 `16fa8db`

7 项全部通过：tsc、vitest 1575、lint、i18n、cargo test（库 1484 通过 0 失败 6 忽略，其余目标全过）、clippy、fmt。改动只在测试和平台分支里，没有动 `search.rs`、排序逻辑和前端；`rank.rs`、`chunk.rs` 只改了测试里的断言写法。

## 1. 合并前要改

1. **`fsutil::sync_file` 吞掉 `PermissionDenied`**：这会让 Windows 上真正的 fsync 失败也被当成成功，"先落盘再改名"的保证就没了。真正的原因只有一处：`mirror.rs` 的 `copy_compressed` 用 `File::open(&tmp)`（只读）再 `sync_all`，Windows 的 `FlushFileBuffers` 需要写权限。改法：那里用 `OpenOptions::new().write(true).open(&tmp)`，删掉 `sync_file`，其余地方恢复 `sync_all()`
2. **`paths::is_absolute` 放宽了正式校验**：`\data\kb` 这种没有盘符的路径，实际落在"当前盘"上，定时任务和 App 的当前盘不一定一样，不能当成绝对路径。恢复用 `Path::is_absolute`；测试里的 `/data/AgentKB` 这类路径改成按平台生成的真实绝对路径（例如临时目录下）
3. **锁的旁路信息文件改变了 Mac 的行为**：`.kb.lock.info` 等文件现在 Mac 上也会写、也会优先读。改成**只在 Windows 上**写和读，Mac 保持原样。`migrate_dir::is_runtime_lock` 里"名字含 lock、扩展名是 info"这种模糊判断，改成列出确切的文件名（`kb.lock`、`digest.lock`、`vectors/embed.lock` 和对应的 `.info`）。注意：以前 Mac 换数据目录时会拷贝锁文件，跳过它们没问题，但测试里要有一条覆盖

另外补一条测试：`safe_join` 遇到 junction 返回错误（设计审核第 1 节第 7 条要求 `discover` 和 `safe_join` 都覆盖，现在只有 `discover`）。

## 2. 留到 2b（现在不用改，2b 报告里写怎么处理的）

1. **`lock.rs` 用 `tasklist` 判断进程是否还在**：从图形界面程序里起 `tasklist` 会闪一下黑窗口，而且解析文字输出不可靠。2b 放到 `kb/platform/` 里，用 `OpenProcess` + `GetExitCodeProcess`，起任何子进程都加 `CREATE_NO_WINDOW`
2. **`schedule::classify_exe`**：现在任何以 `AgentKB.exe` 结尾的路径都算"稳定"，包括"下载"文件夹和 U 盘；正式代码里还有 `cfg!(test)` 的判断。2b 做定时任务时按安装位置（`%LOCALAPPDATA%\Programs\...`）重新写，测试用参数传路径，不要在正式代码里认测试文件名
3. 报告里写一下 Windows 上的 `rustc` / `clippy` 版本。很多 `assert!(x.is_empty())` 改成了 `assert_eq!(x.len(), 0)`，看起来是两边 clippy 版本不同引起的；两边版本最好一致（Mac 是 1.97.0）

## 3. 下一步

改完第 1 节，在 Windows 上重跑 7 项，追加到 `P7-2a-report.md`，推送到 `origin/agent-kb-win` 后停下。leader 复核后合并，然后开始 2b。
