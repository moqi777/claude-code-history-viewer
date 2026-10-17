#!/bin/bash
# Acceptance 9 (the vectors part): a library with a finished semantic index is exported, imported into an
# empty machine, and that machine does not compute the vectors again. Real library COPY, temp HOMEs;
# the export file holds real sessions and is deleted at the end.
#
#   transfer_vectors.sh <BIN> <work dir outside the repo> <HOME with a finished index and the model> [REAL_DATA]
set -u
BIN=$1; T=$2; SRC=$3; REAL=${4:-$HOME/AgentKB}
case "$T" in "$PWD"/*|*/claude-code-history-viewer/*) echo "work dir must be outside the repository"; exit 2;; esac
mkdir -p "$T"
cfg() { mkdir -p "$1/Library/Application Support/agent-kb"; cat > "$1/Library/Application Support/agent-kb/config.json" <<EOF
{"version":1,"data_dir":"$1/AgentKB","schedule_enabled":false,"sources":{"claude":{"enabled":true,"root":"$1/.claude"},"codex":{"enabled":true,"root":"$1/.codex"}},"semantic":{"enabled":$2,"source":"modelscope"}}
EOF
}
A=$T/homeA; B=$T/homeB
mkdir -p "$A/AgentKB" "$A/.claude" "$A/.codex" "$B/.claude" "$B/.codex"
cp -c -R "$REAL/raw" "$A/AgentKB/raw"; cp -c -R "$REAL/notes" "$A/AgentKB/notes"; cp -c "$REAL/kb.db" "$A/AgentKB/kb.db"
cp -c -R "$SRC/AgentKB/vectors" "$A/AgentKB/vectors"; cp -c -R "$SRC/AgentKB/models" "$A/AgentKB/models"
cfg "$A" true
echo "## export with the semantic index"
HOME=$A "$BIN" --kb-export "$T/a.agentkb" | python3 -c "import sys,json; r=json.load(sys.stdin); print('files', r['files'], 'size', r['file_bytes'])"
HOME=$A "$BIN" --kb-export "$T/a-novec.agentkb" --no-vectors | python3 -c "import sys,json; r=json.load(sys.stdin); print('without: files', r['files'], 'size', r['file_bytes'])"
echo "## import into an empty machine B (model there, setting off as a fresh machine has it)"
mkdir -p "$B/AgentKB"; cp -c -R "$SRC/AgentKB/models" "$B/AgentKB/models"
HOME=$B "$BIN" --kb-import "$T/a.agentkb" --no-sync | python3 -c "import sys,json; r=json.load(sys.stdin)['import']; print(r['mode'], 'all_match', r['all_match'], 'vectors_adopted', r['vectors_adopted'])"
python3 - "$B" <<'P'
import json,sys,os
p=os.path.join(sys.argv[1],"Library/Application Support/agent-kb/config.json")
c=json.load(open(p)); print("semantic setting after the import:", c.get("semantic")); c["semantic"]={"enabled":True,"source":"modelscope"}; json.dump(c,open(p,"w"))
P
echo "## B switches it on and runs a builder round: nothing to compute"
HOME=$B "$BIN" --kb-embed --auto | python3 -c "import sys,json; d=json.load(sys.stdin); r=d['state']['last_run']; print(d['outcome'], {k:r[k] for k in ['new','changed','unchanged','removed','embedded']})"
echo "## same answers on A and B"
for q in "电池 耗电" "怎么让外面的人访问到我电脑上跑的服务" "复制粘贴历史管理的小工具为什么这么吃资源"; do
  for h in $A $B; do HOME=$h "$BIN" --kb-search "$q" | python3 -c "
import sys,json
d=json.load(sys.stdin); print('  ', '$(basename $h)', d['mode_used'], d['semantic']['state'], [r['session']['session_id'][:8] for r in d['results'][:5]])"; done
done
echo "## import without vectors: the builder has everything to do"
C=$T/homeC; mkdir -p "$C/.claude" "$C/.codex" "$C/AgentKB"; cp -c -R "$SRC/AgentKB/models" "$C/AgentKB/models"
HOME=$C "$BIN" --kb-import "$T/a-novec.agentkb" --no-sync | python3 -c "import sys,json; r=json.load(sys.stdin)['import']; print(r['mode'], 'vectors_adopted', r['vectors_adopted'])"
HOME=$C "$BIN" --kb-embed --status | python3 -c "import sys,json; print(json.load(sys.stdin))" | cut -c1-120
if [ -z "${KEEP:-}" ]; then rm -rf "${T:?}"; echo "(work dir with real session text deleted)"; fi
