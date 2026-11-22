/* Demo data for the Ember mockup. Everything here is made up. */
window.EMBER_DEMO = {
 me: "BREW",
 people: {
  "BREW": {av:"B", cls:"me"}, "Jun": {av:"J", cls:"p2"},
  "Claude Code": {av:"C", cls:"claude", agent:true}, "Codex": {av:"X", cls:"codex", agent:true}, "Gemini CLI": {av:"G", cls:"gemini", agent:true}
 },
 projects: [
  {id:"ember", characterStyle:"mongle", short:"ember", name:"darkpyonix-ember", repo:"github.com/DarkPyonix/darkpyonix-ember", branch:"develop", computers:["mac-mini","ws-linux-01","macbook-pro"]},
  {id:"core", characterStyle:"mongle", short:"core", name:"darkpyonix-core", repo:"github.com/DarkPyonix/darkpyonix-core", branch:"develop", computers:["ws-linux-01"]},
  {id:"pyx", characterStyle:"mongle", short:"pyx", name:"pythonx-compose", repo:"github.com/thisisthepy/pythonx-compose", branch:"develop", computers:["win-desktop"]},
  {id:"site", characterStyle:"mongle", short:"site", name:"ember-site", repo:"github.com/DarkPyonix/ember-site", branch:"main", computers:["mac-mini"]},
  {id:"unsorted", characterStyle:"mongle", short:"기본", name:"기본 프로젝트", repo:"홈에서 시작한 대화", branch:"", computers:["mac-mini"]}
 ],
 computers: [
  {id:"mac-mini", os:"macOS 26", osk:"mac", kind:"mini", status:"online", agents:"Claude Code, Codex"},
  {id:"macbook-pro", os:"macOS 26", osk:"mac", kind:"laptop", status:"online", agents:"Claude Code"},
  {id:"ws-linux-01", os:"Ubuntu 24.04", osk:"linux", kind:"server", status:"online", agents:"Claude Code, Gemini CLI"},
  {id:"win-desktop", os:"Windows 11", osk:"win", kind:"monitor", status:"offline", agents:"Codex", seen:"2일 전"},
  {id:"gpu-box", os:"Linux (확인 중)", osk:"linux", kind:"server", status:"unknown", agents:"확인 중"}
 ],
 accounts: [
  {vendor:"claude", name:"Claude", tier:"Max", id:"ncmnd", left:72, five:59, reset:"3일 뒤"},
  {vendor:"claude", name:"Claude", tier:"Max", id:"work", left:38, five:82, reset:"5일 뒤"},
  {vendor:"codex", name:"Codex", tier:"Pro", id:"ncmnd", left:9, five:43, reset:"3일 뒤"},
  {vendor:"gemini", name:"Gemini", tier:"Pro", id:"ncmnd", left:null, five:null, reset:"공급자가 알려 주지 않음"}
 ],
 week: {labels:["일","월","화","수","목","금","토"], c:[2.1,2.8,1.6,3.3,2.6,2.4,2.5], x:[0.9,1.4,1.5,0.8,1.3,1.6,1.1], g:[0.3,0.4,0.3,0.4,0.5,0.4,0.3]},
 sessions: [
  {id:"s1", lead:true, role:"리더", title:"Wire shim into proxy", project:"ember", agent:"Claude Code", model:"claude-opus · High", computer:"mac-mini", account:"ncmnd · Max", status:"wait", t:9, pinned:true, msgs:[
   {k:"user", text:"Wire the shim into ember/proxy and make the integration test pass."},
   {k:"working", text:"1분 12초 동안 작업함"},
   {k:"agent", text:"프록시 진입점을 먼저 읽고, 기존 router 뒤에 shim 경로를 붙입니다."},
   {k:"cmds", items:[{cmd:"rg -n \"Router::new\" ember/proxy/src", out:"ember/proxy/src/router.rs:41:    let app = Router::new().merge(ide);"},{cmd:"sed -n 1,80p ember/proxy/src/router.rs", out:"(80 lines)"},{cmd:"ls ember/proxy/src/shim", out:"mod.rs  routes.rs  state.rs"},{cmd:"git status --short", out:" M ember/proxy/src/router.rs"}]},
   {k:"edit", file:"ember/proxy/src/router.rs", add:3, del:1, diff:["-    let app = Router::new().merge(ide);","+    let app = Router::new()","+        .merge(ide)","+        .nest(\"/shim\", shim::routes(state.clone()));"]},
   {k:"agent", text:"라우트를 붙였습니다. 이제 통합 테스트를 돌려 /shim/health 응답을 확인합니다."},
   {k:"approval", cmd:"cargo test -p ember-proxy --test shim", state:"pending"}
  ]},
  {id:"s2", role:"디자인", title:"Vendor workbench CSS", project:"ember", agent:"Codex", model:"gpt-5 · High", computer:"mac-mini", account:"ncmnd · Pro", status:"run", t:8, pinned:true, msgs:[
   {k:"user", text:"Code-OSS 워크벤치 CSS를 스크립트로 가져오고 :has()를 상태 클래스로 바꿔줘."},
   {k:"agent", text:"vendor.sh를 만들고 있습니다. :has() 사용처는 14곳입니다."},
   {k:"cmds", items:[{cmd:"rg -c \":has(\" vendor/", out:"14"}]},
   {k:"working", text:"작업 중"}
  ]},
  {id:"s3", lead:true, role:"리더", title:"Kernel manager hub API", project:"core", agent:"Claude Code", model:"claude-opus · High", computer:"ws-linux-01", account:"work · Max", status:"run", t:7, msgs:[
   {k:"user", text:"hub API 초안을 SPEC에 맞춰 정리해줘."}, {k:"working", text:"작업 중"}]},
  {id:"s4", role:"구현", title:"Fix session clone race", project:"ember", agent:"Claude Code", model:"claude-sonnet", computer:"ws-linux-01", account:"ncmnd · Max", status:"unread", t:6, msgs:[
   {k:"user", text:"IDE router가 세션을 가져가기 전에 clone하도록 고쳐줘 (#128)."},
   {k:"agent", text:"세션을 router에 넘기기 전에 clone하도록 바꿨습니다. 회귀 테스트를 하나 추가했고 통과합니다."},
   {k:"edit", file:"ember/server/src/ide.rs", add:6, del:2, diff:["-    router.take(session)","+    let s = session.clone();","+    router.take(s)"]}]},
  {id:"s5", role:"인프라", title:"Egress stop on exit", project:"ember", agent:"Claude Code", model:"claude-sonnet", computer:"mac-mini", account:"ncmnd · Max", status:"done", t:5, pinned:true, msgs:[
   {k:"user", text:"종료할 때 egress 프로세스도 멈추게 해줘."}, {k:"agent", text:"종료 경로에서 egress를 멈춥니다. 6개 파일을 바꿨고 PR #138로 병합됐습니다."}]},
  {id:"s6", role:"빌드", title:"OSE build on Windows", project:"ember", agent:"Codex", model:"gpt-5", computer:"win-desktop", account:"ncmnd · Pro", status:"fail", t:4, msgs:[
   {k:"user", text:"Windows에서 OSE 빌드 돌려줘."}, {k:"cmds", items:[{cmd:"ember/ose/build.ps1", out:"LINK : fatal error LNK1104: cannot open file 'vscode.lib'"}]}, {k:"agent", text:"링크 단계에서 실패했습니다. win-desktop이 오프라인이 되어 재시도하지 못했습니다."}]},
  {id:"s7", role:"보안", title:"Hub auth tokens", project:"core", agent:"Claude Code", model:"claude-opus", computer:"ws-linux-01", account:"work · Max", status:"done", t:3, msgs:[{k:"user", text:"hub 인증 토큰 만료 처리."}, {k:"agent", text:"만료 30초 전에 갱신합니다. 테스트 4개 추가."}]},
  {id:"s8", lead:true, role:"벤치", title:"Compose text layout bench", project:"pyx", agent:"Gemini CLI", model:"gemini-pro", computer:"win-desktop", account:"ncmnd · Pro", status:"done", t:2, msgs:[{k:"user", text:"텍스트 레이아웃 벤치마크 정리."}, {k:"agent", text:"결과 표를 docs/bench.md에 정리했습니다."}]},
  {id:"s9", role:"비서", title:"릴리스 전 실패 체크 요약", project:"unsorted", agent:"Claude Code", model:"claude-opus", computer:"mac-mini", account:"ncmnd · Max", status:"done", t:1, msgs:[{k:"user", text:"릴리스 전에 develop에서 실패한 체크 요약해줘"}, {k:"agent", text:"실패한 체크는 test-ose.yml 하나입니다. Windows 링크 단계에서 멈춥니다."}]}
,
  {id:"s10", role:"구현2", title:"Channel unread push", project:"ember", agent:"Codex", model:"gpt-5", computer:"ws-linux-01", account:"ncmnd · Pro", status:"run", t:0.9, msgs:[{k:"user", text:"채널 안 읽은 수를 PR-1 푸시로 바꿔줘."},{k:"agent", text:"서버 이벤트 스트림에 unread 갱신을 추가하는 중입니다."}]},
  {id:"s11", role:"스케줄러", title:"Routine scheduler", project:"ember", agent:"Claude Code", model:"claude-opus", computer:"mac-mini", account:"ncmnd · Max", status:"wait", t:0.8, msgs:[{k:"user", text:"루틴 스케줄러 초안 만들어줘."},{k:"agent", text:"cron 파서를 추가하려 합니다."},{k:"approval", cmd:"cargo add cron@0.15", state:"pending"}]},
  {id:"s12", role:"조사", title:"Squircle shape modifier", project:"ember", agent:"Gemini CLI", model:"gemini-pro", computer:"ws-linux-01", account:"ncmnd · Pro", status:"done", t:0.7, msgs:[{k:"user", text:"스퀘어클 모서리 지원 범위 조사."},{k:"agent", text:"ContinuousCornerShape가 이미 있습니다. 스키마 modifier만 추가하면 됩니다."}]},
  {id:"s13", role:"검색", title:"Search index rebuild", project:"ember", agent:"Claude Code", model:"claude-sonnet", computer:"mac-mini", account:"work · Max", status:"done", t:0.6, msgs:[{k:"user", text:"세션 검색 인덱스 재구성."},{k:"agent", text:"인덱스를 다시 만들었습니다. 검색 지연이 40% 줄었습니다."}]},
  {id:"s14", role:"문서", title:"Export transcript", project:"ember", agent:"Codex", model:"gpt-5", computer:"mac-mini", account:"ncmnd · Pro", status:"unread", t:0.5, msgs:[{k:"user", text:"세션 내보내기 형식 정리."},{k:"agent", text:"정규화된 JSONL 하나로 내보냅니다. 예제 파일을 추가했습니다."}]},
  {id:"s15", role:"구현", title:"Kernel restart policy", project:"core", agent:"Codex", model:"gpt-5", computer:"ws-linux-01", account:"ncmnd · Pro", status:"wait", t:0.4, msgs:[{k:"user", text:"커널 재시작 정책 구현."},{k:"approval", cmd:"cargo test -p manager restart", state:"pending"}]},
  {id:"s16", lead:true, role:"카피", title:"Landing page copy", project:"site", agent:"Claude Code", model:"claude-sonnet", computer:"mac-mini", account:"ncmnd · Max", status:"done", t:0.3, msgs:[{k:"user", text:"랜딩 페이지 문구 다듬어줘."},{k:"agent", text:"첫 화면 문구를 세 문장으로 줄였습니다."}]},
  {id:"s17", role:"배포", title:"Docs deploy", project:"site", agent:"Gemini CLI", model:"gemini-pro", computer:"mac-mini", account:"ncmnd · Pro", status:"run", t:0.2, msgs:[{k:"user", text:"문서 사이트 배포."},{k:"agent", text:"빌드를 올리는 중입니다."}]},
  {id:"s18", role:"테스터", title:"Compose snapshot tests", project:"pyx", agent:"Claude Code", model:"claude-opus", computer:"win-desktop", account:"work · Max", status:"fail", t:0.1, msgs:[{k:"user", text:"스냅샷 테스트 갱신."},{k:"agent", text:"win-desktop이 오프라인이라 실패했습니다."}]}
 ],
 runsWeek: {labels:["일","월","화","수","목","금","토"], ok:[5,7,6,8,7,6,4], fail:[1,0,2,1,0,1,1]},
 routines: [
  {id:"rt1", name:"develop CI 실패 요약", project:"ember", trigger:"time", schedule:"매일 오전 9시", cron:"0 9 * * *", agent:"Claude Code", model:"claude-sonnet", computer:"mac-mini", account:"ncmnd · Max", enabled:true, next:"내일 오전 9:00",
   prompt:"develop 브랜치에서 지난 24시간 동안 실패한 CI 체크를 모아 원인별로 요약하고, ember 개발방 채널에 올립니다.",
   runs:[{status:"done",time:"오늘 오전 9:00",session:"s9"},{status:"done",time:"어제 오전 9:00",session:"s9"},{status:"fail",time:"10월 23일 오전 9:00",session:"s6"}]},
  {id:"rt2", name:"PR 열리면 리뷰", project:"ember", trigger:"event", schedule:"develop 대상 PR이 열리거나 갱신될 때", cron:"event: pull_request.opened, synchronize", agent:"Codex", model:"gpt-5 · High", computer:"mac-mini", account:"ncmnd · Pro", enabled:true, next:"다음 PR 이벤트",
   prompt:"새 PR의 diff를 읽고 SPEC ID와 테스트가 있는지 확인한 뒤, 문제점을 PR 코멘트로 남깁니다.",
   runs:[{status:"done",time:"오늘 오전 8:50",session:"s4"},{status:"done",time:"어제 오후 6:40",session:"s5"}]},
  {id:"rt3", name:"주간 의존성 점검", project:"ember", trigger:"time", schedule:"매주 월요일 오전 10시", cron:"0 10 * * 1", agent:"Claude Code", model:"claude-opus", computer:"ws-linux-01", account:"work · Max", enabled:false, next:"꺼짐",
   prompt:"각 크레이트의 Cargo.lock에서 보안 권고와 오래된 의존성을 찾아 표로 정리하고, 갱신 PR 초안을 만듭니다.",
   runs:[{status:"done",time:"10월 20일 오전 10:00",session:"s5"}]},
  {id:"rt4", name:"OSE 야간 빌드", project:"ember", trigger:"time", schedule:"매일 새벽 2시", cron:"0 2 * * *", agent:"Codex", model:"gpt-5", computer:"win-desktop", account:"ncmnd · Pro", enabled:true, next:"내일 오전 2:00",
   prompt:"Windows에서 OSE 빌드를 돌리고 실패하면 로그 마지막 200줄과 함께 CI 알림 채널에 올립니다.",
   runs:[{status:"fail",time:"오늘 오전 2:00",session:"s6"},{status:"fail",time:"어제 오전 2:00",session:"s6"}]},
  {id:"rt5", name:"hub API 회귀 테스트", project:"core", trigger:"event", schedule:"develop에 push될 때", cron:"event: push(develop)", agent:"Claude Code", model:"claude-sonnet", computer:"ws-linux-01", account:"work · Max", enabled:true, next:"다음 push",
   prompt:"kernel, manager, hub API 통합 테스트를 돌리고 실패하면 원인을 찾아 수정 PR을 엽니다.",
   runs:[{status:"run",time:"방금",session:"s3"},{status:"done",time:"어제 오후 3:14",session:"s7"}]},
  {id:"rt6", name:"벤치마크 리포트", project:"pyx", trigger:"time", schedule:"매주 금요일 오후 6시", cron:"0 18 * * 5", agent:"Gemini CLI", model:"gemini-pro", computer:"win-desktop", account:"ncmnd · Pro", enabled:true, next:"10월 31일 오후 6:00",
   prompt:"텍스트 레이아웃 벤치마크를 돌려 지난주와 비교한 표를 docs/bench.md에 갱신합니다.",
   runs:[{status:"done",time:"10월 24일 오후 6:00",session:"s8"}]},
  {id:"rt7", name:"아침 브리핑", project:"unsorted", trigger:"time", schedule:"평일 오전 8시 30분", cron:"30 8 * * 1-5", agent:"Claude Code", model:"claude-opus", computer:"mac-mini", account:"ncmnd · Max", enabled:true, next:"월요일 오전 8:30",
   prompt:"모든 프로젝트의 밤사이 세션 결과, 승인 대기, 사용량을 한 화면으로 요약합니다.",
   runs:[{status:"done",time:"10월 24일 오전 8:30",session:"s9"}]}
 ],
 prs: [
  {project:"ember", num:151, title:"Design mockup v3", branch:"feat/design-mockup", sid:"s12", ci:"run", state:"open", review:"리뷰 대기"},
  {project:"ember", num:139, title:"Wire shim into proxy", branch:"feat/shim-wiring", sid:"s1", ci:"pass", state:"open", review:"코멘트 2"},
  {project:"ember", num:145, title:"OSE build on Windows", branch:"feat/ose-win", sid:"s6", ci:"fail", state:"open", review:"변경 요청"},
  {project:"ember", num:144, title:"Vendor workbench CSS", branch:"feat/vendor-workbench-css", sid:"s2", ci:"pass", state:"merged", review:""},
  {project:"core", num:55, title:"Kernel restart policy", branch:"feat/restart-policy", sid:"s15", ci:"run", state:"open", review:"리뷰 대기"},
  {project:"core", num:52, title:"Hub auth tokens", branch:"feat/hub-auth", sid:"s7", ci:"pass", state:"merged", review:""},
  {project:"pyx", num:210, title:"Compose snapshot tests", branch:"feat/snapshots", sid:"s18", ci:"fail", state:"open", review:"리뷰 대기"},
  {project:"site", num:12, title:"Landing page copy", branch:"copy/landing", sid:"s16", ci:"pass", state:"open", review:"승인됨"}
 ],
 subagents: {
  s1: [{name:"Explore: proxy router", status:"done", task:"router.rs 구조 파악"},{name:"Test runner", status:"wait", task:"cargo test 승인 대기"}],
  s2: [{name:"CSS scanner", status:"run", task:":has() 사용처 14곳", children:[{name:":has() rewrite", status:"run", task:"상태 클래스로 바꾸는 중"},{name:"Snapshot diff", status:"done", task:"차이 없음"}]}],
  s11: [{name:"cron 파서 조사", status:"done", task:"cron 0.15 선택"}],
  s15: [{name:"Restart tests", status:"wait", task:"승인 대기"}]
 },
 rooms: [
  {id:"r1", project:"ember", name:"ember 개발방", members:["BREW","Jun","Claude Code","Codex","Gemini CLI"], unread:3, t:9, msgs:[
   {date:"2026년 10월 24일 토요일"},
   {from:"Jun", text:"내일 develop 정리하고 릴리스 싱크 돌릴게요", time:"오후 6:02", unread:0},
   {from:"Codex", text:"PR #144 병합 완료. CI 전부 통과.", time:"오후 6:40", unread:0, card:{title:"PR #144 merged", body:"Vendor Code-OSS workbench CSS"}},
   {date:"2026년 10월 25일 일요일"},
   {sys:"Gemini CLI님이 들어왔습니다."},
   {from:"BREW", text:"#151 디자인 시안 먼저 올리고 승인 받자", time:"오전 9:12", unread:0},
   {from:"Claude Code", text:"디자인 에이전트 시작했습니다. 시안은 .scratch/design-mockup/에 올립니다.", time:"오전 9:13", unread:1},
   {from:"Gemini CLI", text:"Windows OSE 빌드가 아직 링크 단계에서 실패합니다.", time:"오전 10:02", unread:2},
   {from:"Gemini CLI", text:"로그는 세션 OSE build on Windows에 있습니다.", time:"오전 10:02", unread:2}
  ]},
  {id:"r2", project:"ember", name:"리뷰", members:["BREW","Codex","Claude Code"], unread:2, t:8, msgs:[
   {date:"2026년 10월 25일 일요일"},
   {from:"Codex", text:"#139 shim wiring 리뷰 부탁", time:"오전 8:50", unread:1},
   {from:"Claude Code", text:"코멘트 2개 남겼습니다. 나머지는 괜찮습니다.", time:"오전 9:05", unread:1}]},
  {id:"r3", project:"ember", name:"CI 알림", members:["BREW","Codex"], unread:0, t:6, msgs:[{date:"2026년 10월 24일 토요일"},{from:"Codex", text:"test-ose 실패 (Windows)", time:"오후 11:20", unread:0}]},
  {id:"r4", project:"core", name:"core 일반", members:["BREW","Jun","Claude Code"], unread:0, t:5, msgs:[{date:"2026년 10월 23일 금요일"},{from:"Jun", text:"hub 인증 토큰 들어갔어요", time:"오후 3:14", unread:0}]},
  {id:"r5", project:"core", name:"kernel", members:["BREW","Claude Code","Gemini CLI"], unread:0, t:4, msgs:[{date:"2026년 10월 22일 목요일"},{from:"Claude Code", text:"manager API 초안 올렸습니다.", time:"오후 2:01", unread:0}]},
  {id:"r6", project:"unsorted", name:"메모", members:["BREW","Claude Code"], unread:0, t:1, msgs:[{date:"2026년 10월 20일 월요일"},{from:"BREW", text:"아이디어: 채널에서 세션 바로 열기", time:"오후 9:30", unread:0}]}
 ]
};

