#!/bin/sh
# Fake `claude` for FR-S8: asks one permission (request r1) and waits for the answer on stdin.
# Logs every stdin line to ./stdin.log. Stays alive until stdin closes, like the real CLI.
if [ "$1" = "--version" ]; then echo "9.9.9 (Claude Code)"; exit 0; fi
read -r line; echo "$line" >> stdin.log
echo '{"type":"system","subtype":"init","session_id":"fake-approval"}'
echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
cat >> stdin.log
