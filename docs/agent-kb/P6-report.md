# P6 报告：语义搜索（本地向量），AgentKB 0.6.0

状态：步骤 0–3 做完，已推送到 `origin/agent-kb`，**没有安装到 `/Applications`**（里面还是 0.5.0，二进制 34,942,976 字节，Oct 16 23:13，没动过）。等 leader 验收。

打好的包：`src-tauri/target/release/bundle/macos/AgentKB.app`（`codesign --verify --deep --strict` 通过；主程序 SHA-256 `5e2fc12b9acae4d276489ccf926b2db1ed6a559530e78a5049a182bf53940020`，61,412,224 字节）。

## 1. 提交

| 提交 | 内容 |
|---|---|
| `d083178` | 步骤 0：界面文件大小统一按 1000 换算（`format.ts`，测试跟着改） |
| `188a77b` | `P6-eval.md`（新 15 题，先于任何向量检索提交） |
| `f5538e7` / `eff897f` | 设计、实测程序 / leader 的设计审核 |
| `ee8f6ad` | 核心：切段、向量文件、模型下载与校验、增量建索引、混合排序、`--kb-embed`、Tauri 命令 |
| `e483bcd` | 设置页、搜索页标记、导出导入带向量、锁、skill 模板 4、0.6.0 |
| `a96d4cc` | 证据脚本，损坏向量文件自愈，重复键修复 |
| `e1852ec` | 笔记索引分数不一致的修复，界面截图，手动测试 |

规模：Rust `kb/semantic/` 11 个文件约 3,500 行（含测试），前端 `KbSemantic.tsx`、44+1 个 i18n 键 ×5 语言、各处小改动，共 50 个文件。

## 2. 按设计审核第 2 节补的 5 处

1. **读 `kb.db` 不用 `immutable`**：`corpus::open_ro` 只用 `mode=ro`，读不了就报错、本轮跳过，没有退路到 immutable。测试 `messages_still_in_the_write_ahead_log_are_seen`：另一个连接写入后不让它合并进主文件（`wal_autocheckpoint=0`，`-wal` 文件非空），建向量那一轮能读到这条新消息（`total` 从 6 变成 7）。
2. **真实优先级下的速度**：见第 4.4 节。结论：**不用 macOS 的"后台"类别**（实测慢 8–24 倍），默认 `nice 19`，速度和不降优先级时一样。加了设置页的"现在全部建完"按钮（正常优先级、可取消、有进度）。
3. **锁**：`embed.lock`（`vectors/embed.lock`）。**换数据目录**（`migrate_dir::migrate_data`）和 **P5 导入的提交**阶段都要拿它，拿不到等一小会儿（生产 60 秒）后报"正在建语义索引，稍等再试"（导入的错误码 `embed_running`，前端有文字）；换数据目录时 `vectors/` 和 `models/` 也一起复制并逐文件校验（模型下载一次 570 MB，不该丢）。**导出**不等建索引：向量文件用 SQLite 的备份接口拍一份一致的快照（`Store::backup_to`），不直接复制文件。测试：`an_import_does_not_start_while_the_vector_builder_runs`、`a_move_waits_for_a_running_vector_builder_instead_of_copying_under_it`、`the_semantic_index_and_the_model_move_along`、`backup_is_a_usable_copy`。
4. **构建时联网 / 打包验证**：见第 4.8 节。ONNX Runtime **1.22.0**，由 `ort-sys 2.0.0-rc.10` 在编译时从 `cdn.pyke.io` 下载，压缩包 SHA-256（`ort-sys` 的 `dist.txt` 里写死、编译时校验）`00FBFD6F08BAC2A4E28C66723AF900D58D1B4B1C73EFBA6290637CD3019883D5`；静态库 `libonnxruntime.a` 71,060,144 字节，SHA-256 `e5c83560aa9e88afa39d9dca9fb5f5a767e28adb5458d1c36fe0357131b6af8b`。静态链接，包里没有 dylib。
5. **保密题跑法**：见第 6 节。

## 3. 改了什么（要点）

