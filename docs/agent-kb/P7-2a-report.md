# P7 步骤 2a Windows 检查点报告

## 范围与基线

- 工作分支：`agent-kb-win`
- 已先合并 `origin/agent-kb`，合并提交为 `bf1c4654`（包含 `5adaaea8` 的 P7 设计审核意见）。
- 本检查点只处理 Windows 测试夹具、文件系统和锁的兼容性、clippy 问题以及 junction 安全检查；没有开始步骤 2b 的功能实现。
- 本次提交只推送到 `origin/agent-kb-win`，不会推送到 `agent-kb`。

## 安全和测试边界

- 测试使用临时目录，最终 Rust 测试目录为 `%TEMP%\agentkb-p7-2a-final-rust2`，前端测试使用独立临时目录。
- 没有读取真实 Claude Code 或 Codex 记录内容；真实目录仅作存在性边界处理，保持只读。
- 没有读取密钥、令牌或凭据文件。
- fake CLI 在 Windows 上改为编译临时 Rust 程序生成的 `fake-ai.exe`，不调用真实 AI CLI。
- 符号链接测试遇到 Windows `os error 1314` 时按设计打印原因并跳过；junction 安全检查仍执行，`safe_join` 和目录扫描都会拒绝重解析点。
- 没有修改系统执行策略；PowerShell 下直接调用 `pnpm` 可执行命令运行前端检查。

## PLAN 第 6 节七项检查

| # | 命令 | 结果 |
|---|---|---|
| 1 | `pnpm tsc --build .` | 通过 |
| 2 | `pnpm vitest run` | 通过：146 个测试文件，1575 个测试全部通过 |
| 3 | `pnpm lint` | 通过 |
| 4 | `pnpm run i18n:validate` | 通过：各语言 1990 个 key 同步、无重复；已有 provider 名称未翻译提示为 warning，不影响退出码 |
| 5 | `cargo test -- --test-threads=1` | 通过：1445 passed，0 failed，6 ignored；其余集成测试目标也全部通过 |
| 6 | `cargo clippy --all-targets --all-features -- -D warnings` | 通过，无 warning/error |
| 7 | `cargo fmt --all -- --check` | 通过 |

Rust 检查实际使用了 `--manifest-path src-tauri/Cargo.toml`；前端检查从仓库根目录运行。最后还执行了 `git diff --check`，没有发现空白错误。

## 本检查点的实现内容

- 修正 Windows 路径、分隔符和带空格/中文临时目录的测试夹具。
- 将 fake shell CLI 替换为无外部依赖的 Rust fake CLI，并覆盖成功、更新、错误、配额、认证和超时场景。
- 为 Windows 文件同步、锁文件共享模式、sidecar 信息文件和进程存活检查补充实现。
- 在目录发现、`safe_join` 和迁移路径中阻止符号链接/junction 穿越，并补充 junction 测试。
- 修正 Windows 专属代码触发的 clippy 问题，并保持格式检查通过。

## 待审核

步骤 2a 已完成，等待审核。步骤 2b 尚未开始。

## 审核意见修订后的复跑

本轮先合并了 `origin/agent-kb` 的 `e703ac26`（`P7-2a-review.md`），只处理审核意见第 1 节；第 2 节全部留到步骤 2b。

### 修订内容

1. 删除 `fsutil::sync_file` 的 Windows `PermissionDenied` 豁免；所有需要持久化的文件恢复直接传播 `sync_all()` 错误，压缩镜像临时文件改用可写句柄后再同步。
2. `paths::is_absolute` 恢复严格的 `Path::is_absolute()`；受影响的 Windows 测试夹具改用临时目录生成真实绝对路径。
3. 锁旁路 `.info` 文件仅在 Windows 写入、读取和清理；迁移跳过列表改为确切的运行时锁文件名，并增加迁移覆盖测试。
4. 增加 `safe_join` 遇到 junction 组件时返回错误的 Windows 测试；实际创建 junction 后测试通过。

### 七项复跑结果

| # | 命令 | 结果 |
|---|---|---|
| 1 | `pnpm tsc --build .` | 通过 |
| 2 | `pnpm vitest run` | 通过：146 个测试文件，1575 个测试全部通过 |
| 3 | `pnpm lint` | 通过 |
| 4 | `pnpm run i18n:validate` | 通过：各语言 1990 个 key 同步、无重复；已有 provider 名称提示仍为 warning-only |
| 5 | `cargo test -j 1 -- --test-threads=1` | 通过：库 1447 passed、0 failed、6 ignored；集成目标分别为 6、5、4（另 6 ignored）和 3 个通过 |
| 6 | `cargo clippy -j 1 --all-targets --all-features -- -D warnings` | 通过，无 warning/error |
| 7 | `cargo fmt --all -- --check` | 通过 |

Rust 检查使用 `--manifest-path src-tauri/Cargo.toml`；`-j 1` 用于避免 Windows MSVC 并行链接时的 PDB 资源竞争。所有复跑仍使用 `%TEMP%` 下的独立测试根目录，没有读取真实 Claude Code/Codex 记录或凭据文件。复跑结束后执行了 `git diff --check`，无空白错误。

本轮仍未开始步骤 2b。
