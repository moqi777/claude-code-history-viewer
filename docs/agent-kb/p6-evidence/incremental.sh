#!/bin/bash
# Acceptance 4: a new session and new messages in an old one become searchable by meaning after the next
# collection, and what did not change is not computed again (the log has the counts).
# Synthetic sessions only; a throw-away HOME.
#
#   incremental.sh <BIN> <work dir outside the repo> [MODEL_DIR]
set -eu
BIN=$1; T=$2; MODEL=${3:-/private/tmp/p6work/models/bge-m3}
H=$T/home; rm -rf "$H"
mkdir -p "$H/.claude/projects/-tmp-demo" "$H/.codex/sessions" "$H/AgentKB/models/bge-m3-int8" "$H/Library/Application Support/agent-kb"
cp -c "$MODEL/model.onnx" "$MODEL/tokenizer.json" "$H/AgentKB/models/bge-m3-int8/"
cat > "$H/Library/Application Support/agent-kb/config.json" <<EOF
{"version":1,"data_dir":"$H/AgentKB","schedule_enabled":false,
 "sources":{"claude":{"enabled":true,"root":"$H/.claude"},"codex":{"enabled":true,"root":"$H/.codex"}},
 "semantic":{"enabled":true,"source":"modelscope"}}
EOF
S1=$H/.claude/projects/-tmp-demo/11111111-1111-4111-8111-111111111111.jsonl
S2=$H/.claude/projects/-tmp-demo/22222222-2222-4222-8222-222222222222.jsonl
line() { python3 -c "
import json,sys
role,ts,text,i=sys.argv[1:5]
m={'role':role,'content':text if role=='user' else [{'type':'text','text':text}]}
print(json.dumps({'type':role,'uuid':'u'+i,'timestamp':ts,'cwd':'/tmp/demo','message':m},ensure_ascii=False,separators=(',',':')))" "$@"; }
{ line user 2026-10-01T10:00:00Z "我想把博客的图片放到对象存储上，用哪个服务比较便宜？" 1
  line assistant 2026-10-01T10:00:05Z "可以用阿里云 OSS 或者 Cloudflare R2，R2 没有出口流量费，小站点基本免费。" 2
  line user 2026-10-01T10:01:00Z "那怎么绑定自己的域名？" 3; } > "$S1"
HOME=$H "$BIN" --kb-sync > /dev/null
echo "== first run"; HOME=$H "$BIN" --kb-embed --auto | python3 -c "import sys,json; r=json.load(sys.stdin)['state']['last_run']; print({k:r[k] for k in ['new','changed','unchanged','removed','embedded']})"
# new messages in the old session, and a new session
{ line assistant 2026-10-02T09:00:00Z "绑定域名要在 R2 的设置里添加自定义域，再把 DNS 解析指到 Cloudflare。" 4
  line user 2026-10-02T09:01:00Z "好的，已经能打开图片了，谢谢" 5; } >> "$S1"
{ line user 2026-10-03T08:00:00Z "笔记本合上盖子以后电量掉得特别快，一晚上掉了三成，怎么回事？" 1
  line assistant 2026-10-03T08:00:09Z "可能是休眠时仍有唤醒源，用 pmset -g log 查看谁在唤醒，并关闭 Power Nap。" 2; } > "$S2"
HOME=$H "$BIN" --kb-sync > /dev/null
echo "== second run (after the next collection)"; HOME=$H "$BIN" --kb-embed --auto | python3 -c "import sys,json; r=json.load(sys.stdin)['state']['last_run']; print({k:r[k] for k in ['new','changed','unchanged','removed','embedded']})"
echo "== third run (nothing changed)"; HOME=$H "$BIN" --kb-embed --auto | python3 -c "import sys,json; d=json.load(sys.stdin); r=d['state']['last_run']; print(d['outcome'], {k:r[k] for k in ['new','changed','unchanged','removed','embedded']})"
for q in "续航变短" "图片放在网上的托管服务" "怎么给存图片的地方配自己的网址"; do
  echo "== search: $q"
  HOME=$H "$BIN" --kb-search "$q" | python3 -c "
import sys,json
d=json.load(sys.stdin)
print('  mode_used', d['mode_used'], '| semantic', d['semantic']['state'], d['semantic']['indexed'],'/',d['semantic']['total'])
for r in d['results'][:3]: print('  ', r['session']['session_id'][:8], r.get('match'), r.get('semantic_score'), '|', (r['hits'][0]['snippet'][:50] if r['hits'] else ''))"
done
echo "== log"; grep -h "embed: wanted" "$H"/AgentKB/logs/embed-*.log
