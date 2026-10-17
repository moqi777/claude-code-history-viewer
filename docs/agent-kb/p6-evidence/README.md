# P6 设计阶段的实测材料

- `spike-src/main.rs`、`Cargo.toml.txt`：测量用程序（`ort` + `tokenizers`），`embed` 建向量、`query` 查最近的段。不是产品代码。
- `chunker.py`：切段（第 3 节的规则）；`queries.py`：评测题（同 `P6-eval.md`）；`kw.py`：用已安装的 0.5.0 在真实库**副本**上取关键词结果；`vec_eval.py`、`fuse_grid.py`、`grid2.py`、`final_tables.py`：向量/混合排序评估和调参。
- `ranks-bge-m3.txt`、`ranks-bge-small-zh.txt`：逐题名次（关键词/向量/混合）。
- 脚本里的 `/private/tmp/p6work` 是当时的临时目录（真实库副本、模型、向量都在那里，不提交）。

## 实现阶段（步骤 2、3）新增

- `setup_home.sh`：做一个开着语义搜索的临时 HOME（真实库副本 + 模型克隆），也是给 leader 跑保密题用的。
- `build_in_runs.sh` / `runs-first-build.log`：一轮接一轮跑 `--kb-embed --auto`，量每轮时间和内存；`build-all-now.log`：`--all` 的耗时和中途停止；`postponed.log`：内存不够时推迟。
- `rank_eval_p6.py` / `eval-final.txt`：真实 `--kb-search` 上的 31 题×2 种问法，语义关/开对比。
- `fallbacks.sh` / `fallbacks.log`：模型缺失、损坏、向量文件坏了或没有、只建了一半。
- `incremental.sh` / `incremental.log`：合成会话，新增/追加/没变化的计数。
- `offline.sh` / `offline.log`：沙盒禁网下建索引和搜索。
- `transfer_vectors.sh` / `transfer_vectors.log`：导出导入带向量；`p5-rerun.log`：P5 的 `run_all.sh` 用新包重跑。
- `snapshot_src.sh`：`~/.claude`、`~/.codex` 前后对比。
- `search-speed.log`：10 次搜索耗时。
- `shot-p6.mjs`（截图，输出在 `../p6-shots/`，模拟 IPC，无真实数据）。

## 第 6.1 期（`p61/`）

- `queries61.py`（上一级目录）：8 道口语题；`p61/rank61.py`：用真实 `--kb-search` 量名次，`ranks61-before.txt` 是 0.6.0 的结果（关键词 / 混合）。
- `p61/diag61.rs.txt`：`semantic/rank.rs` 末尾的临时测试（**没有进源码树**），在真实库副本上导出每题的向量会话分、会话段数和每个词的会话占比。
- `p61/vecall.jsonl`、`gate.jsonl`、`slope.txt`、`qall.json`：上面导出的数据（只有会话 id、分数、问题文字，没有会话内容）。
- `p61/sim61.py`（输出 `sim61.out`）：不需要模型和库，重放 RRF，算 §5 的各个表；`p61/trim_experiment.py`（`trim_experiment.out`）：没有采用的"删常见词再搜"。
