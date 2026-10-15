#!/bin/zsh
# usage: run_codex.sh <prompt-file> <out-prefix> [model]
cd /Users/mo7/Dev/JavaCode/claude-code-history-viewer/scratch/p4/work
source ../codex-flags.sh
ARGS=("${CODEX_ISOLATE[@]}" --disable code_mode_host --disable sleep_tool --disable tool_suggest --disable shell_snapshot --json --output-schema ../schema-codex.json -o "$2.last")
[[ -n "$3" ]] && ARGS+=(-m "$3")
start=$(python3 -c 'import time;print(time.time())')
perl -e 'alarm 280; exec @ARGV' codex exec "${ARGS[@]}" - < "$1" > "$2.jsonl" 2> "$2.err"
echo "exit=$? secs=$(python3 -c "import time;print(round(time.time()-$start,1))")"
grep -E '"turn\.(completed|failed)"|"type":"error"' "$2.jsonl" | cut -c1-400
