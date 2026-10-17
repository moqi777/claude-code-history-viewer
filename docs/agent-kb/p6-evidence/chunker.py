import re, sqlite3, json, sys

FENCE = re.compile(r"```.*?(?:```|$)", re.S)
SENT = re.compile(r"(?<=[。！？；!?;\n])")
def split_text(text, target=500, max_chunks=6):
    text = FENCE.sub(lambda m: "" if len(m.group(0)) > 200 else m.group(0), text)
    text = re.sub(r"[ \t]+", " ", text)
    paras = [p.strip() for p in re.split(r"\n\s*\n|\n(?=[#\-\*\d|])", text) if p.strip()]
    chunks, cur = [], ""
    def push():
        nonlocal cur
        if cur.strip(): chunks.append(cur.strip())
        cur = ""
    for p in paras:
        parts = [p] if len(p) <= target else [s for s in SENT.split(p) if s]
        for s in parts:
            while len(s) > target:
                if cur: push()
                chunks.append(s[:target]); s = s[target:]
            if len(cur) + len(s) + 1 > target: push()
            cur += ("\n" if cur else "") + s
    push()
    return chunks[:max_chunks]

def build(db_path, out_jsonl, target=500, max_chunks=6, min_len=20):
    db = sqlite3.connect(db_path)
    meta = {}
    n = 0
    with open(out_jsonl, "w") as out:
        # sessions: title
        sess = {r[0]: r[1:] for r in db.execute("select file_id, source, session_id, title, is_subagent from sessions")}
        for fid, (src, sid, title, sub) in sess.items():
            if title and len(title.strip()) >= 4:
                n += 1; meta[n] = (fid, "title", 0)
                out.write(json.dumps({"id": n, "text": title.strip()[:300]}, ensure_ascii=False) + "\n")
        sid_to_fid = {(v[0], v[1]): k for k, v in sess.items()}
        for src, sid, title, prob, cause, sol, concl, kw in db.execute("select source, session_id, title, problem, cause, solution, conclusion, keywords from notes"):
            fid = sid_to_fid.get((src, sid))
            if fid is None: continue
            text = f"{title}\n问题：{prob}\n原因：{cause}\n解决：{sol}\n结论：{concl}\n关键词：{kw}"
            n += 1; meta[n] = (fid, "note", 0)
            out.write(json.dumps({"id": n, "text": text[:1200]}, ensure_ascii=False) + "\n")
        for fid, seq, kind, text in db.execute("select file_id, seq, kind, text from messages where kind in ('user','assistant') order by file_id, seq"):
            if fid not in sess or len(text.strip()) < min_len: continue
            for c in split_text(text, target, max_chunks):
                if len(c) < min_len: continue
                n += 1; meta[n] = (fid, kind, seq)
                out.write(json.dumps({"id": n, "text": c}, ensure_ascii=False) + "\n")
    json.dump({str(k): v for k, v in meta.items()}, open(out_jsonl + ".meta.json", "w"))
    return n

if __name__ == "__main__":
    print(build(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 500, int(sys.argv[4]) if len(sys.argv) > 4 else 6))
