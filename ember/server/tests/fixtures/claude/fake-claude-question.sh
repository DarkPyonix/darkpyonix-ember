#!/bin/sh
# Fake `claude` for FR-A5 (not a recording; the AskUserQuestion frame has the keys MEASURED on Claude Code
# 2.1.319 on 2026-11-07: tool_name, display_name, input.questions[], tool_use_id, requires_user_interaction). Asks two AskUserQuestion permissions (q1, q2), then two Write permissions (w1, w2),
# each after the previous answer. Logs every stdin line to ./stdin.log.
if [ "$1" = "--version" ]; then echo "9.9.9 (Claude Code)"; exit 0; fi
read -r line; echo "$line" >> stdin.log
echo '{"type":"system","subtype":"init","session_id":"fake-question"}'
Q='"input":{"questions":[{"question":"Which?","header":"H","options":[{"label":"a","description":"A"},{"label":"b","description":"B"}],"multiSelect":false}]}'
echo '{"type":"control_request","request_id":"q1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","requires_user_interaction":true,"tool_use_id":"toolu_fake",'"$Q"'}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"control_request","request_id":"q2","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","requires_user_interaction":true,"tool_use_id":"toolu_fake",'"$Q"'}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"control_request","request_id":"w1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"a","content":"x"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"control_request","request_id":"w2","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"b","content":"y"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
cat >> stdin.log
