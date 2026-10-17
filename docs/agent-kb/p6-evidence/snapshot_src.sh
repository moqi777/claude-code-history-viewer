#!/bin/bash
# Acceptance 8: ~/.claude and ~/.codex are not changed. A running Claude Code / Codex keeps writing its own
# live session files, so the snapshot covers the files that had been idle for 60 minutes when it was taken
# (path, size, modification time), and the "after" snapshot is limited to the paths of the "before" one.
#   snapshot_src.sh before <list file>   |   snapshot_src.sh after <list file>
set -eu
mode=$1; list=$2
if [ "$mode" = before ]; then
  find ~/.claude ~/.codex -type f -mmin +60 2>/dev/null | sort > "$list"
fi
while IFS= read -r f; do stat -f '%N %z %m' "$f" 2>/dev/null || echo "$f MISSING"; done < "$list" | shasum -a 256 | cut -c1-16
wc -l < "$list" | xargs echo "files in the snapshot:"