/* ---- appended data blocks: one block per owner, never edit another block ----
   window.EMBER_DEMO.vcs = {...};          // version-control agent
   window.EMBER_DEMO.integrations = {...}; // home+integrations agent
   Screens read them as Ember.data.vcs / Ember.data.integrations (Ember.data is the live state). */

/* ===== home+integrations agent block (home.js). Do not edit from other screens. ===== */
window.EMBER_DEMO.integrations = {
 list: [
  {id:"github", name:"GitHub", glyph:"git", state:"on", account:"DarkPyonix", sync:"1분 전", scope:"저장소 4개 · PR, Actions, 이슈",
   stats:[["열린 PR","6"],["리뷰 대기","3"],["CI 통과율","88%"]], ci:[1,1,1,0,1,1,1,0,1,1,1,1,0,1],
   feeds:["성공률","작업 상태"], rules:[
    {on:true, when:"develop CI 실패", then:"루틴 'CI 고치기'가 core-구현에게 맡김", n:"이번 주 3회, 2회 고침"},
    {on:true, when:"PR 리뷰 요청", then:"ember-리더가 요약을 채널에 올림", n:"이번 주 7회"},
    {on:false, when:"이슈에 ember 라벨", then:"기본 프로젝트에 새 세션 열기", n:"꺼짐"}]},
  {id:"calendar", name:"Google 캘린더", glyph:"cal", state:"on", account:"ncmnd13", sync:"5분 전", scope:"기본 캘린더 읽기",
   stats:[["오늘 회의","3"],["집중 시간","4시간"]], day:[["10:00","11:00","m","주간 회의"],["11:00","13:00","f","집중"],["14:00","15:00","m","리뷰"],["15:00","17:00","f","집중"],["17:30","18:00","m","1:1"]],
   feeds:["승인 대기"], rules:[
    {on:true, when:"회의 중", then:"승인 알림을 묶어서 회의 뒤에 보냄", n:"오늘 2번 묶음"},
    {on:true, when:"집중 시간", then:"긴 작업(빌드, 리팩터)을 이 시간에 몰아 실행", n:"오늘 3건 예약"}]},
  {id:"slack", name:"Slack", glyph:"hash", state:"on", account:"darkpyonix", sync:"방금", scope:"#ember-dev, #alerts",
   stats:[["Slack에서 승인","12"],["보낸 알림","41"]], feeds:["승인 대기"], rules:[
    {on:true, when:"승인 대기 5분 넘음", then:"#ember-dev에 승인 버튼과 함께 알림", n:"오늘 4회"},
    {on:true, when:"루틴 실패", then:"#alerts에 요약", n:"이번 주 2회"}]},
  {id:"discord", name:"Discord", glyph:"chats", state:"off", account:"", sync:"", scope:"서버 하나를 고르면 채널을 연결합니다",
   stats:[], feeds:["채널"], rules:[{on:false, when:"Discord 채널 메시지", then:"Ember 채널로 가져와 팀원이 답함", n:"연결 전"}]},
  {id:"mail", name:"Gmail", glyph:"mail", state:"warn", account:"me@example.com", sync:"2일 전", scope:"토큰 만료, 다시 로그인 필요",
   stats:[["주간 요약","월 09:00"]], feeds:["비용"], rules:[
    {on:true, when:"매주 월요일", then:"사용량과 비용 요약을 메일로 보냄", n:"지난주 보냄"},
    {on:false, when:"청구서 메일", then:"공급자 비용에 반영", n:"재인증 필요"}]},
  {id:"linear", name:"Linear", glyph:"line", state:"off", account:"", sync:"", scope:"이슈를 팀원의 작업 큐로 가져옵니다",
   stats:[], feeds:["작업 상태"], rules:[{on:false, when:"이슈가 In Progress", then:"담당 팀원 세션을 엽니다", n:"연결 전"}]}
 ],
 flow: [["CI 실패",5,"github"],["루틴이 받음",4,"github"],["팀원이 고침",3,""],["PR 머지",2,"github"]]
};
window.EMBER_DEMO.homeStats = {
 days:["10/12","10/13","10/14","10/15","10/16","10/17","10/18","10/19","10/20","10/21","10/22","10/23","10/24","10/25"],
 cost:[8.2,11.4,9.1,6.0,7.3,12.9,10.4,9.8,13.6,11.2,14.8,12.1,10.9,12.4],
 tok:[2.6,3.4,2.9,2.0,2.4,4.1,3.3,3.3,4.6,3.4,4.5,4.2,4.0,3.9],
 ok:[91,88,93,90,86,94,92,89,95,90,93,91,94,92],
 status:{ok:47, retry:3, fail:4},
 wait:{median:4.2, prev:5.6, buckets:[["1분 미만",14],["1-3분",11],["3-10분",9],["10-30분",5],["30분 이상",2]],
  hours:[0,0,0,0,0,0,0,0,1,3,6,8,4,3,7,9,5,4,2,1,1,0,0,0], meet:2.3},
 eff:[{id:"s1",tasks:14,ok:93,cost:0.62},{id:"s4",tasks:11,ok:91,cost:0.48},{id:"s3",tasks:9,ok:100,cost:0.71},{id:"s2",tasks:8,ok:88,cost:0.55},{id:"s15",tasks:7,ok:86,cost:0.31},{id:"s16",tasks:5,ok:80,cost:0.22}],
 load:[{id:"mac-mini",cpu:72,mem:64,run:3},{id:"ws-linux-01",cpu:91,mem:78,run:4},{id:"macbook-pro",cpu:23,mem:41,run:1}]
};

