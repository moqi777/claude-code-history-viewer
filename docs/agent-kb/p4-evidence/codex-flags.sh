CODEX_ISOLATE=(--ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules -s read-only
  --disable shell_tool --disable unified_exec --disable apps --disable plugins --disable browser_use --disable computer_use
  --disable multi_agent --disable image_generation --disable view_image --disable skill_search --disable hooks --disable goals
  -c 'web_search="disabled"' -c 'mcp_servers={}' -c 'skills.include_instructions=false')
