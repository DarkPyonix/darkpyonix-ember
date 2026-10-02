#!/bin/sh
# Fake `claude` for adapter plumbing tests (not a recording). Speaks the stream-json shapes the
# adapter expects and logs every stdin line it receives to ./stdin.log in its working directory.
if [ "$1" = "--version" ]; then echo "9.9.9 (Claude Code)"; exit 0; fi
echo "$@" > args.log
read -r line; echo "$line" >> stdin.log
echo '{"type":"system","subtype":"init","session_id":"fake-session"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"a","content":"x"}}]}}'
echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"a","content":"x"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"b","content":"y"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"control_request","request_id":"r3","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":3,"output_tokens":4}}'
# Stay alive until stdin closes, like the real CLI.
cat >> stdin.log