/* ==== vcs block (version-control agent). Per project: repo state, branches, changes, history, PRs, stashes, conflicts, worktrees. ==== */
window.EMBER_DEMO.vcs = {
 ember: {
  branch:"feat/session-shim", upstream:"origin/feat/session-shim", ahead:2, behind:1, fetched:"3분 전",
  branches:[
   {name:"feat/session-shim", base:"develop", ahead:2, behind:1, by:"s1", when:"방금"},
   {name:"develop", base:"", ahead:0, behind:0, when:"12분 전", protected:true},
   {name:"main", base:"", ahead:0, behind:0, when:"2일 전", protected:true},
   {name:"feat/vendor-workbench-css", base:"develop", ahead:5, behind:0, by:"s2", when:"8분 전"},
   {name:"fix/session-clone-race", base:"develop", ahead:3, behind:2, by:"s4", when:"25분 전"},
   {name:"feat/egress-stop", base:"develop", ahead:0, behind:4, by:"s5", when:"1시간 전", merged:true},
   {name:"feat/ose-windows", base:"develop", ahead:7, behind:6, by:"s6", when:"2시간 전"},
   {name:"feat/channel-unread-push", base:"develop", ahead:1, behind:0, by:"s10", when:"4분 전"},
   {name:"feat/routine-scheduler", base:"develop", ahead:4, behind:1, by:"s11", when:"16분 전"}
  ],
  remotes:["origin/main","origin/develop","origin/release","origin/feat/session-shim","origin/feat/vendor-workbench-css","origin/fix/session-clone-race"],
  changes:[
   {path:"ember/proxy/src/shim.rs", st:"M", add:14, del:3, hunks:[
    {h:"@@ -41,9 +41,20 @@ impl SessionShim {", lines:[
     [" ","    pub fn attach(&mut self, session: &Session) -> Result<()> {",41,41],
     [" ","        let id = session.id().clone();",42,42],
     ["-","        self.router.insert(id, session.handle());",43,0],
     ["+","        // Clone before the IDE router takes ownership (#128).",0,43],
     ["+","        let handle = session.handle().clone();",0,44],
     ["+","        self.router.insert(id.clone(), handle);",0,45],
     ["+","        self.egress.watch(&id)?;",0,46],
     [" ","        Ok(())",44,47],
     [" ","    }",45,48]]},
    {h:"@@ -88,6 +99,13 @@ impl SessionShim {", lines:[
     [" ","    pub fn detach(&mut self, id: &SessionId) {",88,99],
     ["-","        self.router.remove(id);",89,0],
     ["-","        // TODO: stop egress",90,0],
     ["+","        if let Some(h) = self.router.remove(id) {",0,100],
     ["+","            h.close();",0,101],
     ["+","        }",0,102],
     ["+","        self.egress.stop(id);",0,103],
     ["+","        tracing::debug!(%id, \"shim detached\");",0,104],
     [" ","    }",91,105]]}]},
   {path:"ember/proxy/src/router.rs", st:"M", add:6, del:2, hunks:[
    {h:"@@ -12,7 +12,11 @@ use crate::shim::SessionShim;", lines:[
     [" ","pub struct IdeRouter {",12,12],
     ["-","    routes: HashMap<SessionId, Handle>,",13,0],
     ["+","    routes: HashMap<SessionId, Handle>,",0,13],
     ["+","    /// Sessions the shim has attached; checked before routing.",0,14],
     ["+","    attached: HashSet<SessionId>,",0,15],
     [" ","}",14,16]]}]},
   {path:"ember/proxy/tests/shim_attach.rs", st:"A", add:22, del:0, hunks:[
    {h:"@@ -0,0 +1,9 @@", lines:[
     ["+","use ember_proxy::shim::SessionShim;",0,1],
     ["+","",0,2],
     ["+","#[tokio::test]",0,3],
     ["+","async fn attach_clones_handle_before_router() {",0,4],
     ["+","    let mut shim = SessionShim::for_test();",0,5],
     ["+","    let s = shim.spawn_session().await;",0,6],
     ["+","    shim.attach(&s).unwrap();",0,7],
     ["+","    assert!(s.handle().is_alive());",0,8],
     ["+","}",0,9]]}]},
   {path:"docs/SPEC.md", st:"M", add:3, del:1, hunks:[
    {h:"@@ -204,4 +204,6 @@ ## Proxy", lines:[
     [" ","- PX-12 The proxy serves the VS Code Web workbench.",204,204],
     ["-","- PX-13 Sessions attach through the shim.",205,0],
     ["+","- PX-13 Sessions attach through the shim, which clones the handle",0,205],
     ["+","  before the IDE router takes it. [provisional]",0,206],
     ["+","- PX-14 Detach stops egress for that session. [provisional]",0,207]]}]},
   {path:"ember/proxy/src/legacy_bridge.rs", st:"D", add:0, del:31, hunks:[
    {h:"@@ -1,4 +0,0 @@", lines:[
     ["-","//! Bridge kept for the pre-shim proxy. Removed with PX-13.",1,0],
     ["-","use crate::router::IdeRouter;",2,0],
     ["-","",3,0],
     ["-","pub fn bridge(r: &mut IdeRouter) { /* ... */ }",4,0]]}]}
  ],
  commits:[
   {sha:"a91c2f0", lane:0, parents:["7d3e118"], msg:"Feat: Clone the handle in SessionShim::attach", body:"PX-13. The IDE router took the handle and the shim lost it.", who:"s1", time:"방금", refs:["HEAD","feat/session-shim"], local:true},
   {sha:"7d3e118", lane:0, parents:["c40b9aa"], msg:"Test: Shim attach keeps the session alive", who:"s1", time:"9분 전", local:true},
   {sha:"5be20d4", lane:1, parents:["645d0f4"], msg:"Feat: Vendor Code-OSS workbench CSS by script", who:"s2", time:"8분 전", refs:["feat/vendor-workbench-css"]},
   {sha:"c40b9aa", lane:0, parents:["645d0f4"], msg:"Refactor: Split shim from proxy router", who:"s1", time:"34분 전", refs:["origin/feat/session-shim"]},
   {sha:"645d0f4", lane:2, parents:["3380069","3203e4a"], msg:"Merge pull request #144 from feat/vendor-workbench-css", who:"BREW", time:"1시간 전", refs:["develop","origin/develop"]},
   {sha:"3203e4a", lane:3, parents:["91e3cc8"], msg:"Feat: Rewrite :has() to state classes", who:"s2", time:"1시간 전"},
   {sha:"3380069", lane:2, parents:["2f7aabf","b81f0e2"], msg:"Merge pull request #138 from feat/egress-stop", who:"BREW", time:"2시간 전"},
   {sha:"b81f0e2", lane:4, parents:["91e3cc8"], msg:"Feat: Stop egress when the session exits", who:"s5", time:"3시간 전"},
   {sha:"2f7aabf", lane:2, parents:["91e3cc8"], msg:"Merge pull request #139 from feat/shim-wiring", who:"BREW", time:"3시간 전"},
   {sha:"91e3cc8", lane:2, parents:["e02a7c1"], msg:"Fix: Clone sessions before the IDE router takes it (#128)", who:"s4", time:"5시간 전"},
   {sha:"e02a7c1", lane:2, parents:["0f6b9d3"], msg:"Docs: Record the root layout decision", who:"BREW", time:"어제"},
   {sha:"0f6b9d3", lane:2, parents:[], msg:"Chore: Release sync develop to main", who:"BREW", time:"2일 전", refs:["main","origin/main"]}
  ],
  prs:[
   {n:146, title:"Feat: Wire the session shim into the proxy", branch:"feat/session-shim", base:"develop", by:"s1", state:"open", draft:false, ci:"run", review:"pending", checks:[["Test (ubuntu, stable)","ok"],["Test (macos, stable)","run"],["Test (windows, stable)","wait"],["No em-dash","ok"],["No webview","ok"]], reviewers:[["s4","pending"],["BREW","pending"]], add:45, del:37, files:5, comments:[{who:"s4", file:"ember/proxy/src/shim.rs", line:44, text:"handle을 clone하면 Drop이 두 번 불리지 않는지 테스트로 확인해 주면 좋겠습니다."}]},
   {n:145, title:"Feat: Channel unread count over PR-1 push", branch:"feat/channel-unread-push", base:"develop", by:"s10", state:"open", draft:true, ci:"fail", review:"none", checks:[["Test (ubuntu, stable)","fail"],["Test (macos, stable)","ok"],["No em-dash","ok"]], reviewers:[], add:120, del:8, files:7, comments:[]},
   {n:143, title:"Fix: Session clone race on reconnect", branch:"fix/session-clone-race", base:"develop", by:"s4", state:"open", draft:false, ci:"ok", review:"changes", checks:[["Test (ubuntu, stable)","ok"],["Test (macos, stable)","ok"],["Test (windows, stable)","ok"]], reviewers:[["s1","changes"]], add:31, del:12, files:3, comments:[{who:"s1", file:"ember/server/src/clone.rs", line:77, text:"재연결 중에 clone이 두 번 들어오는 경로가 아직 남아 있습니다."}]},
   {n:141, title:"Feat: OSE build on Windows runners", branch:"feat/ose-windows", base:"develop", by:"s6", state:"open", draft:false, ci:"fail", review:"pending", checks:[["OSE build (windows-latest)","fail"],["OSE build (ubuntu-latest)","ok"]], reviewers:[["s1","pending"]], add:210, del:44, files:9, comments:[]},
   {n:144, title:"Feat: Vendor Code-OSS workbench CSS by script", branch:"feat/vendor-workbench-css", base:"develop", by:"s2", state:"merged", ci:"ok", review:"approved", checks:[["Test (ubuntu, stable)","ok"]], reviewers:[["s1","approved"]], add:380, del:12, files:14, comments:[]},
   {n:138, title:"Feat: Stop egress on exit", branch:"feat/egress-stop", base:"develop", by:"s5", state:"merged", ci:"ok", review:"approved", checks:[["Test (ubuntu, stable)","ok"]], reviewers:[["s1","approved"]], add:64, del:9, files:4, comments:[]}
  ],
  stashes:[
   {id:"stash@{0}", msg:"WIP on feat/session-shim: router logging", branch:"feat/session-shim", files:2, when:"40분 전"},
   {id:"stash@{1}", msg:"WIP on develop: try corner-shape fallback", branch:"develop", files:3, when:"어제"}
  ],
  conflict:{from:"develop", into:"fix/session-clone-race", files:[
   {path:"ember/server/src/clone.rs", blocks:[
    {ctx:["pub fn clone_session(src: &Session) -> Session {"], ours:["    let mut s = src.shallow();","    s.reset_cursor();"], theirs:["    let mut s = src.deep_clone();","    s.reset_cursor();","    s.mark_cloned_at(Instant::now());"], pick:null},
    {ctx:["    s","}"], ours:[], theirs:[], pick:"none"}]},
   {path:"ember/server/Cargo.lock", blocks:[
    {ctx:["[[package]]","name = \"ember-node\""], ours:["version = \"0.4.1\""], theirs:["version = \"0.4.2\""], pick:null}]}
  ]},
  worktrees:[
   {who:"s1", branch:"feat/session-shim", path:".claude/worktrees/session-shim", computer:"mac-mini", dirty:5, ahead:2, behind:1, last:"Feat: Clone the handle in SessionShim::attach"},
   {who:"s2", branch:"feat/vendor-workbench-css", path:".claude/worktrees/workbench-css", computer:"mac-mini", dirty:0, ahead:5, behind:0, last:"Feat: Vendor Code-OSS workbench CSS by script"},
   {who:"s4", branch:"fix/session-clone-race", path:".claude/worktrees/clone-race", computer:"ws-linux-01", dirty:2, ahead:3, behind:2, last:"Fix: Retry clone after reconnect", conflict:true},
   {who:"s6", branch:"feat/ose-windows", path:"C:/ember/.claude/worktrees/ose-win", computer:"win-desktop", dirty:1, ahead:7, behind:6, last:"Chore: Pin MSVC toolset"},
   {who:"s10", branch:"feat/channel-unread-push", path:".claude/worktrees/unread-push", computer:"ws-linux-01", dirty:3, ahead:1, behind:0, last:"Feat: Count unread per room"},
   {who:"s11", branch:"feat/routine-scheduler", path:".claude/worktrees/routine-sched", computer:"mac-mini", dirty:0, ahead:4, behind:1, last:"Feat: Cron parser for routines"}
  ]
 },
 core: {
  branch:"feat/hub-api", upstream:"origin/feat/hub-api", ahead:0, behind:3, fetched:"10분 전",
  branches:[
   {name:"feat/hub-api", base:"develop", ahead:0, behind:3, by:"s3", when:"10분 전"},
   {name:"develop", base:"", ahead:0, behind:0, when:"20분 전", protected:true},
   {name:"main", base:"", ahead:0, behind:0, when:"3일 전", protected:true},
   {name:"feat/kernel-restart", base:"develop", ahead:2, behind:0, by:"s15", when:"30분 전"},
   {name:"feat/hub-auth-tokens", base:"develop", ahead:0, behind:0, by:"s7", when:"1시간 전", merged:true}
  ],
  remotes:["origin/main","origin/develop","origin/feat/hub-api"],
  changes:[
   {path:"crates/hub/src/api.rs", st:"M", add:4, del:1, hunks:[
    {h:"@@ -20,5 +20,8 @@ pub fn routes() -> Router {", lines:[
     [" ","    Router::new()",20,20],
     ["-","        .route(\"/kernels\", get(list))",21,0],
     ["+","        .route(\"/kernels\", get(list).post(start))",0,21],
     ["+","        .route(\"/kernels/:id\", delete(stop))",0,22],
     ["+","        .route(\"/kernels/:id/restart\", post(restart))",0,23],
     ["+","        .layer(auth::layer())",0,24],
     [" ","}",22,25]]}]}
  ],
  commits:[
   {sha:"d1f0a33", lane:1, parents:["8a2c771"], msg:"Feat: Restart policy with backoff", who:"s15", time:"30분 전", refs:["feat/kernel-restart"]},
   {sha:"9e4b120", lane:0, parents:["8a2c771"], msg:"Feat: Kernel list endpoint", who:"s3", time:"1시간 전", refs:["HEAD","feat/hub-api"]},
   {sha:"8a2c771", lane:2, parents:["44ce0d1","2b7e9f5"], msg:"Merge pull request #52 from feat/hub-auth-tokens", who:"BREW", time:"1시간 전", refs:["develop","origin/develop"]},
   {sha:"2b7e9f5", lane:3, parents:["44ce0d1"], msg:"Feat: Expire hub tokens after 24h", who:"s7", time:"2시간 전"},
   {sha:"44ce0d1", lane:2, parents:[], msg:"Chore: Release sync develop to main", who:"BREW", time:"3일 전", refs:["main"]}
  ],
  prs:[
   {n:53, title:"Feat: Kernel restart policy", branch:"feat/kernel-restart", base:"develop", by:"s15", state:"open", ci:"ok", review:"pending", checks:[["Test (ubuntu, stable)","ok"]], reviewers:[["s3","pending"]], add:88, del:10, files:4, comments:[]},
   {n:52, title:"Feat: Hub auth tokens", branch:"feat/hub-auth-tokens", base:"develop", by:"s7", state:"merged", ci:"ok", review:"approved", checks:[["Test (ubuntu, stable)","ok"]], reviewers:[["s3","approved"]], add:140, del:22, files:6, comments:[]}
  ],
  stashes:[], conflict:null,
  worktrees:[
   {who:"s3", branch:"feat/hub-api", path:".claude/worktrees/hub-api", computer:"ws-linux-01", dirty:1, ahead:0, behind:3, last:"Feat: Kernel list endpoint"},
   {who:"s15", branch:"feat/kernel-restart", path:".claude/worktrees/kernel-restart", computer:"ws-linux-01", dirty:0, ahead:2, behind:0, last:"Feat: Restart policy with backoff"}
  ]
 },
 pyx: {
  branch:"develop", upstream:"origin/develop", ahead:0, behind:0, fetched:"1시간 전",
  branches:[
   {name:"develop", base:"", ahead:0, behind:0, when:"1시간 전", protected:true},
   {name:"main", base:"", ahead:0, behind:0, when:"1주 전", protected:true},
   {name:"bench/text-layout", base:"develop", ahead:3, behind:0, by:"s8", when:"2시간 전"},
   {name:"test/compose-snapshots", base:"develop", ahead:2, behind:1, by:"s18", when:"3시간 전"}
  ],
  remotes:["origin/main","origin/develop"],
  changes:[],
  commits:[
   {sha:"71aa0de", lane:1, parents:["5c0e8f2"], msg:"Test: Snapshot for LazyColumn", who:"s18", time:"3시간 전", refs:["test/compose-snapshots"]},
   {sha:"b3d9e41", lane:2, parents:["5c0e8f2"], msg:"Bench: Text layout with 10k glyphs", who:"s8", time:"2시간 전", refs:["bench/text-layout"]},
   {sha:"5c0e8f2", lane:0, parents:[], msg:"Docs: Writing style guide", who:"BREW", time:"1시간 전", refs:["HEAD","develop","origin/develop"]}
  ],
  prs:[{n:212, title:"Bench: Text layout", branch:"bench/text-layout", base:"develop", by:"s8", state:"open", ci:"ok", review:"approved", checks:[["Test (ubuntu, 3.13)","ok"]], reviewers:[["BREW","approved"]], add:150, del:0, files:3, comments:[]}],
  stashes:[], conflict:null,
  worktrees:[
   {who:"s8", branch:"bench/text-layout", path:".claude/worktrees/text-bench", computer:"win-desktop", dirty:0, ahead:3, behind:0, last:"Bench: Text layout with 10k glyphs"},
   {who:"s18", branch:"test/compose-snapshots", path:".claude/worktrees/snapshots", computer:"win-desktop", dirty:4, ahead:2, behind:1, last:"Test: Snapshot for LazyColumn"}
  ]
 },
 site: {
  branch:"main", upstream:"origin/main", ahead:1, behind:0, fetched:"5분 전",
  branches:[
   {name:"main", base:"", ahead:1, behind:0, when:"5분 전", protected:true},
   {name:"copy/landing", base:"main", ahead:2, behind:0, by:"s16", when:"20분 전"},
   {name:"deploy/docs", base:"main", ahead:1, behind:0, by:"s17", when:"3분 전"}
  ],
  remotes:["origin/main"],
  changes:[],
  commits:[
   {sha:"c2f81aa", lane:1, parents:["4e0d6b7"], msg:"Docs: Deploy preview", who:"s17", time:"3분 전", refs:["deploy/docs"]},
   {sha:"9a03bd2", lane:0, parents:["4e0d6b7"], msg:"Copy: Landing hero", who:"s16", time:"20분 전", refs:["HEAD","main"]},
   {sha:"4e0d6b7", lane:0, parents:[], msg:"Chore: Site scaffold", who:"BREW", time:"어제", refs:["origin/main"]}
  ],
  prs:[], stashes:[], conflict:null,
  worktrees:[
   {who:"s16", branch:"copy/landing", path:".claude/worktrees/landing", computer:"mac-mini", dirty:0, ahead:2, behind:0, last:"Copy: Landing hero"},
   {who:"s17", branch:"deploy/docs", path:".claude/worktrees/docs-deploy", computer:"mac-mini", dirty:2, ahead:1, behind:0, last:"Docs: Deploy preview"}
  ]
 }
};

