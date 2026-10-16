#!/bin/bash
# P5 acceptance evidence, end to end, on a COPY of the real library in a temp folder.
#
#   run_all.sh <AgentKB binary> <work dir outside the repository>
#
# Everything runs with HOME pointed into the work dir, so the real ~/AgentKB, the real
# config.json and the real skills are never written. The real ~/.claude and ~/.codex are read
# (never written) by the "D" machine only, to show a merge into a library with live sources.
# The export files hold real sessions: the work dir is deleted at the end (set KEEP=1 to keep it).
set -u
BIN=$1
T=$2
EV=$(cd "$(dirname "$0")" && pwd)
REAL=${REAL_DATA:-$HOME/AgentKB}
REALHOME=$HOME
mkdir -p "$T"
case "$T" in "$PWD"/*|*/claude-code-history-viewer/*) echo "work dir must be outside the repository"; exit 2;; esac

cfg() { # <home> <data dir> <claude root> <codex root>
  mkdir -p "$1/Library/Application Support/agent-kb"
  cat > "$1/Library/Application Support/agent-kb/config.json" <<EOF
{"version":1,"data_dir":"$2","schedule_enabled":false,"sources":{"claude":{"enabled":true,"root":"$3"},"codex":{"enabled":true,"root":"$4"}},"exclude_cwd_prefixes":["$REALHOME/Dev/JavaCode/claude-code-history-viewer/scratch"]}
EOF
}
as() { local h=$1; shift; HOME=$h "$BIN" "$@"; }
treesum() { (cd "$1" && find raw notes -type f -print0 | sort -z | xargs -0 shasum -a 256); }

echo "## A: a copy of the real library (APFS clones, instant)"
A=$T/homeA; mkdir -p "$A/AgentKB"
cp -c -R "$REAL/raw" "$A/AgentKB/raw"; cp -c -R "$REAL/notes" "$A/AgentKB/notes"; cp -c "$REAL/kb.db" "$A/AgentKB/kb.db"
cfg "$A" "$A/AgentKB" "$A/.claude" "$A/.codex"

# The copy still carries the real machine's idea of where things are (exclusion list applied under the
# real data folder). One collection on A, whose sources are empty, brings that in line with A's own
# folder, as it would have been on a machine that had been running there.
as "$A" --kb-sync >/dev/null 2>&1

echo "## 1. export"
( time as "$A" --kb-export "$T/a.agentkb" ) 2>&1 | grep -v '^export ' | cut -c1-400
ls -l "$T/a.agentkb" | awk '{print "file size:", $5}'

echo "## 2. import into an empty machine B (another user name), then compare"
B=$T/homeB; mkdir -p "$B/.claude/projects" "$B/.codex/sessions"
( time as "$B" --kb-import "$T/a.agentkb" --no-sync 2>/dev/null | python3 -c "import json,sys;r=json.load(sys.stdin)['import'];print(r['mode'],'all_match',r['all_match']);print(r['checks'])" ) 2>&1
as "$B" --kb-sync >/dev/null 2>&1
python3 "$EV/compare.py" "$BIN" "$A" "$B"

echo "## 3. every raw/ and notes/ file has the same SHA-256"
treesum "$A/AgentKB" > "$T/a.sums"; treesum "$B/AgentKB" > "$T/b.sums"
wc -l < "$T/a.sums" | xargs echo "files in A:"; cmp "$T/a.sums" "$T/b.sums" && echo "identical in B"

echo "## 4. one changed bit: refused, and B (which holds data) is unchanged"
cp -c "$T/a.agentkb" "$T/tampered.agentkb"
python3 - "$T/tampered.agentkb" <<'EOF'
import sys
with open(sys.argv[1], 'r+b') as f:
    f.seek(1_000_000_123); b = f.read(1); f.seek(1_000_000_123); f.write(bytes([b[0] ^ 1]))
EOF
before=$(cd "$B/AgentKB" && find . -type f ! -name '*.lock' -print0 | sort -z | xargs -0 shasum -a 256 | shasum -a 256)
as "$B" --kb-import "$T/tampered.agentkb" --no-sync 2>&1 | tail -2 | cut -c1-300
after=$(cd "$B/AgentKB" && find . -type f ! -name '*.lock' -print0 | sort -z | xargs -0 shasum -a 256 | shasum -a 256)
[ "$before" = "$after" ] && echo "B unchanged"; ls -A "$B/AgentKB" | tr '\n' ' '; echo
rm -f "$T/tampered.agentkb"

echo "## 5. kill -9 in the middle of an import into an empty machine C, then import again"
C=$T/homeC; mkdir -p "$C/.claude/projects" "$C/.codex/sessions"
( HOME="$C" exec "$BIN" --kb-import "$T/a.agentkb" --no-sync >/dev/null 2>&1 ) & PID=$!
sleep 7; kill -9 $PID; wait $PID 2>/dev/null; echo "killed the importing process (pid $PID) after 7 seconds"
ls -A "$C/AgentKB" | tr '\n' ' '; echo "(only the work folder is there)"
test ! -e "$C/AgentKB/raw" && test ! -e "$C/AgentKB/kb.db" && test ! -e "$C/Library" && echo "no raw/, kb.db or config of the half import"
as "$C" --kb-import "$T/a.agentkb" --no-sync 2>/dev/null | python3 -c "import json,sys;r=json.load(sys.stdin)['import'];print(r['mode'],'all_match',r['all_match'])"
treesum "$C/AgentKB" | cmp - "$T/a.sums" && echo "C identical to A"
python3 "$EV/compare.py" "$BIN" "$A" "$C" | tail -1

echo "## 6. another user: the file says it came from /Users/<name>; F is a different user"
echo "(the file is rewritten by the ignored helper test repack_as_another_machine in import_tests.rs)"
REPACKED=$T/a-as-user.agentkb
if [ -n "${REPACK_CARGO_DIR:-}" ]; then
  ( cd "$REPACK_CARGO_DIR" && AGENTKB_REPACK_FROM="$T/a.agentkb" AGENTKB_REPACK_TO="$REPACKED" AGENTKB_AS_HOME="$REALHOME" \
    CCHV_TEST_HOME="$T/homeB" cargo test --release --lib repack_as_another_machine -- --ignored >/dev/null 2>&1 )
fi
if [ -f "$REPACKED" ]; then
  F=$T/user2; mkdir -p "$F/.claude/projects" "$F/.codex/sessions"
  as "$F" --kb-import "$REPACKED" --no-sync 2>/dev/null | python3 -c "import json,sys;r=json.load(sys.stdin)['import'];print(r['mode'],'all_match',r['all_match'],r['added_exclusions'])"
  as "$F" --kb-sync >/dev/null 2>&1
  sqlite3 "$F/AgentKB/kb.db" "select 'elsewhere', count(*) from files where origin is not null and source_status='present' union all select 'deleted', count(*) from files where source_status='deleted' union all select 'excluded by cwd', count(*) from sessions where cwd_excluded=1"
  python3 "$EV/compare.py" "$BIN" "$A" "$F" | tail -1
fi

echo "## 7. merge into a machine with its own data (frozen corpus: A's sessions without the notes)"
E=$T/homeE; mkdir -p "$E/AgentKB" "$E/.claude" "$E/.codex"
cp -c -R "$A/AgentKB/raw" "$E/AgentKB/raw"; cp -c "$A/AgentKB/kb.db" "$E/AgentKB/kb.db"
sqlite3 "$E/AgentKB/kb.db" "DROP TABLE notes_fts; DROP TABLE notes; DROP TABLE digest_sessions; DELETE FROM meta WHERE key='notes_index_version';"
cfg "$E" "$E/AgentKB" "$E/.claude" "$E/.codex"
as "$E" --kb-import "$T/a.agentkb" --plan 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print('preview:',d['mode'],d['summary'])"
as "$E" --kb-import "$T/a.agentkb" --no-sync 2>/dev/null | python3 -c "import json,sys;r=json.load(sys.stdin)['import'];print(r['mode'],'all_match',r['all_match']);print(r['checks'])"
as "$E" --kb-import "$T/a.agentkb" --no-sync 2>/dev/null | python3 -c "import json,sys;r=json.load(sys.stdin)['import'];s=r['summary'];print('second time:',{k:s[k] for k in ['sessions_added','files_added','extended','notes_added','notes_identical']})"
python3 "$EV/compare.py" "$BIN" "$A" "$E" --ignore-note-scores | tail -1

echo "## 8. merge into D: a machine whose own collection reads the real ~/.claude and ~/.codex (read only)"
D=$T/homeD; mkdir -p "$D/AgentKB"
cfg "$D" "$D/AgentKB" "$REALHOME/.claude" "$REALHOME/.codex"
as "$D" --kb-sync 2>/dev/null | python3 -c "import json,sys
for l in sys.stdin:
    if l.startswith('{'): d=json.loads(l); print('D collected', d['new_sessions'], 'sessions', d['copied_bytes'], 'bytes')"
treesum "$D/AgentKB" > /dev/null 2>&1
( cd "$D/AgentKB" && find raw -type f -print0 | sort -z | xargs -0 shasum -a 256 ) > "$T/d-before.sums"
as "$D" --kb-import "$T/a.agentkb" --plan 2>/dev/null | python3 -c "import json,sys;d=json.load(sys.stdin);print('preview:',d['summary'])"
as "$D" --kb-import "$T/a.agentkb" 2>/dev/null | python3 -c "import json,sys;r=json.load(sys.stdin)['import'];print(r['mode'],'all_match',r['all_match'],r['checks'])"
( cd "$D/AgentKB" && find raw -type f -print0 | sort -z | xargs -0 shasum -a 256 ) > "$T/d-after.sums"
python3 - "$T/d-before.sums" "$T/d-after.sums" <<'EOF'
import sys
b=dict(l.split("  ",1)[::-1] for l in open(sys.argv[1]).read().splitlines())
a=dict(l.split("  ",1)[::-1] for l in open(sys.argv[2]).read().splitlines())
print("D files before",len(b),"after",len(a),"missing",len([k for k in b if k not in a]),"changed",[k.rsplit('/',1)[-1][:8] for k in b if a.get(k)!=b[k]],"new",len(set(a)-set(b)))
print("(a changed file is a live session that grew; its old bytes stay the start of it)")
EOF

echo "## 9. a collection runs every second while D is exported"
as "$D" --kb-export "$T/d.agentkb" >/dev/null 2>&1 & EP=$!
n=0; ok=0
while kill -0 $EP 2>/dev/null; do
  n=$((n+1))
  st=$(as "$D" --kb-sync 2>&1 | grep '^{' | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['status'])")
  [ "$st" = ok ] && ok=$((ok+1)); sleep 1
done
wait $EP; echo "export exit $?; collections during the export: $n, status ok: $ok"
rm -f "$T/d.agentkb"

echo "## 10. a digest is running: the export is refused"
python3 - "$D/AgentKB/digest.lock" <<'EOF' &
import fcntl, json, os, sys, time, datetime
f=open(sys.argv[1],'r+b'); fcntl.flock(f, fcntl.LOCK_EX|fcntl.LOCK_NB)
f.truncate(0); f.write(json.dumps({"pid":os.getpid(),"started_at":datetime.datetime.utcnow().isoformat()+"Z"}).encode()); f.flush(); time.sleep(5)
EOF
sleep 1; as "$D" --kb-export "$T/d2.agentkb" 2>&1 | head -1 | cut -c1-200; test ! -e "$T/d2.agentkb" && echo "no file written"; wait

echo "## 11. search time on B"
for q in "电池 耗电" "死锁" "502 网关"; do /usr/bin/time -p env HOME="$B" "$BIN" --kb-search "$q" 2>&1 >/dev/null | grep real | sed "s/^/$q: /"; done

if [ -z "${KEEP:-}" ]; then
  echo "## cleanup: deleting the work dir (it holds real session text)"
  rm -rf "${T:?}"
fi
