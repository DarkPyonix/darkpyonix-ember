#!/bin/sh
# Fake `claude` for FR-A5 questions. Emits the AskUserQuestion control_request RECORDED on Claude
# Code 2.1.319 (fixture question_answered_2.1.319.jsonl, request id 364602b5-...), reads our reply
# from stdin and appends it to ./stdin.log, then ends the turn. Logs every stdin line.
if [ "$1" = "--version" ]; then echo "9.9.9 (Claude Code)"; exit 0; fi
read -r line; echo "$line" >> stdin.log
echo '{"type":"system","subtype":"init","session_id":"fake-question-reply"}'
echo '{"type":"control_request","request_id":"364602b5-0a1e-4b36-9dc1-8910841b10ac","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","input":{"questions":[{"question":"Which colour do you prefer?","header":"Colour","options":[{"label":"Red","description":"Choose red"},{"label":"Blue","description":"Choose blue"}],"multiSelect":false}]},"tool_use_id":"toolu_01GTpKxVBuYKnSb5QGoaQ2WS","requires_user_interaction":true}}'
read -r line; echo "$line" >> stdin.log
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
cat >> stdin.log