/* ---- session block (session agent): computer IPs, detected ports, session outputs ---- */
window.EMBER_DEMO.session = {
 ips: {"mac-mini":"192.168.0.21","macbook-pro":"192.168.0.34","ws-linux-01":"10.0.4.11","win-desktop":"192.168.0.40","gpu-box":"10.0.4.20"},
 ports: {
  s1:[{port:8080,proc:"ember-proxy",label:"VS Code Web"}],
  s2:[{port:5173,proc:"vite",label:"디자인 목업"},{port:6006,proc:"storybook"}],
  s4:[{port:3000,proc:"ember-server",label:"세션 API"},{port:9229,proc:"node --inspect"}],
  s10:[{port:3001,proc:"cargo run",label:"ember-server 실험"}],
  s3:[{port:7070,proc:"core-hub",label:"허브 대시보드"}],
  s15:[{port:7071,proc:"core-manager"}],
  s17:[{port:4321,proc:"astro dev",label:"사이트 미리보기"}],
  s8:[{port:8888,proc:"jupyter lab"}]
 },
 outputs: {
  s1:[{name:"Ember Mockup",kind:"아티팩트",icon:"spark"},{name:"docs/INTENT.md",kind:"문서",icon:"edit"}],
  s2:[{name:"session.css 스냅샷 비교",kind:"이미지",icon:"spark"}],
  s4:[{name:"ember/server/src/router.rs",kind:"코드",icon:"code"}],
  s3:[{name:"hub API 초안",kind:"문서",icon:"edit"}],
  s12:[{name:"VS Code Web 조사 메모",kind:"문서",icon:"edit"}]
 }
};
/* models per harness (v10 owner addition 4): used by Ember.modelPicker. Demo lists, not a provider catalogue.
   block: account plan (tier after " · " in the account string) -> reason shown when that plan cannot use the model. */
