# P8b Windows 验证与 NSIS 打包（AgentKB 0.8.0）

日期：2026-11-22。分支：`agent-kb-win`。先快进拉取该分支，再合并 `origin/agent-kb`，快进到 `6363595b`。**第 3 步通过。** 范围见 [P8b 任务书第 6 节](P8b-compress.md#6-步骤-3windowsleader-审完-mac-再派)、[Mac 报告](P8b-report.md)与 [leader 验收](P8b-acceptance.md)。

## 七项检查

使用 Windows x64、Rust 1.99.1、pnpm 10.13.1、MSVC 14.51 和 Windows SDK 10.0.26100.0。用仓库的 `.cargo/with-build-storage.ps1` 将 Cargo 输出和临时文件放到 `D:\AgentKB-build`；Cargo 检查使用 `-j 2`，测试串行运行。Rust 命令从仓库根目录加 `--manifest-path src-tauri/Cargo.toml`。最终 Rust 测试进程设置了 `NO_PROXY=localhost,127.0.0.1,::1`。

| 检查 | 结果 |
| --- | --- |
| `pnpm exec tsc --build .` | 通过 |
| `pnpm exec vitest run` | 149 个文件、1,596 项通过 |
| `pnpm lint` | 通过 |
| `pnpm run i18n:validate` | 通过，有疑似未翻译提示 |
| `cargo test --features windows-cli -j 2 -- --test-threads=1` | 1,530 个库测试、18 个集成测试通过，0 失败、17 个 ignored |
| `cargo clippy --all-targets --all-features -j 2 -- -D warnings` | 通过，0 警告 |
| `cargo fmt --all -- --check` | 通过 |

首轮的 13 个压缩用例失败于测试夹具用字符串比较 Windows 路径，改用 `Path` 比较后全部通过。另有三个本地 HTTP 用例首轮连接失败，单独及整套重跑均通过。Clippy 首轮标记了四处空列表断言，改用等值比较后通过。已有的 junction 发现测试在本机因 `mklink` 参数格式问题未建成夹具，按测试自身逻辑提前返回；真实文件占用测试独立于该用例。

额外的 `pnpm build` 通过，Vite 耗时 18.69 秒，有大 chunk 提示。所有检查日志在 `D:\AgentKB-build\p80\check-*.log`。

Windows 原生 `cargo check --features windows-cli` 通过。核对了 SDK 中 `GetCurrentThread`、`SetThreadPriority` 及 `THREAD_MODE_BACKGROUND_BEGIN=0x00010000`，release GUI 可执行文件的导入表也包含这两个函数。压缩在独立线程中调用该入口。本机单独的 API 探针用当前线程句柄启动和结束后台模式，两个调用均返回 1，进程优先级类别前后均为 32。证据见 `thread-priority-api.log`。

## Windows 真实文件占用

用本次 release 的 `AgentKB.exe --kb-sync` 和 `--kb-show --full` 操作 `D:\AgentKB-build\p80` 下的隔离临时库。独立 Python 子进程用 `CreateFileW` 打开指定备份，只允许其他进程读写共享，直到父进程关闭管道才释放句柄。三次探针的实际删除调用均被 Windows 拒绝，返回 **32（ERROR_SHARING_VIOLATION）**。

| 占用场景 | 占用期间的收集 | 释放后第一次收集 |
| --- | --- | --- |
| 压缩时旧 `.jsonl` 被占用 | `ok`、退出 0，压缩 1 个文件，`left_behind=1`；库指向 `.packed.zst`，仍能读到完整旧消息 | `ok`，`duplicates_removed=1`，只剩压缩备份 |
| 解包时旧 `.packed.zst` 被占用 | `ok`、退出 0，`unpacked_files=1`，明文备份与已追加的源文件逐字节一致，另一条会话也收集成功；旧压缩文件暂留 | `ok`，`duplicates_removed=1`，只剩完整明文备份 |
| 解包目标处仍有被占用的旧 `.jsonl` | `partial`、退出 1，只有该文件的 `mirror` 错误；收集正常结束，另一条新会话已入库，旧消息仍可从压缩备份读取 | `ok`、退出 0，`unpacked_files=1`；旧压缩文件已删，新旧消息均可全文读取，无 `.tmp-*` 或 `.prev-*` |

完整记录在 `D:\AgentKB-build\p80\real-file-locks.jsonl`，含持锁 PID、错误码、收集结果、`sync_runs.finished_at`、数据库指向和全文哈希。脚本为同目录的 `verify-real-file-locks.py`。隔离库和源文件已清理。

## 恢复后再次收集 Codex 源文件

用临时数据模拟外盘旧快照仍指向 `.jsonl`、备份目录只剩对应 `.jsonl.zst`。恢复应为 `repointed_files=1`、`missing_files=0`。

修补前在 Windows 上复现：本机原始 `.jsonl` 的大小和修改时间仍和快照一致，收集把行改回明文路径后跳过复制，`raw/` 实际仍只有压缩文件。日志为 `restore-regression-first.log`。

修补后，收集跳过未改变的源文件前会确认备份存在。需要重建明文时，复制后只在证明旧 `.zst` 的全部内容已由受管理的备份保存时才删掉旧文件。本机若已有与恢复备份字节相同的 `.zst`，收集直接从现有备份重建索引，避免多写一份 `.prev-*`。内容不同的备份仍按原流程保留。

新增回归 `collecting_the_local_codex_source_after_restore_keeps_one_complete_backup` 在同一测试中覆盖本机保有明文及保有同一压缩文件两种情况，核对收集返回 `ok`、一条文件记录、`raw/` 仅含受该记录管理的备份、解压后的完整字节和长消息全文。定向运行及完整 Cargo 测试均通过。

## NSIS 安装包

`pnpm.cmd tauri build --config D:/AgentKB-build/p80/package-config.json` 成功，release 编译 9 分 08 秒。临时配置只将 `beforeBuildCommand` 置空，使用已通过的前端构建，Windows 配置仍启用 NSIS 与 `windows-cli`。安装脚本确认 `currentUser`、默认安装目录为 `$LOCALAPPDATA\Programs\AgentKB`，并包含 `agentkb-cli.exe`。PE 校验确认主程序为 x64 GUI、CLI 为 x64 Console。

- 交付文件：**`D:\AgentKB-build\p80\AgentKB_0.8.0_x64-setup.exe`**，25,890,028 字节，未签名。
- SHA-256：`1A9075160D914624EE7CE112C5BF182E938B7F14739B0BB96EFD64737E2D7EB5`。
- 原始包 `D:\AgentKB-build\target\release\bundle\nsis\AgentKB_1.32.0_x64-setup.exe` 与交付文件哈希一致。交付文件名标注 AgentKB 版本，内部沿用基础 viewer 版本 1.32.0。
- 构建和校验记录为 `D:\AgentKB-build\p80\nsis-build.log` 和同目录的 `package-result.json`。真实文件占用测试使用的 release 主程序 SHA-256 为 `90FE08A95BEC93B6B5E573973BF34321E712E2A93B87F86CE1E968D54E63D8BF`。

本次未运行安装包，未安装或升级 `D:\AgentKB`，未写真实 `kb.db`，未修改任务计划，未重启电脑。

## Windows 真机覆盖升级（2026-11-22）

经用户同意，22:31（北京时间）将 0.8.0 覆盖安装到原目录 `D:\AgentKB`。安装前核对安装包 SHA-256 为 `1A9075160D914624EE7CE112C5BF182E938B7F14739B0BB96EFD64737E2D7EB5`，没有 `AgentKB.exe` 或 `agentkb-cli.exe` 进程。用 Windows 非阻塞独占文件锁探测 `kb.lock`、`digest.lock`、`vectors\embed.lock`，三把锁均可取得并已立即释放。NSIS 以覆盖更新模式退出 0，注册的安装目录仍为 `D:\AgentKB`。

0.8.0 的 `agentkb-cli.exe` 不提供 `--kb-sync`，该调用返回用法信息、退出 2，数据库版本和收集记录未变。随后使用同一安装目录的 `AgentKB.exe --kb-sync` 无界面收集一次。

| 检查 | 实测结果 |
| --- | --- |
| 手动收集 | 22:31:44 至 22:31:56，`status=ok`、0 个错误、退出 0；`sync_runs.id=10182`，`trigger=cli`、`ok=1` |
| 数据库与迁移备份 | `PRAGMA user_version` 从 5 升到 6；保留 `D:\AgentKB\kb.db.pre-v6-20261122T143144Z`（41,934,848 字节），只读检查确认备份仍为 v5 |
| 旧备份压缩 | 300 个文件，失败 0、待处理 0、旧副本残留 0；247,850,442 → 62,622,726 字节，净省 **185,227,716 字节**（约 176.65 MiB） |
| 搜索 | `agentkb-cli.exe --kb-search "手机连接电脑控制那个事" --limit 5` 退出 0，以 `normal` 模式返回 5 条结果 |
| App 首页 | 打开安装版并留在知识库首页；通过 Windows UI Automation 确认窗口可见，页面有「立即收集」，版本栏显示 `AgentKB 0.8.0` |

升级前和首页打开后导出的正式收集任务 XML 完全一致。未重启、未运行卸载器，也未手动删除文件；旧备份由本轮收集的压缩流程替换。安装、收集、搜索和首页核验记录保存在 `D:\AgentKB-build\p80\upgrade-*`。
