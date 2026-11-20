# 실제 컴퓨터와 계정에 걸친 팀: 1.0 이후 설계 (#82, FR-T7)

상태: 제안. 이 문서는 설계 노트이고 SPEC, INTENT에는 아직 반영하지 않았다. 리더 검토(2026-11-20)의
결정과 소유자에게 남은 질문은 6절에 있다. 기준 커밋은 `develop` 8ea03cb9 (2026-11-20)이다.

## 1. 범위

#82의 완료 기준은 한 문장이다: "실제 Claude Code와 Codex 세션으로 된 팀이 컴퓨터 두 대와 계정 두 개
위에서 돌고, 그 결과가 SPEC에 기록된다."

다루는 것:

- 그 실행을 사람 손이 가장 적게 드는 형태로 만드는 검증 구조 (단계별 하네스, 증거 파일).
- 실제 실행에서 드러날 제품의 빈 곳: 프로필(D30)과 `team spawn`의 어긋남, 컴퓨터를 잃었을 때의 팀
  동작(D20), 컴퓨터마다 다른 프로젝트 경로, 보고(FR-T8)에 실린 실행 위치.
- 같은 하드웨어 준비로 함께 끝낼 수 있는 다른 미검증 항목과의 묶음.

다루지 않는 것:

- 팀 기능 자체의 재설계. 팀, 작업, 우편함, 메시지 규칙은 #55에서 만들어졌고 그대로 둔다.
- 서로 다른 사람(Ember 사용자) 사이의 팀. 여기서 "계정"은 공급자 계정(Claude, ChatGPT)이다
  (6절 Q-T1에서 확인을 구한다).
- 채널과 손님(FR-T10~T17).

## 2. 현재 있는 것

코드에서 확인한 것:

| 조각 | 위치 | 상태 |
| --- | --- | --- |
| 팀 모델과 규칙 | `ember/server/src/a2a/team.rs` (1352줄): `SpawnRequest { agent, account, pipeline, proxy, computer, title }`, `MAX_TEAMMATES = 8` | CI |
| 스크립트 에이전트 테스트 | `ember/server/tests/a2a_team.rs`: `a_leader_spawns_a_teammate_that_keeps_its_own_approvals`, `only_the_leader_spawns_and_ends_teammates`, `tasks_are_shared_and_members_update_their_own`, `mail_reaches_members_and_the_mailbox`, `teammates_are_reachable_only_from_their_team`, `team_traffic_is_loop_protected`, `the_cli_runs_a_team_over_http` 등 | CI |
| CLI | `ember/server/src/a2a/client.rs`: `ember-a2a team spawn <name> [--agent] [--account] [--computer] [--title]`, `team end`, `task`, `mail` | CI |
| 팀원 정의 | `ember/server/src/members.rs`: 하네스와 계정 하나 또는 전환 파이프라인, 선택적 모델 프록시 (D18) | CI |
| 컴퓨터 지정과 복귀 | `ember/server/src/computers/mod.rs`: 지정 컴퓨터(`session_designated`), 탐침 2회 x 10초, 잃으면 메인 서버로, 돌아오면 복귀 (`EMBER_FALLBACK_PROBES`) | 코드에 있다. INTENT D20의 "Status: Not built"는 낡았다 |
| 에이전트별 실제 실행 테스트 | `ember/server/tests/{claude_code,codex,antigravity,acp_gemini,acp_omp}.rs`, `EMBER_E2E_*`로 켠다 | 손으로 돌린다 |
| 실제 서버 스모크 | `ember/server/scripts/real-server-smoke.sh`, `docs/design/REAL-SERVER-RUN.md` | 손으로 돌린다 |

문서에만 있는 것:

- `docs/design/MULTI-COMPUTER-TEAM-RUN.md` (#727): 두 컴퓨터, 두 계정 실행의 절차서. `curl` 명령을
  사람이 순서대로 치고 `jq` 출력을 눈으로 본다. 실행된 적은 없다.

함께 미검증으로 남은 이웃 항목 (SPEC Status 표 기준):

| 항목 | 남은 것 |
| --- | --- |
| FR-X1~X3 | 물리 컴퓨터 두 대 사이의 전환. 같은 Mac의 노드로만 확인됐다 (#6) |
| D19, FR-X6 | 에이전트 API 트래픽이 현재 컴퓨터로 나가는지. 실제 CLI와 컴퓨터로는 미확인 (#130) |
| FR-N1~N3 | NAT 두 개를 건너는 실행 (#10, #229) |
| FR-R1~R3 | #12. Rivet 기준으로 다시 짠 뒤에 잡는다 (4.5). 이 문서의 묶음에서 뺐다 |
| FR-T1 | 서로 다른 컴퓨터와 계정의 Claude Code, Codex 세션이 메시지를 주고받기 |

## 3. 빈 곳

**검증의 빈 곳.**

1. 실제 에이전트로 팀을 돌리는 자동 테스트가 없다. 스크립트 에이전트는 "리더가 `ember-a2a team
   spawn`을 스스로 올바르게 부르는가"와 "Codex 팀원이 받은 A2A 메시지를 일로 받아들이는가"를 증명하지
   못한다. 이 둘은 하드웨어 없이도 확인할 수 있는데 지금은 두 번째 컴퓨터 뒤에 같이 묶여 있다.
2. 절차서는 사람이 명령 스무 개를 치고 판정도 사람이 한다. 다시 돌릴 수 없고 증거가 남지 않는다.
3. "팀원의 도구 호출이 그 컴퓨터에서 돌았다"를 호스트 이름 한 줄로만 판정한다. 파일 도구, API 출구,
   전환 뒤의 상태는 보지 않는다.

**제품의 빈 곳.** 실제 실행에서 걸릴 것으로 보이는 것이다. 3번과 4번은 코드를 끝까지 따라가 확인하지
못했고 작업 분해의 P0이 그 확인이다.

1. **프로필과 어긋난다.** D30은 계정, 파이프라인, 프록시를 팀원에서 프로필로 옮겼다. `team spawn`은
   여전히 `--account`를 받고 `team.rs`에는 프로필이라는 말이 없다. 리더 에이전트가 계정 id를 알아야
   하는 지금 모양은 D30이 버린 "팀원별 설정"이다.
2. **컴퓨터를 잃은 팀원.** D20에 따라 팀원 세션은 메인 서버로 내려와 일을 계속한다. 리더는 그 일이
   B에서 돌았다고 믿는다. 이동은 팀원 세션에만 알림으로 가고 리더와 작업 목록에는 가지 않는다.
3. **프로젝트 경로.** 팀원은 "리더의 프로젝트와 디렉터리"에서 시작한다. 그 절대 경로가 컴퓨터 B에
   없을 때의 동작이 정해져 있지 않다. 절차서는 두 컴퓨터에 같은 경로를 손으로 만들라고 한다. 프로젝트
   마운트(`mount-nfs`, `mount-fuse`)는 CI에서 빌드되지 않는다.
4. **보고에 위치가 없다.** FR-T8의 완료 보고는 결과, 요약, 바뀐 파일을 싣는다. 어느 컴퓨터에서 어느
   계정으로 돌았는지는 싣지 않는다. 팀이 여러 컴퓨터에 걸치면 리더에게 필요한 정보다.
5. **런타임 토큰.** FR-T2a 행은 토큰이 `EMBER_RUNTIME_TOKEN`으로 에이전트 환경에 나간다고 적는다.
   팀원이 다른 계정으로 돌 때도 같은 서버의 A2A 권한을 가진다. 한 사람의 계정들 사이에서는 받아들일 수
   있으나 기록해 둔다.

## 4. 제안 구조

### 4.1 검증을 네 단계로 나눈다

```
 T0 스크립트 에이전트        있다 (a2a_team.rs, CI)
 T1 실제 에이전트, 한 대     새로 만든다. 사람 손: 계정 로그인 두 번 (한 번 해 두면 남는다)
      main(local) + 같은 Mac의 두 번째 ember-node (다른 루트, 다른 표식)
 T2 실제 에이전트, 두 대     T1과 같은 러너, 두 번째 컴퓨터만 진짜. 사람 손: 등록 한 번
 T3 NAT 두 개               T2와 같은 러너, 두 번째 컴퓨터가 다른 망. FR-N의 인수 실행과 같은 날
```

나누는 이유: 실패의 대부분은 T1에서 나온다 (에이전트가 CLI를 잘못 부른다, 계정 디렉터리가 섞인다,
승인이 막힌다). T1은 두 번째 컴퓨터가 없어도 돌고, 로그인이 된 계정 디렉터리만 있으면 에이전트 세션이
혼자 돌릴 수 있다. 소유자의 시간은 T2와 T3의 짧은 실행에만 쓴다.

### 4.2 러너 하나, 증거 파일 하나

절차서의 `curl` 순서를 프로그램 하나로 옮긴다. 자리는 `ember/server/tests/live_team.rs`
(`#[ignore]`, `EMBER_E2E_TEAM=1`)이고, 손으로 부르는 얇은 래퍼가
`ember/server/scripts/team-run.sh`다. 기존 `EMBER_E2E_*` 테스트와 같은 방식이라 새 도구가 필요 없다.

입력 (환경 변수):

| 변수 | 뜻 |
| --- | --- |
| `EMBER_E2E_TEAM_LEAD_ACCOUNT`, `EMBER_E2E_TEAM_MATE_ACCOUNT` | 로그인이 끝난 계정 id 두 개. 러너는 로그인을 하지 않는다 (FR-U1: Ember는 공급자 로그인을 자동화하지 않는다) |
| `EMBER_E2E_TEAM_COMPUTER` | 팀원의 컴퓨터 id. 없으면 러너가 같은 Mac에 두 번째 노드를 띄운다 (T1) |
| `EMBER_E2E_TEAM_DIR` | 두 컴퓨터에 모두 있는 프로젝트 경로 |

하는 일:

1. 두 계정이 `logged_in`인지 확인한다. 아니면 계정별 `login.command`를 출력하고 "건너뜀"으로 끝난다
   (실패가 아니다. 로그인은 사람의 일이다).
2. 프로젝트와 리더 세션(Claude Code, 계정 1, `local`)을 만든다.
3. 리더에게 자연어로 시킨다: 작업을 하나 만들고, Codex 팀원을 계정 2와 컴퓨터 B에 띄워 그 작업을
   맡겨라. 명령 문자열을 그대로 불러 주지 않는다. FR-T2의 "에이전트가 자기 방식으로 닿는다"를 같이
   확인하기 위해서다.
4. 팀원의 일은 판정할 수 있는 것으로 고른다: 러너가 미리 컴퓨터 B의 프로젝트 경로에만 둔 난수 표식
   파일을 읽고, 그 값을 새 파일에 쓰고, 값을 리더에게 우편으로 보내고, 작업을 `done`으로 바꾼다.
5. 판정한다 (4.3).
6. 증거를 `evidence.json`으로 쓴다: 팀 응답, 두 세션의 `account_id`와 컴퓨터, 표식 값, 노드의 실행
   기록, 걸린 시간, 쓴 토큰(FR-U7), 바이너리 버전. 토큰과 계정 자격은 싣지 않는다.

### 4.3 판정 항목

| # | 불변식 | 어떻게 본다 |
| --- | --- | --- |
| I1 | 팀에 리더(Claude Code)와 팀원(Codex)이 있다 | `GET /api/teams` |
| I2 | 계정이 갈려 있다 | 두 세션의 `account_id`. 추가로 두 계정 디렉터리의 세션 파일이 각자 자기 디렉터리에만 생겼는지 본다 |
| I3 | 팀원의 명령이 B에서 돌았다 | 팀원이 보고한 표식 값이 B에만 있는 값과 같다. 노드 B의 실행 기록에 그 세션의 명령이 있고 `local`에는 없다 |
| I4 | 팀원의 파일 쓰기가 B의 디스크에 있다 | 러너가 노드 B의 파일 API로 새 파일을 읽는다 |
| I5 | 작업 목록과 우편함이 맞다 | 작업이 `done`, 우편함에 표식 값, 리더의 대화 기록에 전달된 우편 |
| I6 | 리더가 폴링 없이 보고를 받았다 (FR-T8) | 리더의 기록에 완료 보고가 한 번 있고, 그 사이 리더의 상태 조회 호출이 없다 |
| I7 | 팀원의 API 트래픽이 B로 나갔다 (D19) | T2, T3에서만. 세션의 출구 프록시가 B의 `/egress`로 연 터널 수가 0보다 크다 |
| I8 | 컴퓨터를 잃으면 팀이 안다 | T1에서 두 번째 노드를 죽인다. 팀원이 메인 서버로 내려오고, 리더가 그 사실을 받는다 (4.4의 P2 뒤) |

I1~I6이 #82의 기준이다. I7과 I8은 같은 실행에 얹어 이웃 항목을 함께 닫는다.

### 4.4 제품 쪽 변경

검증이 드러낼 빈 곳을 메우는 작은 변경들이다. 모두 서버 쪽이고 클라이언트 UI에 닿지 않는다.

- **P1. 프로필로 띄우기.** `SpawnRequest`에 `profile`을 더하고 CLI에 `--profile <name>`을 둔다.
  아무것도 주지 않으면 그 하네스의 기본 프로필(프로젝트 덮어쓰기 포함, D30)을 따른다. `--account`는
  남기되 도움말에서 "검증과 디버깅용"으로 내린다. 리더 에이전트의 안내문(`A2a::instructions`)은
  프로필만 말한다. 이유: 에이전트가 계정 id를 다루지 않게 하려는 것이 D30의 뜻이다.
- **P2. 위치 변화를 팀에 알린다.** 팀원 세션이 지정 컴퓨터를 잃어 내려오거나 돌아가면, 팀원에게 가는
  알림과 같은 문장("now working on X (moved from Y)")을 리더에게 A2A 알림으로 한 번 보내고
  `team_updated`에 팀원의 `designated_computer`와 `current_computer`를 싣는다. 무효라는 말은 쓰지
  않는다 (D20).
- **P3. 보고에 위치와 계정.** FR-T8 보고에 `computer`(그 턴이 끝난 컴퓨터), 턴 중 이동이 있었는지,
  `account_label`(id가 아니라 표시 이름)을 더한다. 덧붙이는 필드라 받는 쪽은 그대로다 (D15).
- **P4. 띄울 때 경로를 확인한다.** 팀원의 컴퓨터가 `local`이 아니면 띄우기 전에 그 노드에 프로젝트
  디렉터리가 있는지 묻는다. 없으면 팀원을 만들지 않고 리더에게 이유를 돌려준다 ("computer B에
  `/path`가 없다"). 조용히 엉뚱한 곳에서 시작하는 것보다 낫다. 컴퓨터별 경로 대응표는 이 문서의 범위
  밖이고 Q-T4로 묻는다.
- **P0. 확인.** 위 2~4절의 "확인하지 못했다" 항목을 코드에서 끝까지 따라간다: 경로가 없을 때의 지금
  동작, 보고의 지금 필드, 리더가 팀원의 이동을 지금 볼 수 있는 길. 결과에 따라 P2~P4가 줄 수 있다.

### 4.5 같은 날 묶기

두 번째 컴퓨터를 켜고 등록하는 일은 FR-X3(물리 전환), D19(출구), FR-N(두 NAT)과 겹친다. 러너에
`--suite team,switch,egress`를 두고 한 번의 준비로 차례로 돌린다. 소유자가 두 번째 컴퓨터 앞에
앉는 횟수가 세 번에서 한 번으로 준다.

브라우저(#12, FR-R1~R3)의 인수 실행은 이 묶음에 넣지 않는다 [leader, 2026-11-20]. 그 기준은 서버
Chrome이 아니라 Rivet 기준으로 다시 짜야 하고(`POST-1-0-REMOTE-BROWSER.md` 4.6, R8), Rivet의
egress(Rivet M5)와 Windows 기기를 기다린다. 재계획이 끝나면 같은 러너에 `browser` 묶음을 더한다.

### 4.6 버린 대안

- **절차서를 그대로 두고 소유자가 손으로 돌린다.** 한 번은 되지만 다시 돌릴 수 없고, 에이전트 CLI가
  바뀔 때마다(AGENTS 규칙 4) 회귀를 볼 길이 없다.
- **두 번째 컴퓨터를 컨테이너나 VM으로 대신한다.** T1의 "같은 Mac의 두 번째 노드"가 이미 그 값의
  대부분을 준다. VM은 리더의 허락이 필요하고(Windows VM 규칙) 실제 다른 망을 주지 않는다. T2를
  대신하지 못한다.
- **CI에서 실제 팀을 돌린다.** 공급자 계정의 자격을 CI에 둘 수 없다 (FR-U5). 실제 실행은 소유자의
  기계에서만 한다.

## 5. 위험

| 위험 | 완화 |
| --- | --- |
| 실제 모델은 결정적이지 않다. 리더가 다른 방법으로 일을 끝낼 수 있다 | 판정은 과정이 아니라 결과(I1~I6)로 한다. 세 번 가운데 두 번 통과를 기준으로 하고 실패한 기록도 증거에 남긴다 |
| 토큰 비용 | 작은 모델(haiku급, Codex의 가장 작은 모델)과 한 줄짜리 일을 쓴다. 러너가 쓴 토큰을 증거에 적는다 |
| 승인에서 멈춘다 | 러너가 두 세션에만 approve-all을 켜고 끝나면 끈다. 프로젝트는 임시 디렉터리다 |
| 계정 로그인이 만료된다 | 러너가 먼저 확인하고 "건너뜀"으로 끝낸다. 만료는 실패로 세지 않는다 |
| 러너가 계정 자격이나 토큰을 증거 파일에 흘린다 | 증거에 싣는 필드를 허용 목록으로 둔다. D45의 로그 가리기 규칙을 그대로 쓴다 |
| CLI의 팀 관련 동작이 바뀐다 (Claude Code의 내장 팀 도구가 먼저 불린다 등) | FR-T2의 측정과 같은 문제다. 러너는 내장 수단 호출을 막지 않고 세기만 한다. 결과가 나쁘면 #102 계열로 넘긴다 |
| 같은 Mac의 두 번째 노드는 "다른 컴퓨터"가 아니다 | 그래서 T1은 #82를 닫지 않는다. T2의 증거만 SPEC의 "verified"가 된다 |

## 6. 결정과 열린 질문

**리더 검토 결과 (2026-11-20).** 아래 표가 기준이다. 표 뒤의 질문 본문은 검토 전의 원문이고
선택지와 근거를 남기려고 그대로 둔다. 소유자 질문의 답은 리더가 전한다.

| 질문 | 상태 | 내용 |
| --- | --- | --- |
| Q-T1 | **열림, 소유자** | "계정 두 개"는 한 사람의 공급자 계정 둘인가, 서로 다른 사람의 계정인가. 추천: 한 사람의 계정 둘 |
| Q-T2 | **열림, 소유자** | Claude 계정 하나와 ChatGPT 계정 하나로 검증하는가. 추천: 그렇다. 같은 서비스의 계정 둘은 계정이 있을 때 얹는다 |
| Q-T3 | 결정 [leader, 2026-11-20] | 프로필로 띄운다 (P1). `--account`는 디버깅용으로만 남긴다 (D30) |
| Q-T4 | 결정 [leader, 2026-11-20] | (a) 같은 절대 경로를 요구하고 없으면 이유와 함께 거절한다 (P4). 경로 대응표는 미룬다 |
| Q-T5 | 결정 [leader, 2026-11-20] | (a) D20대로 메인 서버에서 계속하고 리더 에이전트에게 알린다 (P2) |
| Q-T6 | **열림, 소유자** | 두 번째 컴퓨터로 약 20분, 다른 망으로 한 번 더. 다른 검증과 같은 날 묶는가. 추천: 한 날에 묶는다. 브라우저 검증은 Rivet 기준으로 다시 짠 뒤다 |
| Q-T7 | 결정 [leader, 2026-11-20] | 세 번 가운데 두 번 통과. 실패 기록은 증거에 남긴다 |

- **Q-T1. #82의 "계정 두 개"는 무엇인가?** (a) 한 사람의 공급자 계정 두 개 (절차서의 해석),
  (b) 서로 다른 사람의 계정. **권고 (a).** (b)는 사람 사이의 권한 모델이 필요하고 채널 손님
  (FR-T14)의 영역이다.
- **Q-T2. 계정 조합은?** (a) Claude 계정 하나와 ChatGPT 계정 하나, (b) 같은 하네스의 계정 둘도 함께.
  **권고 (a)로 #82를 닫고 (b)는 계정이 있을 때 얹는다.** 이슈의 문장이 "Claude Code and Codex"다.
- **Q-T3. `team spawn`을 프로필 기준으로 바꾸는가 (P1)?** **권고: 바꾼다.** `--account`는 남기고
  안내문에서만 뺀다. D30과 맞추는 일이다.
- **Q-T4. 프로젝트 경로가 컴퓨터마다 다를 때는?** (a) 같은 절대 경로를 요구하고 없으면 거절 (P4),
  (b) 프로젝트에 컴퓨터별 경로 대응표를 둔다. **권고: 지금은 (a).** (b)는 Q1(프로젝트의 컴퓨터들은
  무엇인가)과 함께 정할 일이다.
- **Q-T5. 컴퓨터를 잃은 팀원은?** (a) D20대로 메인 서버에서 계속하고 리더에게 알린다 (P2),
  (b) 팀원은 멈추고 리더가 정한다. **권고 (a).** D20의 "잃은 컴퓨터가 일을 멈추지 않는다"를 팀에도
  그대로 둔다.
- **Q-T6. 실제 실행 날짜와 두 번째 컴퓨터.** T2는 소유자의 두 번째 컴퓨터와 20분쯤이 필요하다. T3은
  다른 망(예: 휴대폰 테더링)이 필요하다. **권고: FR-X3, FR-N의 인수 실행과 같은 날로 잡는다
  (4.5). 브라우저(#12)는 Rivet 재계획 뒤다.**
- **Q-T7. 통과 기준.** 실제 모델이라 한 번의 실패가 있을 수 있다. **권고: 세 번 가운데 두 번 통과,
  실패 기록은 증거에 남긴다.**

## 7. SPEC/INTENT 변경 제안

적용하지 않은 제안문이다. 모두 [provisional]로 넣는다.

**FR-T7 (인수 기준에 덧붙임).**
> *[provisional]* A teammate is spawned by profile (`--profile`, default: the harness's default
> profile, `INTENT.md` D30); `--account` stays for verification. A spawn onto a computer where the
> project directory does not exist is refused with the reason. A teammate's move between its
> designated computer and the main server (D20) is announced to the leader once per move and shown
> in `team_updated`. | Verified in tiers by one runner (`ember/server/tests/live_team.rs`,
> `EMBER_E2E_TEAM=1`): T1 real agents with a second node on the same machine; T2 two physical
> computers; T3 across two NATs. Invariants I1 to I6 of `docs/design/POST-1-0-REAL-TEAMS.md`; the
> evidence file of the T2 run is referenced here.

**FR-T8 (덧붙임).**
> *[provisional]* The report also names the computer the turn ended on, whether the session moved
> during the turn, and the account's label. Additive fields.

**Status 표, FR-T1~T7 행.** "T1 verified on <date> (evidence ...); T2 not verified"처럼 단계별로 적는다.
지금의 "not verified" 한 줄은 어디까지 됐는지 말해 주지 않는다.

**INTENT D20, Status.** "Not built; today only the user switches (#34)"를 지금 코드에 맞게 고친다:
지정 컴퓨터, 탐침, 내려오기와 복귀가 `ember/server/src/computers/mod.rs`에 있고 두 대로는 미검증이다.
이것은 결정의 변경이 아니라 상태 문장의 정정이다.

**INTENT D30 (덧붙임).** "A leader's `team spawn` names a profile, not an account."

**`MULTI-COMPUTER-TEAM-RUN.md`.** 러너가 생기면 3절의 `curl` 순서를 러너 사용법으로 줄이고, 소유자
몫(로그인, 등록)만 남긴다.

## 8. 작업 분해

추정은 에이전트 세션 하나의 작업일 기준이고 거칠다. 빌드와 테스트는 빌더가 돌린다. 모든 단계가 서버와
스크립트이고 클라이언트 UI에 닿지 않으므로, 12월의 빌더 API 제거 이전과 나란히 할 수 있다.

| # | 내용 | 닿는 곳 | 증명하는 테스트 | 추정 | 사람 손 |
| --- | --- | --- | --- | --- | --- |
| P0 | 확인: 경로 없는 컴퓨터로 띄울 때, 보고 필드, 리더가 보는 팀원 위치 | 읽기만 | 이 문서 3절의 정정 | 0.5일 | 없음 |
| P1 | `SpawnRequest.profile`, `--profile`, 안내문 | `ember/server` (`a2a/team.rs`, `a2a/client.rs`, `a2a/mod.rs`) | `fr_t7_spawn_follows_the_harness_profile`, `fr_t7_account_flag_still_works` | 1.5일 | Q-T3 뒤 |
| P2 | 팀원 이동을 리더에게 알림, `team_updated`에 컴퓨터 | `ember/server` (`computers`, `a2a/team.rs`) | `fr_t7_leader_is_told_once_when_a_teammate_falls_back` | 1.5일 | Q-T5 뒤 |
| P3 | FR-T8 보고에 컴퓨터, 이동 여부, 계정 표시 이름 | `ember/server` | `fr_t8_report_names_the_computer_and_account` | 1일 | 없음 |
| P4 | 띄우기 전 경로 확인과 거절 | `ember/server`, `ember/node` (있는 파일 API 사용) | `fr_t7_spawn_is_refused_when_the_directory_is_missing` | 1일 | Q-T4 뒤 |
| V1 | 러너 뼈대: 준비, 정리, 증거 파일, 허용 목록 | `ember/server/tests/live_team.rs`, `scripts/team-run.sh` | 스크립트 에이전트로 러너 자신을 도는 테스트 (CI) | 2일 | 없음 |
| V2 | T1: 같은 Mac의 두 번째 노드, 표식 파일, I1~I6 판정 | 같은 곳 | `EMBER_E2E_TEAM=1`로 통과 | 2일 | 계정 로그인 두 번 (소유자, 10분) |
| V3 | I8: 노드를 죽이고 살리는 경우 | 같은 곳 | T1에서 통과 | 1일 | 없음. P2 뒤 |
| V4 | T2 실행과 SPEC 기록 | `docs/SPEC.md` | 증거 파일, I1~I7 | 0.5일 | 두 번째 컴퓨터 등록과 실행 (소유자, 20분) |
| V5 | T3 실행 (두 NAT) | `docs/SPEC.md` | 증거 파일 | 0.5일 | 다른 망의 컴퓨터 (소유자, 20분). FR-N 인수 실행과 같은 날 |
| D1 | 절차서 줄이기, D20 상태 문장 정정 | `docs/` | 문서 검사 | 0.5일 | 없음 |

합쳐서 12일 안팎이다. P1~P4와 V1은 서로 닿는 파일이 거의 달라 둘로 나눠 나란히 할 수 있다. #82는 V4가
통과하고 SPEC에 기록되면 닫는다.