window.EMBER_DEMO.models = {
 "Claude Code": {vendor:"claude", def:"opus-5.5", effort:["low","medium","high"], defEffort:"high", list:[
  {id:"opus-5.5", name:"Opus 5.5", note:"가장 깊게 생각합니다", block:{Pro:"Pro 플랜에서는 쓸 수 없습니다"}},
  {id:"sonnet-5.5", name:"Sonnet 5.5", note:"속도와 품질의 균형"},
  {id:"haiku-5.5", name:"Haiku 5.5", note:"가장 빠르고 저렴합니다"}]},
 "Codex": {vendor:"codex", def:"gpt-5", effort:["low","medium","high"], defEffort:"medium", list:[
  {id:"gpt-5", name:"GPT-5", note:"기본 코딩 모델"},
  {id:"gpt-5-codex", name:"GPT-5 Codex", note:"긴 코딩 작업에 맞춥니다", block:{Plus:"Plus 플랜에서는 쓸 수 없습니다"}},
  {id:"gpt-5-mini", name:"GPT-5 mini", note:"빠른 수정용"}]},
 "Gemini CLI": {vendor:"gemini", def:"gemini-pro", effort:null, list:[
  {id:"gemini-pro", name:"Gemini Pro", note:"긴 문맥을 다룹니다"},
  {id:"gemini-flash", name:"Gemini Flash", note:"빠른 응답"},
  {id:"gemini-ultra", name:"Gemini Ultra", note:"가장 큰 모델", block:{Pro:"Pro 플랜에서는 쓸 수 없습니다"}}]}
};

