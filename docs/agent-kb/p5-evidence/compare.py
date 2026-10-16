#!/usr/bin/env python3
"""Are two libraries the same to a searcher?  (read-only: --kb-search and --kb-show only)

usage: compare.py <AgentKB binary> <HOME of library 1> <HOME of library 2> [--ignore-note-scores] [ignored session id prefix ...]

--ignore-note-scores: a library whose note index was rebuilt from the note files (every merge does
this) scores the same notes slightly differently from one built up note by note over several
digest rounds (observed on the real library: 16.55 vs 14.51 for the same note, rebuilding A's own
index from A's own note files gives the same change). Order and sessions are still compared.

Sessions named in the optional prefixes are left out of the comparison (a session that is
still being written, such as the one running this work, differs between two copies taken
minutes apart).

For the 15 P3-eval queries plus "电池 耗电": the session ids and their order, the first hit of
each session and the notes must match; then `--kb-show` of the best session of each query must
give the same messages. What is allowed to differ is where the session lives (status, resume
command, paths): library 2 usually holds sessions "from another device".
"""
import json, os, subprocess, sys

QUERIES = [
    "502 网关", "充电功率 命令", "dubbo provider RpcException", "重庆 攻略 航班", "Claude 订阅 中转站",
    "手写 Excel 报价单 还原度", "agent-browser Chrome CDP", "访达 右键 新建文件", "熄屏 睡眠 插电",
    "tedis 依赖", "提醒 渠道 写死", "giftInfo null 新用户", "Ghostty 默认目录 zshrc",
    "fastjson 升级 空指针", "红手指 AutoHotkey 滚轮", "电池 耗电",
]
binary, home1, home2 = sys.argv[1:4]
ARGS = sys.argv[4:]
NOTE_SCORES = "--ignore-note-scores" not in ARGS
IGNORE = tuple(a for a in ARGS if not a.startswith("--"))


def run(home, *args):
    env = dict(os.environ, HOME=home)
    out = subprocess.run([binary, *args], env=env, capture_output=True, check=False).stdout
    return json.loads(out)


def shape(data):
    return {
        "matched": data.get("matched_sessions"),
        "results": [
            (r["session"]["source"], r["session"]["session_id"], r.get("has_note", False),
             [(h.get("seq"), h.get("kind"), h.get("snippet")) for h in r["hits"][:1]])
            for r in data["results"] if not r["session"]["session_id"].startswith(IGNORE)
        ],
        "notes": [(n["session"]["session_id"], n["title"], n["score"] if NOTE_SCORES else None)
                  for n in data.get("notes", []) if not n["session"]["session_id"].startswith(IGNORE)],
    }


def strip(value):
    """Drop what legitimately depends on where a session lives."""
    if isinstance(value, dict):
        return {k: strip(v) for k, v in value.items()
                if k not in ("source_status", "origin", "resume", "paths", "data_as_of", "read_mode", "hint")}
    if isinstance(value, list):
        return [strip(v) for v in value]
    return value


same_search = same_show = 0
for query in QUERIES:
    a, b = run(home1, "--kb-search", query), run(home2, "--kb-search", query)
    sa, sb = shape(a), shape(b)
    ok = sa == sb
    same_search += ok
    show_ok = True
    if sa["results"]:
        source, sid = sa["results"][0][0], sa["results"][0][1]
        key = f"{source}:{sid}"
        show_ok = strip(run(home1, "--kb-show", key, "--head", "30")) == strip(run(home2, "--kb-show", key, "--head", "30"))
    same_show += show_ok
    order = " ".join(r[1][:8] for r in sa["results"][:4])
    print(f"{query:30} search {'same' if ok else 'DIFFERENT'}  show {'same' if show_ok else 'DIFFERENT'}  first: {order}")
print(f"search identical: {same_search}/{len(QUERIES)}   show identical: {same_show}/{len(QUERIES)}")
sys.exit(0 if same_search == len(QUERIES) and same_show == len(QUERIES) else 1)
