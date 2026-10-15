#!/bin/zsh
# usage: run_claude.sh <input-file> <out-file> [model]
cd /Users/mo7/Dev/JavaCode/claude-code-history-viewer/scratch/p4/work
M=${3:-}
ARGS=(-p --no-session-persistence --safe-mode --tools "" --strict-mcp-config --mcp-config '{"mcpServers":{}}' --disable-slash-commands --setting-sources ""
  --system-prompt "$(cat ../prompt_system.txt)" --output-format json --json-schema "$(cat ../schema.json)" --max-budget-usd 2)
[[ -n "$M" ]] && ARGS+=(--model "$M")
start=$(python3 -c 'import time;print(time.time())')
claude "${ARGS[@]}" < "$1" > "$2" 2> "$2.err"
echo "exit=$? secs=$(python3 -c "import time;print(round(time.time()-$start,1))")"
python3 - "$2" <<'PY'
import sys,json
d=json.load(open(sys.argv[1])); u=d['usage']
print('is_error',d['is_error'],'status',d.get('api_error_status'),'turns',d['num_turns'],'tokens in/cc/cr/out',u['input_tokens'],u['cache_creation_input_tokens'],u['cache_read_input_tokens'],u['output_tokens'],'cost',d['total_cost_usd'],'model',list(d['modelUsage']))
print('keys',[k for k in d if k in ('structured_output','result')])
PY