- **切段 `chunk.rs`**：和设计一样（代码块 >200 字符丢掉、500 字符一段、每条消息最多 6 段、短于 20 字符不要；tool 输出不建；笔记整条一段、标题一段；笔记建段前再过一次 `redact`，消息入库时已经过了）。
- **向量文件 `store.rs`**：`<数据目录>/vectors/bge-m3-int8-v1.sqlite3`，键是 `(source, session_id, key)` + 文本的 SHA-256，int8 加缩放系数；模型或切段规则变了整个文件作废。不升 `kb.db` 的 schema（还是 v5）。
- **模型 `model.rs`**：大小和 SHA-256 写死（`onnx-community/bge-m3-ONNX` 的 `model_int8.onnx` 568,479,395 字节 `2237f770…4730`，`tokenizer.json` 17,082,799 字节 `249df077…8fd4`）；两个来源 ModelScope（默认）和 Hugging Face；`.part` + `Range` 续传 + 边下边算哈希，不符删掉；本地文件夹导入只认哈希对的文件；"测试"和搜索前用大小+修改时间快速确认，变了才重算 SHA。
- **推理 `embed.rs`**：`ort =2.0.0-rc.10` + `tokenizers`（`default-features=false, features=["onig"]`）。`Embedder` 是接口，测试用假的。
- **增量 `build.rs`**：`--kb-embed --auto|--all|--status|--test`；对比"应有的段"和已有的（新增/变化/没变/删除四个计数写日志和状态文件），先删旧的，再按"笔记→标题→最近活动的会话"的顺序算；每批 8 段检查时间和取消；内存检查（macOS 把空闲、非活跃、推测、可清除页都算上），低于 4 GB 本轮推迟并计数；收集结束后由 `cli.rs` 在后台拉起（同数字整理）。
- **混合排序 `rank.rs` + `search.rs::merge_semantic`**：设计里的公式，4 个常数定死（RRF 10、关键词权重 1.0/0.5、向量取与最高分差 ≤0.1 的前 10 个会话、第二高分占 0.25）。向量会话和关键词一样走过滤条件（来源、客户端、项目、时间、子代理折叠）。笔记命中的会话仍排最前（`promote_note_sessions` 在合并之后照常执行）。新增键：`results[].match`（`keyword`/`semantic`/`both`）、`semantic_score`、`mode_used` 新值 `hybrid`、响应的 `semantic`（`state`/`indexed`/`total`/`note`）。**原有键一个没改**（测试 `without_vectors_the_result_is_the_keyword_result_with_a_reason` 和 skill 字段冻结测试都过）。
- **设置页 `KbSemantic.tsx`**：开关（没下载并测试通过前不能打开）、来源选择（写明域名和大小）、下载（进度、取消、失败原因）、从本地文件夹选、测试、索引进度条、"现在建一批 / 现在全部建完 / 停止 / 重建"、推迟提示、模型损坏提示。**保存设置时不会覆盖这一组**：`save_config` 保留磁盘上的 `semantic`（设置页打开期间如果另一个按钮改了它，不会被旧草稿改回去），测试 `a_settings_page_opened_earlier_cannot_undo_semantic_search`。
- **搜索页**：结果标签"语义命中 / 关键词+语义"（只有关键词命中的不加标签）、顶部一行"关键词+语义"、向量没建完或退回时一行灰字原因。
- **导出导入**：导出多一个勾选"同时带上语义索引"（默认勾，`--kb-export --no-vectors` 关掉）；导入只在**空库**且模型 SHA 一致时采用，合并模式不碰。`.agentkb` 的路径白名单加了 `vectors/`（**0.5.0 读不了带这一项的文件**，会报"路径不合规"；导出时取消勾选即可兼容）。
- **skill 模板 4**：加了两句——`has_note` 为 true 且 `hits` 为空表示靠笔记找到；`match` 各个值怎么理解、`semantic.state` 不是 used 时只用了关键词。用户点一次"更新"。
- **设置合并**：导入不会带来 `semantic.enabled`（`PortableSettings` 里本来就没有）。

## 4. 验收（全部在临时 HOME；证据文件都在 `p6-evidence/`）

测试用的库：真实库的副本（`cp -c` 克隆 `~/AgentKB/kb.db`，只读；当时 313 个会话、24,385 段）。包是上面那个，没装。

### 4.1 换说法能搜到

`rank_eval_p6.py`：真实的 `--kb-search`，默认参数，整句或关键词原样，语义关/开各跑一次（`eval-final.txt`）。每题的名次写成 `关键词只用/混合`（0 = 没进结果）：

