#!/bin/sh
# Fake `claude` for FR-A5 plans. Asks ExitPlanMode twice (p1, p2), each after the previous answer,
# with the key MEASURED on Claude Code 2.1.319 (requires_user_interaction: true). Logs stdin.
if [ "$1" = "--version" ]; then echo "9.9.9 (Claude Code)"; exit 0; fi
read -r line; echo "$line" >> stdin.log
echo '{"type":"system","subtype":"init","session_id":"fake-plan"}'
echo '{"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","display_name":"ExitPlanMode","requires_user_interaction":true,"tool_use_id":"toolu_fake","input":{"plan":"do it","planFilePath":"/x/plan.md"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"control_request","request_id":"p2","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","display_name":"ExitPlanMode","requires_user_interaction":true,"tool_use_id":"toolu_fake","input":{"plan":"again","planFilePath":"/x/plan.md"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
cat >> stdin.log
