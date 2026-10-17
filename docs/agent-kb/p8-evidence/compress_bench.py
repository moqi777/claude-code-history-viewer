#!/usr/bin/env python3
"""P8 measurement: compress every plain file under a COPY of raw/ with the zstd CLI (single thread, like the zstd crate),
and write one line per file: rel, plain size, compressed size, seconds.

usage: compress_bench.py <raw copy dir> <level> <out.tsv> [--verify]
Never run it on the real ~/AgentKB (output goes next to the copy's parent, not into raw/).
--verify decompresses each result and compares sha256 with the input.
"""
import hashlib, os, resource, subprocess, sys, time
raw, level, out = sys.argv[1], sys.argv[2], sys.argv[3]
verify = "--verify" in sys.argv
def sha(cmd):
    h = hashlib.sha256()
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE)
    for b in iter(lambda: p.stdout.read(1 << 20), b""): h.update(b)
    p.wait(); return h.hexdigest()
tot_in = tot_out = 0; t_all = time.time()
with open(out, "w") as o:
    for d, _, fs in os.walk(raw):
        for f in sorted(fs):
            if f.endswith(".zst"): continue
            p = os.path.join(d, f); rel = os.path.relpath(p, raw); n = os.path.getsize(p)
            t = time.time()
            c = subprocess.run(["zstd", "-q", "-T1", f"-{level}", "-c", p], capture_output=True).stdout
            dt = time.time() - t
            if verify:
                tmp = out + ".tmp"; open(tmp, "wb").write(c)
                assert sha(["zstd", "-dc", tmp]) == sha(["cat", p]), rel
                os.remove(tmp)
            o.write(f"{rel}\t{n}\t{len(c)}\t{dt:.3f}\n"); tot_in += n; tot_out += len(c)
peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1048576
print(f"level {level}: {tot_in/1e9:.3f} GB -> {tot_out/1e9:.3f} GB ({tot_in/tot_out:.2f}x), {time.time()-t_all:.1f} s, peak child RSS {peak:.0f} MB")