| 题组 | 逐题名次 | 前 5 命中 |
|---|---|---|
| 新 15 题，整句 | 1:0/1 2:0/2 3:0/5 4:0/1 5:3/3 6:0/4 7:0/0 8:2/1 9:2/3 10:0/1 11:1/1 12:0/3 13:4/2 14:1/2 15:1/4 | **7 → 14 / 15**（要求 ≥12） |
| 新 15 题，关键词形 | 1:0/2 2:0/2 3:0/0 4:0/2 5:0/0 6:0/1 7:0/0 8:1/1 9:3/5 10:4/1 11:1/2 12:1/1 13:0/0 14:4/0 15:0/5 | **6 → 10 / 15**（要求 ≥9） |

没进前 5 的：整句第 7 题（git-ai，两种方式都没找到）；关键词形第 3、7、13、14、15 题。**有一题变差**：关键词形第 14 题（Typora），只用关键词排第 4，混合后挤出了结果（向量侧没找到，关键词那一路的权重在"任一词命中"时只有 0.5，被别的会话挤下去）。前 3 命中：整句 6 → 11，关键词形 4 → 8。

### 4.2 不变差

| 旧 16 题 | 前 3 命中 | 第 1 名 |
|---|---|---|
| 关键词形：只用关键词 / 混合 | 16/16 / **16/16** | 13 / 13 |
| 整句：只用关键词 / 混合 | 16/16 / **16/16** | 12 / 14 |

"电池 耗电"：混合第 1（`a3dc8377`）。逐题：关键词形 1:1/1 … 8:2/2 10:2/2 14:2/3（fastjson 由第 2 降到第 3，仍在前 3）… 16:1/1。

### 4.3 退回（`fallbacks.sh`，`fallbacks.log`，打好的包，都是 `--kb-search "电池 耗电"`，退出码 0，结果 5 条，第 1 名都是 `a3dc8377`）

| 情况 | `mode_used` | `semantic.state` |
|---|---|---|
| 正常 | hybrid | used |
| 删掉模型文件夹 | all | model_missing（"the model is not downloaded; keywords used"） |
| 模型改了 1 个字节 | all | model_corrupt（"…the SHA-256 does not match; keywords used"） |
| 向量文件被写坏 | all | error（"the vector file is damaged (…); keywords used"）；下一次建索引会自动丢掉重建（测试 `a_damaged_vector_file_is_thrown_away_and_built_again`） |
| 向量文件不存在 | all | index_empty |
| 只建了三分之一 | hybrid | index_building（"the semantic index is 33% built; results may be incomplete"），照常用已建的部分 |

### 4.4 速度和"不打扰"（真实库副本 24,385 段，`build_in_runs.sh`，`runs-first-build.log`）

**测量方法**：`/usr/bin/time -l` 量每一轮 `--kb-embed --auto` 的墙钟时间和最大常驻内存；一轮接一轮地跑，等效于收集间隔不等待。

- **第一次全量建**：46 轮，每轮 **50.9–54.6 秒**（平均 52.2，其中约 50 秒是计算、1–4 秒是加载模型和比对），峰值内存 **1.26–2.00 GB**，全程 **0 轮因内存推迟**；总墙钟 2,399 秒。收集间隔 10 分钟的话，**46 轮 ≈ 7.7 小时**建完。
- **优先级的选择**（同一台机器，3 线程，30 秒预算，计算量相同的前半部分，笔记在前所以每段较长）：不降优先级 184 段 / `nice 19` 184 段 / `nice 10`+QoS utility 184 段 / **macOS 后台类别 24 段**（第一次实现用的就是后台类别：一个 64 段的窗口算了 114 秒，所以改了）。默认用 `nice 19`：机器空闲时速度不打折，忙时让位。可用环境变量 `AGENTKB_PRIORITY=none|nice|utility|background` 复现。
- **预算**：原来只在 64 段一个窗口结束时检查，实测一轮超时 21 秒；改成每批 8 段检查，一轮最多超 1 批。
- **"现在全部建完"**（`--kb-embed --all`，正常优先级，8 线程，`build-all-now.log`）：中途用 SIGTERM 停（70 秒，已存 1,584 段，2 秒内退出，状态回到 idle）再接着跑，**剩下的 22,808 段 869 秒（14.5 分钟）**，最大常驻内存 1.55 GB（峰值足迹 1.84 GB）。
- **内存推迟**：`AGENTKB_EMBED_AVAILABLE_MB=1500` 假装只剩 1.5 GB：连续 3 次 `Postponed`，`postponed_rounds` 1→2→3，状态 `postponed_memory`，日志 "postponed, 1500 MB available, 4096 MB needed"；内存恢复后那一轮正常跑完、计数清零（`postponed.log`）。设置页会显示"因为电脑内存紧张，已推迟 N 次"（截图 `p6-shots/semantic-postponed.png`）。**在真实库副本上完整建一次的 46 轮里，没有一轮被推迟**（这台机器 24 GB，可用内存一直够）。
- 峰值内存 2.0 GB 超过任务建议的 1.5 GB，按审核意见接受；每轮结束进程就退出，内存马上还回去。

