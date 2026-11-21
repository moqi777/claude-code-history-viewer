#!/usr/bin/env python3
"""Run the old 16 questions and the P6-eval.md questions (whole sentence and
keyword form) and --kb-show on chosen sessions against a library copy, and
compare two runs. Output holds session content: keep it OUTSIDE the repository.

  eval.py run  <AgentKB binary> <temp HOME> <out dir> [source:session[@seq] ...]
  eval.py diff <out dir A> <out dir B>

Normalised before comparing: `data_as_of` (a timestamp) and `paths.mirror`
(the one value allowed to change: `.jsonl` -> `.jsonl.packed.zst`; the number
of results whose mirror changed is counted separately).
"""
import json, os, pathlib, subprocess, sys

HERE = pathlib.Path(__file__).resolve().parent
ns = {}
exec((HERE.parent / "p6-evidence" / "queries.py").read_text(encoding="utf-8").split("\ndef ")[0], ns)
QUERIES = []
for (n, sentence, keywords, _) in ns["NEW"]:
    QUERIES += [(f"new{n:02}-sentence", sentence), (f"new{n:02}-keywords", keywords)]
for i, (sentence, keywords, _) in enumerate(ns["OLD"], 1):
    QUERIES += [(f"old{i:02}-sentence", sentence), (f"old{i:02}-keywords", keywords)]


def call(binary, home, args):
    env = dict(os.environ, HOME=home)
    out = subprocess.run([binary, *args], env=env, capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


def normal(value, mirrors):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k == "data_as_of":
                continue
            if k == "mirror" and isinstance(v, str):
                mirrors.append(v)
                continue
            out[k] = normal(v, mirrors)
        return out
    if isinstance(value, list):
        return [normal(v, mirrors) for v in value]
    return value


def run(binary, home, out, sessions):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)
    for name, query in QUERIES:
        raw = call(binary, home, ["--kb-search", query, "--limit", "10"])
        mirrors = []
        data = normal(raw, mirrors)
        (out / f"q-{name}.json").write_text(json.dumps({"result": data, "mirrors": mirrors}, ensure_ascii=False, indent=1))
    for s in sessions:
        # `source:id@seq`: the message at seq (a long one: read from the backup file)
        sel, _, seq = s.partition("@")
        where = ["--around", seq, "--context", "1"] if seq else ["--head", "40"]
        raw = call(binary, home, ["--kb-show", sel, "--full", *where, "--max-chars", "1000000"])
        mirrors = []
        data = normal(raw, mirrors)
        (out / f"show-{s.replace(':', '_').replace('@', '_at_')}.json").write_text(json.dumps({"result": data, "mirrors": mirrors}, ensure_ascii=False, indent=1))
    print(f"{len(QUERIES)} searches, {len(sessions)} shows -> {out}")


def diff(a, b):
    a, b = pathlib.Path(a), pathlib.Path(b)
    names = sorted(p.name for p in a.glob("*.json"))
    differ, mirror_changed = [], 0
    for name in names:
        x = json.loads((a / name).read_text())
        y = json.loads((b / name).read_text())
        if x["result"] != y["result"]:
            differ.append(name)
        mirror_changed += sum(1 for m, n in zip(x["mirrors"], y["mirrors"]) if m != n)
    print(f"compared {len(names)} files: {len(differ)} differ; mirror path changed in {mirror_changed} results")
    for name in differ:
        print("  differs:", name)
    return 1 if differ else 0


if __name__ == "__main__":
    if sys.argv[1] == "run":
        run(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:])
    else:
        sys.exit(diff(sys.argv[2], sys.argv[3]))
