# Sign in with ChatGPT on a self-hosted ember server (FR-U4)

Status: implemented in `crates/server/src/chatgpt/` (issue #16). Not yet compiled or run. OpenAI's pages
were read on 2026-10-03; the feature is in preview, so recheck them before release.

## What OpenAI documents

All pages are under `developers.openai.com/siwc/`.

| Topic | What the page says | Page |
| ----- | ------------------ | ---- |
| Who may use plan usage | Open-source and locally hosted apps may ask to use the user's ChatGPT plan for Responses API requests. A paid or remotely hosted app needs OpenAI's approval through the interest form (`openai.com/form/sign-in-with-chatgpt-interest/`). | `token-sharing-open-source` |
| Client registration | No client id is issued to Ember in advance. The first sign-in sends `client_id=dynamic_agent_client` and `agent_name_hint=<app name>`. OpenAI then registers a client for that user and workspace and returns its id as `client_id` on the callback. Later sign-ins and refreshes use that id. It is a public client, so there is no client secret. | `token-sharing-open-source/sign-in` |
| Host id | `ext_agent_host_id` must be stable for each host and chosen before the first sign-in. The accepted forms are a JWK thumbprint URN, `urn:uuid:` or `did:key:`. | `token-sharing-open-source` |
| Authorize | `GET https://auth.openai.com/api/accounts/authorize` with `response_type=code`, `client_id`, `redirect_uri`, `scope`, `resource=https://api.openai.com/v1`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256` and `ext_agent_host_id`. A first sign-in also sends `agent_name_hint`; a later one may send `login_hint` or `id_token_hint`. | `…/sign-in` |
| Scopes | `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`. Plan usage requires `chatgpt.tokens.use.direct` in the granted scopes; a valid ID token on its own is not enough. | `…/sign-in`, `…/errors-and-recovery` |
| Redirect | `http://127.0.0.1:<port>/auth/callback`. The scheme, host and path must not change between attempts; only the port may. `localhost` must not be used. | `…/sign-in` |
| Token | `POST https://auth.openai.com/api/accounts/oauth/token`, form-encoded. The code exchange sends `grant_type=authorization_code`, `client_id`, `code`, `code_verifier`, `redirect_uri` and `resource`. A refresh sends `grant_type=refresh_token`, `client_id`, `refresh_token` and `resource`, without `scope`. | `…/sign-in`, `…/profiles-and-sessions` |
| Lifetimes | The access token lasts 1 h (`expires_in: 3600`). The refresh token lasts 30 days, and each refresh replaces it with a new 30-day token. The response may include `earliest_refresh_at`, but its format is not documented. Refreshes must run one at a time. | `…/token-reference`, `…/profiles-and-sessions` |
| Refresh failure | After `invalid_grant`, `invalid_refresh_token`, `token_expired` or `refresh_token_reused`, clear the tokens and sign in again. | `…/errors-and-recovery` |
| Revocation | The `revocation_endpoint` comes from `https://auth.openai.com/.well-known/openid-configuration`. Send `token`, `token_type_hint=refresh_token` and `client_id`. | `…/profiles-and-sessions` |
| Inference | `POST https://api.openai.com/v1/responses` with `Authorization: Bearer`. Requests must set `store: false` and `stream: true`, and `input` must be an array. `GET /v1/models` lists the models; show those with `visibility: "list"`. | `…/models-and-inference`, `…/preview-limitations` |
| Not supported | `previous_response_id` over HTTP. The fields `background`, `conversation`, `max_output_tokens`, `max_tool_calls`, `metadata`, `moderation`, `multi_agent`, `prompt`, `prompt_cache_retention`, `safety_identifier`, `temperature`, `top_logprobs`, `top_p`, `truncation` and `user`. The tools image generation, file search, Code Interpreter, native computer use, hosted MCP/connectors and `tool_search`. Audio/video input and the Files API. Function/custom tools are allowed but go "in namespaces or … through `additional_tools` input items". | `…/preview-limitations` |
| Weekly cap | The user sets each app's share of their weekly ChatGPT usage at `chatgpt.com/settings/usage`. No documented API reports the cap or what remains of it. When the cap is reached, the request fails with `subscription_sharing_usage_limit_exceeded`: HTTP 429 before the stream starts, or a `response.failed` event during it. | `…/errors-and-recovery`, `…/models-and-inference`; dev.to and help.openai.com summaries |
| Other errors | `subscription_sharing_user_not_eligible` (403, do not retry), `subscription_sharing_invalid_user` (401, sign in again), `subscription_sharing_unsupported_capability` (400) and `subscription_sharing_usage_unavailable` (temporary). | `…/errors-and-recovery` |
| Remote VM | The 127.0.0.1 callback reaches the machine running the browser. The docs therefore sign in locally and move the tokens over SSH, and the VM keeps its own host id. | `…/self-hosted-vms` |

## What Ember does

- **Self-hosted only.** When `EMBER_HOSTED=1` is set, every `/api/v1/chatgpt/*` route and
  `/auth/callback` answer 403, and `ChatGpt` refuses every operation.
- **Sign-in.** `POST /api/v1/chatgpt/signin` returns the authorization URL. The PKCE verifier,
  `state` and `nonce` stay in server memory for 10 minutes. The callback route is served by
  ember server itself at `http://127.0.0.1:<listen port>/auth/callback`, and
  `EMBER_CHATGPT_REDIRECT_PORT` can change the port.
- **Browser on another machine.** If the browser does not run on the server's machine, the final
  127.0.0.1 page fails to load. The user copies its URL into
  `POST /api/v1/chatgpt/signin/complete`. Ember does not use OpenAI's local-sign-in-then-SSH
  approach because Ember owns the token store.
- **Callback checks.** The `state` must be known; an unknown state consumes nothing. An `error`
  parameter is reported to the user. A first sign-in must come back with an issued `client_id`,
  and a re-sign-in must come back with that account's own client id. The code exchange sends the
  verifier. Ember checks the ID token's `iss`, `aud`, `exp` and `nonce`, then checks the granted
  scopes.
- **Storage.** Tokens are stored in `chatgpt_accounts`, sealed with the FR-U5 `SecretBox`
  (XChaCha20-Poly1305 with the key in `secret.key`). The AAD is
  `ember/chatgpt-tokens/v1:<account id>`. API responses, the callback page and `Debug` output
  never contain a token.
- **Refresh.** Ember refreshes 5 minutes before the access token expires, one refresh at a time,
  and respects `earliest_refresh_at` unless the token is about to expire. A terminal refresh error
  marks the account `signed_out`.
- **Responses client.** `ChatGpt::responses(account, body)` sets `store`/`stream` itself. It
  rejects, by name, the unsupported fields and tools, a non-array `input`, and unsupported tools
  nested in a namespace, in `tool_choice` or in an `additional_tools` item. It streams the SSE
  events and records `response.completed` usage. On `usage_limit_exceeded` it marks the account
  limited, until `Retry-After` or for 1 h, and stops sending requests until then. Nothing in
  agents or sessions calls it yet.
- **Usage.** Per-day tokens appear in `GET /api/v1/usage` next to the agent accounts.
  `GET /api/v1/chatgpt/accounts` shows each account with `kind: "chatgpt"`, `limited`,
  `tokens_today` and `weekly_cap: {known: false, manage_url}`.

## Open questions

1. **Approval for Ember's distribution.** The docs allow an "open-source and locally hosted"
   app. If Ember is ever distributed or sold in a form that counts as "paid", the interest form
   applies.
2. **ID token signature.** OpenAI's page asks for JWKS verification. Ember checks the claims only;
   OIDC Core §3.1.3.7 allows that for a token received directly from the token endpoint over TLS.
   Adding `jsonwebtoken` (or similar) would close the gap.
3. **Function tools.** The docs ask for function/custom tools to go "in namespaces or … through
   `additional_tools`". The exact namespace shape is not documented, so Ember passes top-level
   function tools through unchanged.
4. **`system` role.** A secondary source (dev.to) says system-role messages are not accepted. The
   official pages read do not say so, so Ember does not enforce it.
5. **`earliest_refresh_at` format.** Ember reads it as Unix seconds, or as ms if the value is that
   large.
6. **Remaining weekly usage.** No API reports it. Ember shows "limited" only after a 429.