### 4.5 搜索速度（`search-speed.log`，打好的包，整句，默认参数，10 次墙钟秒）

- 语义关：`0.14 0.14 0.14 0.14 0.14 0.14 0.14 0.14 0.14 0.14`（≤0.3）
- 语义开（含加载模型和读向量）：`0.89 0.82 0.83 0.83 0.81 0.80 0.82 0.79 0.80 0.79`，**中位数 0.82，最大 0.89**（≤1.5 / ≤2.5）。这批 10 次的文件缓存都是热的；设计测过的第一次冷缓存是 1.41 秒（模型+向量部分），加上关键词部分约 1.6 秒，仍在最大值 2.5 秒以内。

### 4.6 增量（`incremental.sh`，`incremental.log`，合成会话）

第一轮：新增 3；新会话 + 旧会话追加消息 + 收集一次后第二轮：`new 4 changed 0 unchanged 3`（只算新增的 4 段）；第三轮：`UpToDate`，`unchanged 7`，没加载模型。日志：`wanted 3 new 3` / `wanted 7 new 4 unchanged 3` / `wanted 7 new 0 unchanged 7`。换说法的搜索都找到：搜"续航变短"得到合成会话 `22222222`（写的是"电量掉得特别快"），"图片放在网上的托管服务"得到 `11111111`（写的是"对象存储"）。单元测试另覆盖新增、变化、删除、限时中断后接着做。

### 4.7 离线（`offline.sh`，`offline.log`）

macOS 沙盒配置 `(deny network*)`（先证明沙盒有效：同样配置下 `curl` 退出码 6，解析不了域名），下面全部成功：建完整个（小）索引、`--kb-search` 用语义、模型测试。源码里只有 `semantic/model.rs` 用 HTTP 客户端（`ureq`），只在点"下载模型"时执行；打好的程序里没有 `cdn.pyke.io` 字符串，不会在运行时去下载 ONNX Runtime。另外用真实模型和真实地址跑过一次下载：忽略测试 `real_download_from_modelscope` 通过程序自己的下载代码在 73 秒内下载并校验通过 568 MB；`real_urls_answer_with_the_pinned_size` 确认两个来源都对 Range 请求回 206 且总大小等于写死的大小。

### 4.8 不碰源目录

`snapshot_src.sh`：对 `~/.claude`、`~/.codex` 里已空闲 60 分钟的 10,832 个文件取（路径、大小、修改时间）的哈希，验收期间前后各取。**其中 10,831 个没变；变了的一个是 `~/.codex/logs_2.sqlite-shm`**：它是 Codex 自己常驻后台进程（app-server）正在写的日志数据库的共享内存文件，现在 `logs_2.sqlite-wal` 仍在不断增长。AgentKB 从不打开 `logs_*`（代码里只读 `state_*.sqlite` 取标题）。直接验证：用真实的 `~/.claude`、`~/.codex` 做源跑两次收集，前后对 `logs_2.sqlite-shm`、`state_5.sqlite*`、`session_index.jsonl` 做 `stat` 对比，**没有任何变化**。（快照脚本对 `~/.claude/projects/-Users-mo7-Dev-JavaCode/memory/` 里的文件也只做了 `stat`，没读内容。）

### 4.9 导出导入

