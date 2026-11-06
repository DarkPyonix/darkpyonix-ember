#!/bin/sh
# Fake Gemini CLI for FR-A10 (the real CLI is not installed here; its behaviour is UNVERIFIED).
#   --version             prints $FAKE_GEMINI_VERSION (default 0.99.1)
#   --acp                 ACP over stdio; with FAKE_GEMINI_MODE=old it is "unknown" (the old CLI)
#   --experimental-acp    ACP over stdio
# Modes (FAKE_GEMINI_MODE): old (rejects --acp), auth (session/new always answers -32000),
# quota (the first prompt fails with RESOURCE_EXHAUSTED), quota-secret and auth-secret (the same
# with a fake key, an e-mail address and a bearer token in the error). The prompt answer carries no usage.
case "$1" in
  --version) echo "${FAKE_GEMINI_VERSION-0.99.1}"; exit 0 ;;
  --acp)
    if [ "$FAKE_GEMINI_MODE" = old ]; then echo "Unknown argument: acp" >&2; exit 1; fi ;;
  --experimental-acp) ;;
  *) echo "Unknown argument: $1" >&2; exit 1 ;;
esac
# FAKE_GEMINI_ENV_DUMP=<file>: write the environment this process got (FR-A10 env allow-list).
[ -n "$FAKE_GEMINI_ENV_DUMP" ] && env > "$FAKE_GEMINI_ENV_DUMP"
prompts=0
while read -r l; do
  id=$(printf '%s' "$l" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$l" in
    *'"method":"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"protocolVersion":1,"agentCapabilities":{},"authMethods":[]}}' ;;
    *'"method":"session/new"'*)
      if [ "$FAKE_GEMINI_MODE" = auth-secret ]; then
        echo '{"jsonrpc":"2.0","id":'$id',"error":{"code":-32000,"message":"Authentication required for dev.person@example.com with key AIzaSyD-FAKEFAKEFAKEFAKEFAKEFAKEFAKE123","data":{"detail":"Bearer FAKEbearerTOKEN9876543210"}}}'
      elif [ "$FAKE_GEMINI_MODE" = auth ]; then
        echo '{"jsonrpc":"2.0","id":'$id',"error":{"code":-32000,"message":"Authentication required"}}'
      else
        echo '{"jsonrpc":"2.0","id":'$id',"result":{"sessionId":"gem-1"}}'
      fi ;;
    *'"method":"session/prompt"'*)
      prompts=$((prompts + 1))
      if [ "$FAKE_GEMINI_MODE" = quota-secret ] && [ "$prompts" = 1 ]; then
        echo '{"jsonrpc":"2.0","id":'$id',"error":{"code":-32603,"message":"RESOURCE_EXHAUSTED: quota for key AIzaSyD-FAKEFAKEFAKEFAKEFAKEFAKEFAKE123 of dev.person@example.com","data":{"detail":"Bearer FAKEbearerTOKEN9876543210"}}}'
      elif [ "$FAKE_GEMINI_MODE" = quota ] && [ "$prompts" = 1 ]; then
        echo '{"jsonrpc":"2.0","id":'$id',"error":{"code":-32603,"message":"RESOURCE_EXHAUSTED: Quota exceeded"}}'
      else
        echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"gem-1","update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"ok"}}}}'
        echo '{"jsonrpc":"2.0","id":'$id',"result":{"stopReason":"end_turn"}}'
      fi ;;
  esac
done
