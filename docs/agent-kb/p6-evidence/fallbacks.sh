#!/bin/bash
# Acceptance 3: with the model deleted, a model file changed by one byte, a damaged vector file, no vector file and a
# half-built index, --kb-search still answers (exit 0, keyword results) and says why.
#
#   fallbacks.sh <BIN> <HOME of a test with a finished index and the model> <work dir outside the repo>
set -u
BIN=$1; H=$2; W=$3
mkdir -p "$W"; M="$H/AgentKB/models/bge-m3-int8"; V="$H/AgentKB/vectors"
show() { HOME=$H "$BIN" --kb-search "电池 耗电" > "$W/out.json"; rc=$?
  python3 - "$1" "$rc" "$W/out.json" <<'P'
import json,sys
d=json.load(open(sys.argv[3]))
print(f"{sys.argv[1]:42} exit {sys.argv[2]}  mode_used={d['mode_used']:7} semantic={d['semantic']['state']:15} results={len(d['results'])}  first={d['results'][0]['session']['session_id'][:8]}  note=\"{d['semantic']['note'][:70]}\"")
P
}
show "0. normal"
mv "$M" "$W/model-away"; show "1. model folder deleted"; mv "$W/model-away" "$M"
cp -c "$M/model.onnx" "$W/model.keep"; python3 - "$M/model.onnx" <<'P'
import sys
with open(sys.argv[1],'r+b') as f:
    f.seek(300_000_000); b=f.read(1); f.seek(300_000_000); f.write(bytes([b[0]^1]))
P
show "2. one byte of the model changed"; rm "$M/model.onnx"; cp -c "$W/model.keep" "$M/model.onnx"; rm "$W/model.keep"
F="$V/bge-m3-int8-v1.sqlite3"; cp -c "$F" "$W/vec.keep"
python3 - "$F" <<'P'
import sys
with open(sys.argv[1],'r+b') as f:
    f.seek(0); f.write(b"this is not a sqlite file at all, just text.....")
P
show "3. vector file damaged"; rm -f "$F" "$F-wal" "$F-shm"; cp -c "$W/vec.keep" "$F"
mv "$F" "$W/vec.away"; rm -f "$F-wal" "$F-shm"; show "4. vector file missing"; mv "$W/vec.away" "$F"
# half built: keep every 3rd row only and say the index should have had more
cp -c "$F" "$W/vec.full"
sqlite3 "$F" "DELETE FROM chunks WHERE rowid % 3 != 0"
python3 - "$V/embed_state.json" <<'P'
import json,sys
s=json.load(open(sys.argv[1])); s['status']='running'; json.dump(s,open(sys.argv[1],'w'))
P
show "5. half built (a third of the vectors)"
rm -f "$F" "$F-wal" "$F-shm"; cp -c "$W/vec.full" "$F"; rm "$W/vec.full" "$W/vec.keep"; show "6. restored"
rm -f "$W/out.json"
