#!/bin/sh
# Fake ACP agent for FR-S8: answers initialize, session/new and session/prompt, then streams
# "tick N " agent_message_chunk updates (N in 1..$FAKE_TICKS, default 25, every 0.2 s) and answers
# the prompt with stopReason end_turn. Request ids follow the adapter's order: 0, 1, 2.
# With FAKE_TOOLS=1 (#310) it announces tool call t1 before the ticks and, after them, moves t1 to
# in_progress, announces and completes t2, then completes t1.
if [ "$1" = "--version" ]; then echo "fake-acp 9.9.9"; exit 0; fi
read -r l
echo '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":1,"agentCapabilities":{}}}'
read -r l
echo '{"jsonrpc":"2.0","id":1,"result":{"sessionId":"acp-fake"}}'
read -r l
upd() {
  echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-fake","update":'"$1"'}}'
}
if [ "${FAKE_TOOLS:-}" = "1" ]; then
  upd '{"sessionUpdate":"tool_call","toolCallId":"t1","title":"build","kind":"execute","status":"pending"}'
fi
i=1
while [ "$i" -le "${FAKE_TICKS:-25}" ]; do
  echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-fake","update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"tick '$i' "}}}}'
  i=$((i + 1))
  sleep 0.2
done
if [ "${FAKE_TOOLS:-}" = "1" ]; then
  upd '{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress"}'
  upd '{"sessionUpdate":"tool_call","toolCallId":"t2","title":"list","kind":"read","status":"pending"}'
  upd '{"sessionUpdate":"tool_call_update","toolCallId":"t2","status":"completed","content":[{"type":"content","content":{"type":"text","text":"two"}}]}'
  upd '{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"one"}}]}'
fi
echo '{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}'
cat > /dev/null