/* ---------- monitor (컴퓨터 상태 모니터, monitor.js) ---------- */
window.EMBER_DEMO.monitor = {
 "mac-mini":   {chip:"M4 Pro", cores:12, gpuCores:20, ram:64, disk:2000, used:1640, uptime:"2일 22시간", since:"10월 22일", iface:"Ethernet", cpu:22, gpu:8, mem:52, temp:54, fan:1100,
  procs:[["WindowServer",408,9,3.1,494],["kernel_task",0,6,0,3200],["mds_stores",625,4,0.8,1100],["Docker Desktop",812,7,2.4,2100],["Safari",901,3,1.2,860],["launchd",1,0.4,0,22]]},
 "macbook-pro":{chip:"M3 Max", cores:16, gpuCores:40, ram:48, disk:1000, used:520, uptime:"5시간 10분", since:"오늘 오전 9시", iface:"Wi-Fi", cpu:12, gpu:4, mem:41, temp:46, fan:0,
  procs:[["WindowServer",388,7,2.2,410],["kernel_task",0,4,0,2600],["Slack",722,3,1.1,640],["Finder",515,0.8,0.2,120]]},
 "ws-linux-01":{chip:"Ryzen 9 7950X", cores:16, gpuCores:0, gpuName:"RTX 4090", ram:128, disk:4000, used:2100, uptime:"18일 3시간", since:"10월 7일", iface:"eth0", cpu:46, gpu:31, mem:63, temp:68, fan:1700,
  procs:[["cargo",3120,38,0,2400],["rustc",3188,24,0,1800],["postgres",902,5,0,780],["systemd",1,0.3,0,14],["sshd",811,0.6,0,9]]},
 "win-desktop":{chip:"Core i9-13900K", cores:24, gpuCores:0, gpuName:"RTX 4080", ram:64, disk:2000, used:1810, uptime:"0분", since:"연결 끊김", iface:"Ethernet", cpu:18, gpu:12, mem:58, temp:51, fan:900, seenAt:"10월 23일 오후 6:12",
  procs:[["MSBuild.exe",4410,21,0,1200],["explorer.exe",3020,2,0,180],["dwm.exe",1280,3,0,140]]},
 "gpu-box":    {chip:"확인 중", cores:8, gpuCores:0, gpuName:"확인 중", ram:32, disk:1000, used:300, uptime:"알 수 없음", since:"확인 중", iface:"eth0", cpu:0, gpu:0, mem:0, temp:0, fan:0, seenAt:"기록이 없습니다", procs:[]}
};