- P5 的 `run_all.sh` 用新包重跑（`p5-rerun.log`）：导出 19 秒 / 2.14 GB，导入 `all_match`，**会话数、笔记数一致，搜索 16/16、`--kb-show` 16/16 一致**，每个文件 SHA 一致，改一个字节被拒绝且目标不变，`kill -9` 后重导入一致，合并两次，导出时同时每秒一次收集 19 次都 ok，整理在跑时导出被拒绝。（第 6 步"换用户名重打包"需要的手动辅助测试这次没跑，P5 时跑过。）
- 向量（`transfer_vectors.sh`，`transfer_vectors.log`）：带索引的导出文件 2,167,331,784 字节，不带 2,145,317,393 字节（**差 22 MB**）；导入到空库 `vectors_adopted True`、`all_match`；那台机器打开语义后跑一轮建索引：`unchanged 24384`、只算了 7 段新增的（库比向量新），没有重算；A、B 两边 3 个查询的前 5 名完全一样；不带向量的导入 `vectors_adopted False`。合并模式不采用（`import_tests`）。

### 4.10 门禁

`pnpm tsc --build .` 通过；`pnpm vitest run` 146 个文件 / 1568 通过；`pnpm lint` 通过；`pnpm run i18n:validate` 通过；`cargo test -- --test-threads=1` 1468 通过、6 忽略（真实模型、真实下载、真实库笔记分数、kilo 冒烟、P5 重打包辅助）；`cargo clippy --all-targets --all-features -- -D warnings` 通过；`cargo fmt --all -- --check` 通过。真实模型的忽略测试 `related_sentences_are_closer_than_unrelated_ones` 单独跑过，通过。

## 5. 顺带解决的问题

- **笔记索引增量和重建分数不同（P4.1/P5 留下的）：找到原因并修了。** 原因：`notes_fts` 是 `contentless_delete=1` 的 FTS5 表，替换一个会话的笔记（先删后插）时，`bm25` 用的文档总数和总长度里没有扣掉被删的文档，所以增量长大的索引和从文件重建的索引对同一个词给不同的分。真实库副本上复现：20 行里 20 行分数不同（"电池"-6.10 vs -4.99，约差 20%，方向因词而异）。修法：每次替换笔记后用 `notes` 表重做一遍 `notes_fts`（几百行，毫秒级，`refresh_fts`），并把 `NOTES_INDEX_VERSION` 升到 2：**真实库在装 0.6.0 后第一次收集会从 `notes/` 文件重建一次笔记索引**（衍生数据，不丢东西）。测试 `scores_do_not_depend_on_how_the_index_was_built`（用只在部分笔记里出现的词，修之前失败：增量 `-0.000001`，重建 `-0.163942`）；真实库副本上跑一次收集后再比：0 行不同。
- **开发会话常排前面**：没做专门降权（设计结论）。61 条评测查询里，开发会话占 32 个会话（10%），在关键词前 5 名占 8.7%、向量 6.2%、混合 9.1%，没有被不成比例地排前。
- **设计里没想到、实测发现并修掉的问题**：同一个会话号出现在两个文件里（移动过的会话）时键重复，那一段每轮都"变化"、永远重算一次（`corpus::desired` 去重，测试 `two_files_with_the_same_session_id_do_not_make_every_run_compute_again`）；向量文件损坏时搜索报"索引为空"，现在报"文件已损坏"并且建索引时自动重建；预算只在窗口结束检查（见 4.4）；macOS 后台优先级太慢（见 4.4）。

## 6. 给 leader：保密题怎么跑

用打好的包，不装。

```
E=docs/agent-kb/p6-evidence
# 1) 临时 HOME：库是 ~/AgentKB/kb.db 的副本（只读克隆），模型从 /private/tmp/p6work/models/bge-m3 克隆，
#    config.json 里 semantic.enabled=true，来源文件夹是空的（不会碰 ~/.claude、~/.codex）
$E/setup_home.sh /private/tmp/p6-leader
BIN=src-tauri/target/release/bundle/macos/AgentKB.app/Contents/MacOS/AgentKB
H=/private/tmp/p6-leader/home
# 2) 模型测试（约 2 秒），然后一次建完（正常优先级，约 15 分钟；不加 --all 是每次 50 秒的后台方式）
HOME=$H $BIN --kb-embed --test
HOME=$H $BIN --kb-embed --all
HOME=$H $BIN --kb-embed --status        # total / done，done == total 就建完了
# 3) 搜索：整句、默认参数
HOME=$H $BIN --kb-search "一句话的问法"
```

