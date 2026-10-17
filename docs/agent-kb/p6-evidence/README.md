# P6 设计阶段的实测材料

- `spike-src/main.rs`、`Cargo.toml.txt`：测量用程序（`ort` + `tokenizers`），`embed` 建向量、`query` 查最近的段。不是产品代码。
- `chunker.py`：切段（第 3 节的规则）；`queries.py`：评测题（同 `P6-eval.md`）；`kw.py`：用已安装的 0.5.0 在真实库**副本**上取关键词结果；`vec_eval.py`、`fuse_grid.py`、`grid2.py`、`final_tables.py`：向量/混合排序评估和调参。
- `ranks-bge-m3.txt`、`ranks-bge-small-zh.txt`：逐题名次（关键词/向量/混合）。
- 脚本里的 `/private/tmp/p6work` 是当时的临时目录（真实库副本、模型、向量都在那里，不提交）。