/* ==== agents block (agents screen agent). Harnesses, routing, egress and usage detail for agents.html.
   Grounded in SPEC FR-A1..A9, FR-A5e, FR-S8, FR-U1..U7, FR-X6 and INTENT D17..D19. Demo values only. ==== */
window.EMBER_DEMO.agents = {
 harnesses: [
  {id:"claude", name:"Claude Code", vendor:"claude", installed:true, version:"2.1.4", conn:"ok", mode:"stream-json", modes:["stream-json","acp"], sandbox:null, shim:true, rejoin:"live", mem:4, path:"/usr/local/bin/claude"},
  {id:"codex", name:"Codex", vendor:"codex", installed:true, version:"0.48.0", conn:"ok", mode:"app-server", modes:["app-server","acp"], sandbox:null, shim:true, rejoin:"resume", mem:4, path:"/opt/homebrew/bin/codex"},
  {id:"gemini", name:"Gemini CLI", vendor:"gemini", installed:true, version:"0.9.2", conn:"ok", mode:"acp", modes:["acp"], sandbox:null, shim:true, rejoin:"resume", mem:3, path:"/usr/local/bin/gemini"},
  {id:"agy", name:"Antigravity", vendor:"agy", installed:true, version:"1.2.16", pinned:"1.2.16", conn:"warn", mode:"per-turn", modes:["per-turn"], sandbox:false, shim:false, rejoin:"none", mem:6, path:"/usr/local/bin/agy"},
  {id:"omp", name:"OMP (oh-my-pi)", vendor:"acp", installed:false, version:"", conn:"off", mode:"acp", modes:["acp"], sandbox:null, shim:true, rejoin:"resume", mem:2, path:""}
 ],
 plans: {"Claude Code":["Pro","Max"], "Codex":["Plus","Pro"], "Gemini CLI":["Pro"]},
 /* profiles (v11 owner addition): account set + switch rules + optional proxy, assigned per harness. Members follow their harness's profile. */
 profiles: [
  {id:"pf-claude", name:"Claude 개인", harness:"Claude Code", mode:"auto", accounts:["claude:ncmnd","claude:work"], at:90, onFail:"next", policy:"order", proxy:null},
  {id:"pf-codex", name:"Codex 팀", harness:"Codex", mode:"fixed", accounts:["codex:ncmnd"], at:90, onFail:"wait", policy:"order", proxy:null},
  {id:"pf-opencodex", name:"Claude via opencodex", harness:"Claude Code", mode:"fixed", accounts:["claude:ncmnd"], at:90, onFail:"stop", policy:"order", proxy:{name:"opencodex", env:"ANTHROPIC_BASE_URL", url:"http://127.0.0.1:4000", model:"GPT-5 Codex"}},
  {id:"pf-gemini", name:"Gemini 기본", harness:"Gemini CLI", mode:"fixed", accounts:["gemini:ncmnd"], at:90, onFail:"stop", policy:"order", proxy:null}
 ],
 extraAccounts: [
  {vendor:"codex", name:"Codex", tier:"Plus", id:"work", left:64, five:21, reset:"6일 뒤"}
 ],
 harnessProfile: {"Claude Code":"pf-claude", "Codex":"pf-codex", "Gemini CLI":"pf-gemini"},
 egress: {on:true, computers:[
  {id:"mac-mini", ip:"211.36.142.18", via:"main", sessions:7, tunnels:12, state:"ok"},
  {id:"ws-linux-01", ip:"121.134.77.205", via:"node", sessions:6, tunnels:9, state:"ok"},
  {id:"macbook-pro", ip:"175.223.10.91", via:"node", sessions:0, tunnels:0, state:"ok"},
  {id:"win-desktop", ip:"", via:"node", sessions:0, tunnels:0, state:"lost", note:"연결이 끊겨 세션 3개가 메인 서버(mac-mini)로 옮겨졌습니다"},
  {id:"gpu-box", ip:"", via:"node", sessions:0, tunnels:0, state:"unknown"}]},
 usage: {
  days:["10/12","10/13","10/14","10/15","10/16","10/17","10/18","10/19","10/20","10/21","10/22","10/23","10/24","10/25"],
  c:[1.6,2.1,1.8,1.2,1.5,2.6,2.0,2.1,2.9,2.1,2.8,2.6,2.5,2.5], x:[0.8,1.0,0.9,0.6,0.7,1.2,1.0,0.9,1.3,1.0,1.3,1.2,1.1,1.1], g:[0.2,0.3,0.2,0.2,0.2,0.3,0.3,0.3,0.4,0.3,0.4,0.4,0.4,0.3],
  split:{input:1.1, output:0.4, cacheRead:2.2, cacheWrite:0.2},
  cost:[
   {harness:"Claude Code", model:"Opus 5.5", tok:21.4, usd:96.2},
   {harness:"Claude Code", model:"Sonnet 5.5", tok:9.8, usd:24.1},
   {harness:"Codex", model:"GPT-5", tok:10.2, usd:31.5},
   {harness:"Codex", model:"GPT-5 Codex", tok:3.9, usd:14.8},
   {harness:"Gemini CLI", model:"Gemini Pro", tok:4.3, usd:6.9},
   {harness:"Antigravity", model:"보고 안 함", tok:null, usd:null}]
 }
};

