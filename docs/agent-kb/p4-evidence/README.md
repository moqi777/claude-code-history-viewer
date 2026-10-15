# P4 设计阶段实测证据

不含用户会话原文：只放脚本、提示词、schema、token/耗时/退出码和 launchd 实验记录。样本笔记原文留到步骤 4 的 `P4-samples.md`。

- `prompt_system.txt` — 系统提示词（与设计 §4.5 一致；"每个话题一条"一句是实测之后加的，尚未实测）
- `schema.json`（Claude）/ `schema-codex.json`（Codex，去掉 maxLength/maxItems/minItems）
- `build_input.py` — 输入拼装原型（Rust 版按同样规则）
- `run_claude.sh`、`run_codex.sh`、`codex-flags.sh` — 实测用的完整命令
- `snap.py`、`diff.py` — 调用前后对 `~/.claude`、`~/.codex` 的文件快照对比
- `runs-usage.json` — 各次真实调用的 token、费用、模型
- `launchd-test/` — 临时 launchd 任务拉起后台 `claude -p` 的实验（父进程退出后子进程仍完成，PATH 为 launchd 默认值）

## 源目录快照对比的结论（路径只列类别）

- Claude：没有新会话文件；`~/.claude/projects` 下没有专用目录的项目；`~/.claude.json` 没有专用目录条目；唯一变化是 `~/.claude/backups/.claude.json.backup.*` 滚动（同时被正在运行的交互会话维护，无法归因）。
- Codex（`--ephemeral`）：没有新的 `sessions/**/rollout-*.jsonl`，`history.jsonl` 未变；变化的是 Codex 自己的内务文件：`state_5.sqlite-*`、`goals_1.sqlite-*`、`logs_2.sqlite*`、`models_cache.json`、`cache/codex_apps_*`、`plugins/cache/**/.codex-remote-plugin-install.json`、`skills/.system/**`、`.codex-global-state.json(.bak)`、`app-server-daemon`、`shell_snapshots/` 旧文件被清理、`tmp/arg0/`。