- **不需要设任何环境变量或参数**：开关是临时 HOME 里 `config.json` 的 `semantic.enabled`（`setup_home.sh` 已经写成 true）。输出里的 `semantic.state` 为 `used` 才表示用了向量，`mode_used` 为 `hybrid`；每条结果的 `match` 说明是关键词、语义还是两者命中。
- **要对比"只用关键词"**：再做一个 HOME（`setup_home.sh /private/tmp/p6-leader-off`），把它的 `config.json` 里 `"enabled":true` 改成 `false`（这个 HOME 不需要建向量）。语义开关只看配置文件，环境变量不起作用。`p6-evidence/rank_eval_p6.py <BIN> <关闭的 HOME> <打开的 HOME>` 可以一次对比两边。
- 模型文件在 `/private/tmp/p6work/models/bge-m3/`（`model.onnx`、`tokenizer.json`，重启后可能被系统清掉；到时从 ModelScope 下载，地址和 SHA-256 在 `model.rs` 里和上面第 3 节，或者在 App 的设置页点"下载模型"）。
- 其余测试用的环境变量（只用来测，日常不用）：`AGENTKB_EMBED_BUDGET_SECS`、`AGENTKB_EMBED_THREADS`、`AGENTKB_PRIORITY`、`AGENTKB_EMBED_AVAILABLE_MB`。

## 7. 没做完的、偏离的、要注意的

- **峰值内存 2.0 GB**（审核已接受）；**第一次全量建约 7.7 小时**（后台方式）或 15 分钟（"现在全部建完"，期间电脑会用掉 8 个线程）。
- **一题变差**：关键词形第 14 题（4 → 没进结果），见 4.1。融合常数按审核要求定死，没有为它调。
- **e5-small 没量**（审核已同意不补）。
- **Windows 没验证**（第 7 期）：`ort` 在 Windows 的预编译库、`sys::available_memory` 在 Windows 返回"未知"（不做内存检查）、`lower_priority` 在非 Unix 不做事。
- **0.5.0 读不了带向量的导出文件**（见第 3 节）；要给 0.5.0 用就取消勾选。
- **`tokenizers` 的特性集和安装包大小**（审核要求给数字）：只开 `onig`；安装包 `AgentKB.app` 34.1 MB → **59.3 MB**（+25.2 MB），主程序 34.9 → 61.4 MB。
- **GUI 里的搜索也会在进程里加载模型**（约 2 GB，约 0.5 秒，搜索完释放）：搜索前检查可用内存（低于 2.5 GB 就退回关键词，`semantic.state` 为 `skipped`）。如果 leader 觉得 GUI 不该背这 2 GB，下一期可以改成 GUI 通过子进程搜索。
- **使用 `--kinds` 过滤时不用语义**（`skipped`），因为向量不分消息类型。
- 临时目录：`/private/tmp/p6e2e`（含真实库副本和向量）会在写完报告后删除；`/private/tmp/p6work` 里只留模型和测量程序（不含个人内容）。仓库里没有模型、向量、导出文件。

## 8. 用户真机确认清单（装好 0.6.0 之后）

- [ ] 11 设置页（"语义搜索"）：选来源（默认 ModelScope）→ 点"下载模型"（约 1–3 分钟，569 MB）→ 点"测试"通过 → 勾"使用语义搜索"。索引在后台自动建（**约 8 小时**，每 10 分钟一批，电脑用着不卡；想马上用就点"现在全部建完"，约 15 分钟，期间风扇会转）。进度条会走。
- [ ] 12 在 App 搜一个"记得意思、记不得原话"的问题（比如"续航变短"、"怎么让外面的人访问到我电脑上跑的服务"），结果里应该有"语义命中"标签的会话。
- [ ] 13 在 Claude Code 里先在设置页把 skill 点一次"更新"，再用自然语言问一个换了说法的问题，Agent 应该通过 skill 找到对应的会话。
- [ ] 装好后第一次收集会把笔记索引重建一次（几毫秒，不丢东西）；`~/AgentKB` 里会多出 `vectors/`（约 25 MB）和 `models/`（约 570 MB）。
