#!/bin/sh
# Stand-in for `claude -p` and `codex exec` in the digest tests. Never calls
# an AI. It always reads its stdin to EOF first (like the real CLIs, which
# hang when the caller leaves stdin open), so an engine that forgets to close
# the pipe makes the test time out.
#
# Environment: FAKE_AI_DIR (required) holds the control and record files:
#   mode         what to do (default "ok"); mode-<n> overrides it for call n
#   count        number of calls so far
#   args-<n>     argv of call n, one per line
#   input-<n>    stdin of call n
#   cwd-<n>      working directory of call n
# Modes: ok, nothing, quota, auth, bad_json, bad_range, leaky, fail, slow, update

case "$1" in
  --version) echo "fake-ai 9.9.9"; exit 0 ;;
esac

dir="${FAKE_AI_DIR:?FAKE_AI_DIR is not set}"
n=$(( $(cat "$dir/count" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$dir/count"
printf '%s\n' "$@" > "$dir/args-$n"
pwd > "$dir/cwd-$n"
cat > "$dir/input-$n"

mode=$(cat "$dir/mode-$n" 2>/dev/null || cat "$dir/mode" 2>/dev/null || echo ok)

codex=0
out=""
if [ "$1" = "exec" ]; then
  codex=1
  prev=""
  for a in "$@"; do
    if [ "$prev" = "-o" ]; then out="$a"; fi
    prev="$a"
  done
fi

from=$(sed -n 's/.*<messages from=#\([0-9][0-9]*\) to=#\([0-9][0-9]*\)>.*/\1/p' "$dir/input-$n" | head -1)
to=$(sed -n 's/.*<messages from=#\([0-9][0-9]*\) to=#\([0-9][0-9]*\)>.*/\2/p' "$dir/input-$n" | head -1)
from=${from:-1}
to=${to:-1}
prior=""
if grep -q '<previous_notes>' "$dir/input-$n"; then prior=" after earlier notes"; fi

entries() {
  printf '{"entries":[{"id":null,"title":"Fake note %s seq %s-%s","problem":"problem text","cause":"cause text","solution":"solution text","conclusion":"fake conclusion%s","keywords":["fake","note"],"seq_from":%s,"seq_to":%s}]}' \
    "$n" "$from" "$to" "$prior" "$from" "$to"
}

emit_claude() { # $1 = structured JSON or empty, $2 = result text
  usage='"usage":{"input_tokens":100,"cache_creation_input_tokens":10,"cache_read_input_tokens":5,"output_tokens":20},"modelUsage":{"fake-model":{}},"total_cost_usd":0.001'
  if [ -n "$1" ]; then
    printf '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"structured_output":%s,%s}\n' "$1" "$usage"
  else
    printf '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"%s",%s}\n' "$2" "$usage"
  fi
}

emit_codex() { # $1 = final message
  [ -n "$out" ] && printf '%s' "$1" > "$out"
  echo '{"type":"thread.started","thread_id":"fake"}'
  echo '{"type":"turn.completed","usage":{"input_tokens":115,"cached_input_tokens":5,"output_tokens":20,"reasoning_output_tokens":0}}'
}

emit() { # $1 = JSON text of the answer
  if [ "$codex" = 1 ]; then emit_codex "$1"; else emit_claude "$1" ""; fi
}

case "$mode" in
  ok) emit "$(entries)" ;;
  update) emit "$(entries)" ;;
  custom) emit "$(cat "$dir/answer.json")" ;;
  nothing) emit '{"entries":[]}' ;;
  bad_json)
    if [ "$codex" = 1 ]; then emit_codex "sorry, I cannot do that"; else emit_claude "" "sorry, I cannot do that"; fi ;;
  bad_range)
    emit '{"entries":[{"id":null,"title":"Out of range","problem":"p","cause":"","solution":"","conclusion":"c","keywords":["k"],"seq_from":900000,"seq_to":900001}]}' ;;
  leaky)
    emit "{\"entries\":[{\"id\":null,\"title\":\"Leaky\",\"problem\":\"p\",\"cause\":\"\",\"solution\":\"set password=hunter2hunter2 in the file\",\"conclusion\":\"c\",\"keywords\":[\"k\"],\"seq_from\":$from,\"seq_to\":$to}]}" ;;
  quota)
    if [ "$codex" = 1 ]; then
      echo '{"type":"error","message":"You'"'"'ve hit your usage limit. Upgrade to Pro or try again at 11:56 PM."}'
      echo '{"type":"turn.failed","error":{"message":"You'"'"'ve hit your usage limit. Upgrade to Pro or try again at 11:56 PM."}}'
    else
      echo '{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"You'"'"'ve hit your session limit · resets 8:20pm (Asia/Shanghai)","usage":{"input_tokens":0,"output_tokens":0}}'
    fi
    exit 1 ;;
  auth)
    if [ "$codex" = 1 ]; then
      echo '{"type":"turn.failed","error":{"message":"401 Unauthorized: please log in again"}}'
    else
      echo '{"type":"result","is_error":true,"api_error_status":401,"result":"Invalid API key · Please run /login","usage":{"input_tokens":0,"output_tokens":0}}'
    fi
    exit 1 ;;
  fail) echo "boom" >&2; exit 1 ;;
  slow) sleep 30; emit "$(entries)" ;;
  *) echo "unknown fake mode $mode" >&2; exit 3 ;;
esac
