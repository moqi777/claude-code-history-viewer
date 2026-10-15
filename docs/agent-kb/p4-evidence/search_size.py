#!/usr/bin/env python3
"""Size of `--kb-search` output on the 15 P3-eval keyword queries.

usage: search_size.py <AgentKB binary> <HOME of an isolated AgentKB setup>
Prints, per query, the bytes of the default and the --verbose output, the number of
notes returned, and whether the expected session is among the first 5 sessions.
"""
import json, os, subprocess, sys

QUERIES = [
    ("502 网关", "019ef330"), ("充电功率 命令", "019ef243"), ("dubbo provider RpcException", "019ef2b4"),
    ("重庆 攻略 航班", "01a0aa7f"), ("Claude 订阅 中转站", "019fb241"), ("手写 Excel 报价单 还原度", "019f79e1"),
    ("agent-browser Chrome CDP", "019fbd9b"), ("访达 右键 新建文件", "019ef305"), ("熄屏 睡眠 插电", "019f640f"),
    ("tedis 依赖", "01a0424c"), ("提醒 渠道 写死", "00d3c5de"), ("giftInfo null 新用户", "a5731e79"),
    ("Ghostty 默认目录 zshrc", "019ef25a"), ("fastjson 升级 空指针", "01a03daf"), ("红手指 AutoHotkey 滚轮", "630f9b1b"),
]
binary, home = sys.argv[1], sys.argv[2]
env = dict(os.environ, HOME=home)
rows, hits = [], 0
for query, expected in QUERIES:
    out = {}
    for label, extra in (("default", []), ("verbose", ["--verbose"])):
        p = subprocess.run([binary, "--kb-search", query, *extra], env=env, capture_output=True, check=False)
        out[label] = p.stdout
    data = json.loads(out["default"])
    top = [r["session"]["session_id"] for r in data.get("results", [])[:5]]
    found = any(s.startswith(expected) for s in top)
    hits += found
    rows.append((query, len(out["default"]), len(out["verbose"]), len(data.get("notes", [])), found))
    print(f"{query:36} default={len(out['default']):6} verbose={len(out['verbose']):6} notes={len(data.get('notes', []))} found={found}")
n = len(rows)
print(f"average default {sum(r[1] for r in rows) / n:.0f} B, verbose {sum(r[2] for r in rows) / n:.0f} B, found {hits}/{n}")
