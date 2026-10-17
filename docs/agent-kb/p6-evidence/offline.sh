#!/bin/bash
# Acceptance 7: building the index and searching work with every network access denied (macOS sandbox profile),
# and the denial is real (curl fails under the same profile). Only "download model" ever uses the network.
#
#   offline.sh <BIN> <HOME with the model, e.g. the one incremental.sh left behind>
set -u
BIN=$1; H=$2
DENY='(version 1)(allow default)(deny network*)'
echo "== the sandbox really blocks the network:"
sandbox-exec -p "$DENY" curl -s -m 5 -o /dev/null -w "curl exit %{exitcode}\n" https://modelscope.cn 2>&1 | tail -1
rm -rf "$H/AgentKB/vectors"
echo "== build the whole (small) index with the network denied:"
sandbox-exec -p "$DENY" env HOME=$H "$BIN" --kb-embed --auto | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['outcome'], d['state']['done'],'/',d['state']['total'])"
echo "== search with the network denied:"
sandbox-exec -p "$DENY" env HOME=$H "$BIN" --kb-search "续航变短" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['mode_used'], d['semantic']['state'], [r['session']['session_id'][:8]+':'+str(r.get('match')) for r in d['results']])"
echo "== the model test with the network denied:"
sandbox-exec -p "$DENY" env HOME=$H "$BIN" --kb-embed --test
echo "== where the program can reach the network (source): only src-tauri/src/kb/semantic/model.rs uses an HTTP client"