/* ---------- files (파일 전송 화면 files.html 전용 블록) ---------- */
window.EMBER_DEMO.files = {
 me: "mac-mini",
 inbox: "~/Downloads/Ember",
 devices: [
  {id:"mac-mini", name:"mac-mini", kind:"desktop", os:"macOS 26", state:"online", path:"self"},
  {id:"macbook-pro", name:"macbook-pro", kind:"laptop", os:"macOS 26", state:"online", path:"near", rtt:3},
  {id:"iphone", name:"BREW의 iPhone", kind:"phone", os:"iOS 26", state:"online", path:"near", rtt:6, battery:64},
  {id:"ws-linux-01", name:"ws-linux-01", kind:"server", os:"Ubuntu 24.04", state:"online", path:"p2p", rtt:18},
  {id:"ipad", name:"iPad Pro", kind:"tablet", os:"iPadOS 26", state:"online", path:"relay", rtt:41, battery:88},
  {id:"pixel", name:"Pixel 10", kind:"phone", os:"Android 17", state:"sleep", path:"relay", seen:"12분 전"},
  {id:"win-desktop", name:"win-desktop", kind:"desktop", os:"Windows 11", state:"offline", path:"relay", seen:"2일 전"},
  {id:"gpu-box", name:"gpu-box", kind:"server", os:"Linux (확인 중)", state:"unknown", path:"relay"}
 ],
 tray: [
  {name:"ose-build-arm64.tar.zst", type:"zip", size:2310},
  {name:"screen-390-dark.png", type:"img", size:1.8},
  {name:"SPEC-FR-files.pdf", type:"doc", size:0.6},
  {name:"demo-recording.mov", type:"video", size:842}
 ],
 transfers: [
  {id:"t1", name:"llama-4-scout-q4.gguf", type:"zip", size:6140, done:0.42, dir:"out", peer:"ws-linux-01", path:"p2p", speed:58, state:"run"},
  {id:"t2", name:"IMG_4021-4038 (18장)", type:"img", size:96, done:0.71, dir:"in", peer:"iphone", path:"near", speed:31, state:"run"},
  {id:"t3", name:"pyx-windows-artifacts.zip", type:"zip", size:1980, done:0.68, dir:"out", peer:"win-desktop", path:"relay", speed:0, state:"broken", note:"win-desktop 연결이 끊겨 멈췄습니다. 받은 부분은 남아 있어 연결되면 이어서 보냅니다."}
 ],
 received: [
  {id:"r1", name:"whiteboard-1024.heic", type:"img", size:3.4, from:"iphone", time:"오늘 오후 2:12", saved:"~/Downloads/Ember"},
  {id:"r2", name:"bench-results.csv", type:"doc", size:0.2, from:"ws-linux-01", time:"오늘 오전 11:40", saved:"~/Downloads/Ember"},
  {id:"r3", name:"voice-memo-설계.m4a", type:"audio", size:12.6, from:"pixel", time:"어제 오후 9:03", saved:"~/Downloads/Ember"},
  {id:"r4", name:"core-trace.tar.gz", type:"zip", size:388, from:"ws-linux-01", time:"어제 오후 4:27", saved:"~/work/darkpyonix-core/.ember/inbox"}
 ],
 history: [
  {name:"whiteboard-1024.heic", size:3.4, dir:"in", peer:"iphone", path:"near", day:"오늘", time:"오후 2:12", state:"done", secs:1},
  {name:"ember-proxy.log", size:14, dir:"out", peer:"s1", agent:true, path:"near", day:"오늘", time:"오후 1:55", state:"done", secs:1},
  {name:"bench-results.csv", size:0.2, dir:"in", peer:"ws-linux-01", path:"p2p", day:"오늘", time:"오전 11:40", state:"done", secs:1},
  {name:"keynote-draft.key", size:214, dir:"out", peer:"ipad", path:"relay", day:"오늘", time:"오전 10:02", state:"declined"},
  {name:"voice-memo-설계.m4a", size:12.6, dir:"in", peer:"pixel", path:"relay", day:"어제", time:"오후 9:03", state:"done", secs:4},
  {name:"core-trace.tar.gz", size:388, dir:"in", peer:"ws-linux-01", path:"p2p", day:"어제", time:"오후 4:27", state:"done", secs:9},
  {name:"pyx-msvc-toolchain.zip", size:3120, dir:"out", peer:"win-desktop", path:"relay", day:"10월 23일", time:"오후 6:40", state:"fail", why:"상대 장비 디스크 공간이 부족합니다"},
  {name:"site-hero.mp4", size:156, dir:"out", peer:"macbook-pro", path:"near", day:"10월 23일", time:"오후 3:18", state:"done", secs:2}
 ],
 week: {sent:9.4, recv:3.1, direct:86},
 offer: {from:"iphone", files:[{name:"IMG_4102.HEIC", type:"img", size:3.1},{name:"IMG_4103.HEIC", type:"img", size:2.9},{name:"회의 메모.pdf", type:"doc", size:0.4}]}
};
/* permission modes per harness (v11): used by Ember.modePicker. Demo subsets, not a provider catalogue. */
window.EMBER_DEMO.modes = {
 list: {
  ask: {name:"명령마다 확인", desc:"파일을 고치거나 명령을 실행하기 전에 매번 묻습니다"},
  edits: {name:"자동 승인(편집만)", desc:"파일 편집은 바로 하고, 명령은 실행 전에 묻습니다"},
  bypass: {name:"바이패스(모두 허용)", desc:"묻지 않고 모두 실행합니다. 믿을 수 있는 환경에서만 쓰세요", warn:true},
  plan: {name:"계획 모드", desc:"읽기만 하고 계획을 세운 뒤 승인을 기다립니다"}
 },
 byHarness: {"Claude Code":["ask","edits","bypass","plan"], "Codex":["ask","edits","bypass"], "Gemini CLI":["ask","edits","bypass"]},
 def: {"Claude Code":"ask", "Codex":"ask", "Gemini CLI":"ask"}
};
/* Ember.profileFor(harness) and Ember.profiles(): the agents screen's profiles in the shape core expects,
   {id, name, harness, account:"ncmnd · Max", mode, accounts, proxy}. account = the account a new session starts on
   (first in order that is under the switch threshold, else the first). Reads saved state (Ember.data) when present. */
(function(){var E=window.Ember=window.Ember||{};
 function src(){var d=E.data; return (d&&d.agents&&d.agents.profiles)?{A:d.agents,acc:d.accounts||[]}:{A:window.EMBER_DEMO.agents,acc:window.EMBER_DEMO.accounts||[]}}
 function shape(p,S){var all=S.acc.concat(S.A.extraAccounts||[]), L=p.accounts.map(function(k){return all.filter(function(x){return x.vendor+":"+x.id===k})[0]}).filter(Boolean);
  var act=p.mode==="auto"?(L.filter(function(x){return x.left==null||x.left>=100-p.at})[0]||L[0]):L[0];
  return {id:p.id, name:p.name, harness:p.harness, account:act?act.id+" · "+act.tier:"", mode:p.mode, accounts:p.accounts.slice(), proxy:p.proxy}}
 E.profiles=function(){var S=src(); return S.A.profiles.map(function(p){return shape(p,S)})};
 E.profileFor=function(h){var S=src(), id=(S.A.harnessProfile||{})[h], p=S.A.profiles.filter(function(x){return x.id===id})[0]; return p?shape(p,S):null};
 window.emberProfileFor=E.profileFor})();
