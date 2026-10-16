#!/usr/bin/env python3
"""Where the expected session lands for the P3-eval keyword queries (+ the P4.1 query).

usage: rank_eval.py <AgentKB binary> [HOME]     (read-only: only runs --kb-search)
Prints, per query, the 1-based rank of the expected session among `results` (0 = not there),
the first 5 sessions and the average default output size.
"""
import json, os, subprocess, sys

QUERIES = [
    ("502 网关", "019ef330"), ("充电功率 命令", "019ef243"), ("dubbo provider RpcException", "019ef2b4"),
    ("重庆 攻略 航班", "01a0aa7f"), ("Claude 订阅 中转站", "019fb241"), ("手写 Excel 报价单 还原度", "019f79e1"),
    ("agent-browser Chrome CDP", "019fbd9b"), ("访达 右键 新建文件", "019ef305"), ("熄屏 睡眠 插电", "019f640f"),
    ("tedis 依赖", "01a0424c"), ("提醒 渠道 写死", "00d3c5de"), ("giftInfo null 新用户", "a5731e79"),
    ("Ghostty 默认目录 zshrc", "019ef25a"), ("fastjson 升级 空指针", "01a03daf"), ("红手指 AutoHotkey 滚轮", "630f9b1b"),
    ("电池 耗电", "a3dc8377"),
]
binary = sys.argv[1]
env = dict(os.environ)
if len(sys.argv) > 2:
    env["HOME"] = sys.argv[2]
top5 = top3 = total = size = 0
for query, expected in QUERIES:
    out = subprocess.run([binary, "--kb-search", query], env=env, capture_output=True, check=False).stdout
    data = json.loads(out)
    ids = [r["session"]["session_id"] for r in data["results"]]
    rank = next((i + 1 for i, s in enumerate(ids) if s.startswith(expected)), 0)
    top5 += 0 < rank <= 5
    top3 += 0 < rank <= 3
    total += 1
    size += len(out)
    first = " ".join(f"{s[:8]}{'*' if r.get('has_note') else ''}" for s, r in zip(ids[:5], data["results"][:5]))
    print(f"{query:30} expected {expected} rank {rank}  notes={len(data.get('notes', []))}  top5: {first}")
print(f"in top 5: {top5}/{total}   in top 3: {top3}/{total}   average output {size // total} B")
