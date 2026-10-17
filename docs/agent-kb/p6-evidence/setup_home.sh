#!/bin/bash
# Make a throw-away HOME with semantic search switched on, for trying P6 without touching ~/AgentKB.
#
#   setup_home.sh <work dir outside the repository> [MODEL_DIR] [LIBRARY_DB]
#
# * the library is a copy of one kb.db (default: the real ~/AgentKB/kb.db, only READ and cloned with
#   `cp -c`; for a library of your own pass its kb.db as the third argument)
# * the model files (model.onnx = onnx-community/bge-m3-ONNX onnx/model_int8.onnx, tokenizer.json) come
#   from MODEL_DIR (default /private/tmp/p6work/models/bge-m3) and are cloned into the temp library
# * the source folders are empty, so a collection there collects nothing and never reads ~/.claude or ~/.codex
#
# After it ran, with BIN = the AgentKB binary:
#   HOME=<work dir>/home $BIN --kb-embed --test           # loads the model, embeds three sentences
#   HOME=<work dir>/home $BIN --kb-embed --all            # build every vector now (normal priority)
#   HOME=<work dir>/home $BIN --kb-embed --status         # progress (total/done)
#   HOME=<work dir>/home $BIN --kb-search "一句话的问法"    # default parameters; "semantic" says whether the vectors were used
# No environment variable is needed: the setting is semantic.enabled in the temp HOME's config.json.
set -eu
T=$1
MODEL=${2:-/private/tmp/p6work/models/bge-m3}
DB=${3:-$HOME/AgentKB/kb.db}
case "$T" in "$PWD"/*|*/claude-code-history-viewer/*) echo "work dir must be outside the repository"; exit 2;; esac
H=$T/home
mkdir -p "$H/AgentKB/models/bge-m3-int8" "$H/.claude/projects" "$H/.codex/sessions" "$H/Library/Application Support/agent-kb"
cp -c "$DB" "$H/AgentKB/kb.db"
cp -c "$MODEL/model.onnx" "$H/AgentKB/models/bge-m3-int8/model.onnx"
cp -c "$MODEL/tokenizer.json" "$H/AgentKB/models/bge-m3-int8/tokenizer.json"
cat > "$H/Library/Application Support/agent-kb/config.json" <<EOF
{"version":1,"data_dir":"$H/AgentKB","schedule_enabled":false,
 "sources":{"claude":{"enabled":true,"root":"$H/.claude"},"codex":{"enabled":true,"root":"$H/.codex"}},
 "semantic":{"enabled":true,"source":"modelscope"}}
EOF
echo "HOME for the test: $H"
