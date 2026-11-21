# P8b 增量设计：旧备份压缩（0.8.0，步骤 1）

只写和 `P8-design.md`（基于 0.6.3）不同的地方。基线 `afbf13e`。读取处统一用 `mirror_is_zst(row) = is_compressed || packed`。

## 1. 读写 `raw/` 的地方（重新 grep）

| 位置 | 压缩后怎么办 | 和原设计比 |
|---|---|---|
| `mirror::decide`（`sync.rs: process_file`） | `decide` 用备份**文件长度**和未压缩的 `mirror_offset` 比，压缩文件一定更短 → 判成 Rewrite，把 `.packed.zst` 挪成 `.prev-*` 再把明文写进 `.packed.zst` 这个名字。所以不能"Append 前加一步"，要**`packed=1` 且源文件大小/时间变了 → 先解包，再调 `decide`** | **不同** |
| `reindex_one`、`join_twin_rows`、`reconcile_sources`、`index_mirror_tail`（P7.3 新） | 条件换成 `mirror_is_zst` | 多一处 |
| `export.rs` 截断长度 | 现在只看 `is_compressed`，packed 行会按未压缩长度去截压缩文件 → 换 `mirror_is_zst` | 原写"不用改"，**要改** |
| `merge.rs: imported_name` | 会生成 `X.jsonl.packed.imported-xx.zst`，改成 `X.imported-xx.jsonl.packed.zst` | 小改 |
| `restore.rs: rehome → local_source_path` | 用备份路径推源文件路径，`.packed.zst` 要先还原成 `.jsonl`，否则改错 `source_path` | **新增** |
| `restore.rs: catch_up`、`dedupe::clean_disk` | 见下面 ★ | **新增** |
| `dedupe.rs`（P8a） | 孪生规则从 1 种扩成 4 种（下表），顺带负责"中断留下的"和"Windows 删不掉的"文件 | **不同** |
| `backup.rs: copy_all`（P7.4/7.6 子文件夹） | 照搬 `raw/`，按原相对路径进本机子文件夹，不改 | 同 |
| App 打开：`load.rs`（Claude，mmap）、`chain.rs`（续接链只认 `.jsonl`）、`providers/codex.rs`（只认 `.jsonl.zst`） | 第 3 节 | **不同** |
| `search.rs: read_line_at`/`--kb-show`、`status.rs`、digest、semantic、`migrate_dir.rs` | 不改 | 同 |

**dedupe 的 4 种孪生**：没有行指向的 A，旁边有行指向的 B，用现有 `content_continues`（两边都可以是压缩的）证明 A 是 B 的开头或全部才删 A。

| 没人指向的 A | 有行指向的 B | 什么时候出现 |
|---|---|---|
| `X.jsonl` | `X.jsonl.zst` | P8a 原有 |
| `X.jsonl` | `X.jsonl.packed.zst` | 压缩最后一步前被杀 / Windows 删不掉 |
| `X.jsonl.packed.zst` | `X.jsonl` | 解包最后一步前被杀 / Windows 删不掉 |
| `X.jsonl.packed.zst` | `X.jsonl.zst` | 我们压过后 Codex 又压了源文件 |

所以**不另建"待删清单"**：删不掉就留着，下次收集的 dedupe 证明后再删。硬盘侧 `clean_disk` 用同一张表。

★ **发现（P8a 起就有）**：硬盘上的 `kb.db` 快照一天最多拍一次，而 `clean_disk` 每次备份都跑、且在拍快照之前。白天压了一个会话 → 当次备份证明后删了硬盘上的 `X.jsonl` → 硬盘快照还是早上的，行仍指向 `X.jsonl`。从硬盘恢复后这一行在 `catch_up` 里记为 missing；Claude 源文件 30 天后已没有，原文就看不到了（内容其实在旁边的 `.packed.zst` 里）。Codex 的 `.zst` 情形现在同样存在。

## 2. 改动文件

- 新增 `kb/shrink.rs`（选候选、压缩、解包、统计）；设置页组件 + 5 种语言 i18n
- `db.rs`：v6 迁移 `files.packed`，`FileRow`/`set_packed`
- `fsutil.rs`：只加两个通用函数 `write_via_tmp(dst, 写入闭包)`（临时文件→fsync→rename→同步目录，压缩和解包共用）、`remove_synced`（从 `dedupe.rs` 挪来）
- `sync.rs`：解包时机；收集完、dedupe 前跑一轮压缩（20 秒；压缩线程复用 `semantic/sys.rs` 降优先级，收集本身不降）；4 处 `mirror_is_zst`
- `dedupe.rs`、`restore.rs`、`export.rs`、`merge.rs`：见第 1 节
- `cli.rs`/`commands.rs`：`--kb-shrink [--status|--all]`、设置、"现在压缩一遍"；`config.rs`：`pack { enabled, after_days }`
- 底座各改几行：`commands/session/load.rs`、`chain.rs`、`providers/codex.rs`
- 版本 0.8.0：`kb/mod.rs`、`src/kb/branding.ts`、`src/kb/fixtures.ts`

## 3. App 里打开压缩会话

不用原设计的"解压到缓存目录 + 前端先调准备命令"：中间栏点兄弟会话走底座的 `selectSession`，绕过 `openKbSession`，会漏；还要管缓存大小和 Windows 文件占用。改成**读时在内存里解压**，和 Codex 自己压的 `.zst` 现在的读法一样：`load.rs` 遇到 `.zst` 结尾就 `decode_all` 再解析，否则照旧 mmap；`codex.rs` 认 `.jsonl.packed.zst`；`chain.rs` 候选文件也认 `.jsonl.packed.zst`（续接链的前一半更老、更可能被压），只在有悬空 parent 时才扫。前端不改；压缩/解包后 `file_path` 会变，列表刷新后跟着变。

估算（真实库只读拷贝到仓库外，已删）：最大的 Claude 会话 89.7 MB → 级别 9 压成 39.2 MB，`zstd -d` 0.05 秒；合成 538 MB → 234.6 MB，0.31 秒。App 里整份进内存，额外内存约等于未压缩大小。预计额外等待 90 MB ≤ 0.1 秒、500 MB ≤ 0.4 秒，步骤 2 在 App 里实测。

## 4. 测试清单

任务书第 5 节全部，另加：① packed 行源变长：先解包再 decide，不产生 `.prev-*`；② 解包每一步被杀（钩子），重跑后不少内容、多余文件被 dedupe 清掉；③ 4 种孪生各一正一反（内容不同两份都留）；④ 恢复：快照指向的文件在硬盘上已被孪生替代 → 恢复后能搜能看；`rehome` 对 packed 行改对源路径；⑤ 导出 packed 行不截断，`imported_name` 新格式；⑥ App 打开 Claude 压缩会话、Codex packed 会话、前一半被压的续接链；⑦ 0.7.6 打开 v6 库干净拒绝；⑧ 硬盘：压缩后旧 `X.jsonl` 证明后删，每台电脑子文件夹各自正确。

## 5. 要 leader 决定

1. ★ 怎么修：**(a，建议)** 恢复时行指向的文件不在，按孪生表找替代，证明内容覆盖到 `mirror_offset` 后改指向（同时改 `packed`/`is_compressed`），连 P8a 的 Codex 情形一起修；(b) 硬盘侧删除推迟到当天快照拍完之后——简单，但不救已有备份，且快照一天一次，删得慢
2. App 打开改为内存解压、不做缓存目录（建议）
3. 续接链支持被压的前一半（建议做；扫描太慢就退回"只显示后一半"，记为已知限制）
4. 不另建待删清单，靠 dedupe 每轮补删（建议）
