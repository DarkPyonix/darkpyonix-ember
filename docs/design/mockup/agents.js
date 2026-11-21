"use strict";
/* ---------- agents: harnesses, models, profiles (accounts + rules + proxy per harness), network egress, usage detail ----------
   Grounded in SPEC FR-A1..A9 (FR-A5e sandbox), FR-S8 (shim), FR-U1..U7, FR-X6 and INTENT D17..D19. Profiles follow the v11 owner addition (not yet in SPEC).
   State lives in D.agents (data.js agents block); model lists come from D.models (Ember.modelPicker). */
var agSec="harness", agPeriod=14, agDrag=null;
var AG_SECS=[["harness","하네스","cpu"],["models","모델","spark"],["profiles","프로필","user"],["network","네트워크","globe"],["usage","사용량","chart"]];
var AG_MODE={"stream-json":["stream-json","Claude Code의 --print 모드를 stream-json 입출력으로 구동합니다. 권한 요청은 stdio로 받습니다."],
 "app-server":["app-server","codex app-server를 JSON-RPC로 구동합니다."],
 "per-turn":["턴마다 실행","턴 하나마다 프로세스를 새로 띄우고 stream-json 출력을 읽습니다."],
 "acp":["ACP","Agent Client Protocol 어댑터로 구동합니다. 다른 방식과 비교 중인 후보입니다."]};
var AG_VEND={"Claude Code":"claude","Codex":"codex","Gemini CLI":"gemini"};
function agS(){if(!D.agents) D.agents=JSON.parse(JSON.stringify(window.EMBER_DEMO.agents)); var a=D.agents;
 if(!a.profiles){var Dm=window.EMBER_DEMO.agents; a.profiles=JSON.parse(JSON.stringify(Dm.profiles)); a.harnessProfile=JSON.parse(JSON.stringify(Dm.harnessProfile)); delete a.pipelines; delete a.members; delete a.proxies; delete a.routing}
 a.modelDef=a.modelDef||{}; a.modeDef=a.modeDef||{}; a.effortDef=a.effortDef||{}; a.hidden=a.hidden||{};
 Object.keys(D.models||{}).forEach(function(h){if(!a.modelDef[h]) a.modelDef[h]=D.models[h].def; if(a.effortDef[h]===undefined) a.effortDef[h]=D.models[h].defEffort||null});
 return a}
function agAccts(){return D.accounts.concat(agS().extraAccounts||[])}
function agKey(a){return a.vendor+":"+a.id}
function agAcct(k){return agAccts().filter(function(a){return agKey(a)===k})[0]}
function agHarnessOf(v){return {claude:"Claude Code",codex:"Codex",gemini:"Gemini CLI"}[v]||v}
function agRingColor(l){return l==null?"var(--c-grey)":l<15?"var(--c-orange)":"var(--c-green)"}
function agRing(l,cls){return '<div class="ring '+(cls||"")+'" style="--p:'+(l==null?0:l)+';--c:'+agRingColor(l)+'"><b>'+(l==null?'<span class="muted">모름</span>':l+'%')+'</b></div>'}
function agSw(on,attr,label,dis){return '<button class="switch'+(on?" on":"")+'" '+attr+' role="switch" aria-checked="'+!!on+'" aria-label="'+esc(label)+'"'+(dis?' disabled aria-disabled="true"':"")+'></button>'}
function agRow(t,d,ctl){return '<div class="ag-row"><div class="col ag-rl"><span class="ag-rt">'+t+'</span>'+(d?'<span class="ag-rd">'+d+'</span>':"")+'</div><div class="ag-rc">'+ctl+'</div></div>'}
function agOpt(list,v){return list.map(function(x){var k=Array.isArray(x)?x[0]:x,l=Array.isArray(x)?x[1]:x; return '<option value="'+esc(k)+'"'+(k===v?" selected":"")+'>'+esc(l)+'</option>'}).join("")}
function agCard(icon,title,body,right,cls){return '<section class="card ag-card'+(cls?" "+cls:"")+'"><div class="card-h"><div class="ico">'+I(icon)+'</div><span class="ellipsis">'+title+'</span><div class="spacer"></div>'+(right||"")+'</div><div class="card-b">'+body+'</div></section>'}

/* ---------- 1. harnesses ---------- */
function agConn(h){if(!h.installed) return '<span class="pill">설치 안 됨</span>';
 return h.conn==="ok"?'<span class="pill green">연결됨</span>':h.conn==="warn"?'<span class="pill orange">확인 필요</span>':'<span class="pill">끊김</span>'}
function agShimDesc(h){return h.rejoin==="live"?"서버가 다시 시작되거나 업데이트되어도 에이전트가 계속 돌고, 새 서버가 이어 붙습니다."
 :h.rejoin==="resume"?"shim은 살아 있지만 서버가 다시 시작되면 다음 메시지에서 에이전트 자체 resume으로 이어집니다."
 :"턴마다 프로세스를 새로 띄우므로 아직 shim 없이 직접 실행합니다."}
function agHarnessCard(h){var a=agS();
 var body=agRow("설치와 버전",h.installed?'<span class="mono">'+esc(h.path)+'</span>':"메인 서버에서 찾지 못했습니다.",
   (h.installed?'<span class="pill">v'+esc(h.version)+'</span>':"")+'<button class="btn sm" data-agact="detect" data-h="'+h.id+'">'+I("refresh")+'다시 찾기</button>')+
  (h.pinned?agRow("고정 버전",'Ember가 검증한 버전은 '+esc(h.pinned)+'입니다. 다른 버전은 읽기 전용으로만 실행됩니다.','<span class="pill '+(h.version===h.pinned?"green":"orange")+'">'+(h.version===h.pinned?"일치":"다름")+'</span>'):"")+
  (AG_VEND[h.name]?agRow("프로필",'이 하네스로 만든 팀원은 모두 이 프로필의 계정과 규칙을 따릅니다.',agPfSel(h.name,'data-agpfof="'+esc(h.name)+'"',h.name+" 프로필")):"")+
  agRow("실행 방식",AG_MODE[h.mode][1],'<select class="field ag-sel" data-agmode="'+h.id+'" aria-label="'+esc(h.name)+' 실행 방식"'+(h.modes.length<2?" disabled":"")+'>'+agOpt(h.modes.map(function(m){return [m,AG_MODE[m][0]]}),h.mode)+'</select>')+
  (h.sandbox!==null?agRow("샌드박스 (--sandbox)",h.sandbox?"켜져 있습니다(EMBER_AGY_SANDBOX=1). 샌드박스 안에서는 셸 쓰기가 실패합니다.":"기본으로 꺼져 있습니다. 승인 훅이 모든 도구 호출을 확인합니다.",agSw(h.sandbox,'data-agsandbox="'+h.id+'"',h.name+" 샌드박스")):"")+
  agRow("상주 shim",agShimDesc(h),agSw(h.shim&&h.rejoin!=="none",'data-agshim="'+h.id+'"',h.name+" 상주 shim",h.rejoin==="none"))+
  agRow("세션당 메모리 한도",'넘으면 그 세션만 종료되고 다른 세션은 계속됩니다.','<span class="ag-range"><input type="range" min="1" max="16" step="1" value="'+h.mem+'" data-agmem="'+h.id+'" aria-label="'+esc(h.name)+' 메모리 한도"><output class="ag-num">'+h.mem+' GB</output></span>');
 return '<section class="card ag-card ag-h'+(h.installed?"":" off")+'"><div class="card-h">'+bot(h.name,h.installed?(h.conn==="ok"?"done":"wait"):"done",30,h.name)+'<span class="col ag-ht"><span class="ellipsis">'+esc(h.name)+'</span><span class="ag-rd">'+AG_MODE[h.mode][0]+(h.installed?" · v"+esc(h.version):"")+'</span></span><div class="spacer"></div>'+agConn(h)+'</div><div class="card-b ag-rows">'+body+'</div></section>'}
function agHarness(){var L=agS().harnesses, inst=L.filter(function(h){return h.installed}).length, shim=L.filter(function(h){return h.shim&&h.rejoin!=="none"}).length, mem=L.filter(function(h){return h.installed}).reduce(function(s,h){return s+h.mem},0);
 return '<div class="kpis ag-kpis"><div class="kpi"><div class="l">설치된 하네스</div><div class="v">'+inst+'<span class="pill">전체 '+L.length+'</span></div></div>'+
  '<div class="kpi"><div class="l">상주 shim</div><div class="v">'+shim+'<span class="pill blue">켜짐</span></div></div>'+
  '<div class="kpi"><div class="l">세션 메모리 한도 합</div><div class="v">'+mem+'<span class="pill">GB</span></div></div>'+
  '<div class="kpi"><div class="l">마지막 감지</div><div class="v ag-word">방금</div></div></div>'+
  '<div class="ag-grid2">'+L.map(agHarnessCard).join("")+'</div>'}

/* ---------- 2. models ---------- */
function agModels(){var a=agS(), M=D.models||{};
 var cards=Object.keys(M).map(function(h){var c=M[h], plans=a.plans[h]||[];
  var eff=c.effort?'<div class="seg ag-eff" role="radiogroup" aria-label="'+esc(h)+' 기본 추론 강도">'+c.effort.map(function(e){return '<button role="radio" aria-checked="'+(a.effortDef[h]===e)+'" class="'+(a.effortDef[h]===e?"on":"")+'" data-ageff="'+esc(h)+'" data-e="'+e+'">'+{low:"낮음",medium:"보통",high:"높음"}[e]+'</button>'}).join("")+'</div>':'<span class="ag-rd">추론 강도를 고르지 않습니다</span>';
  var rows=c.list.map(function(m){var def=a.modelDef[h]===m.id, hid=a.hidden[h+"/"+m.id];
   return '<div class="ag-mrow'+(hid?" off":"")+'"><button class="ag-radio'+(def?" on":"")+'" role="radio" aria-checked="'+def+'" data-agdef="'+esc(h)+'" data-m="'+m.id+'" aria-label="'+esc(m.name)+' 기본 모델로"></button>'+
    '<span class="col ag-mn"><b class="ellipsis">'+esc(m.name)+(def?' <span class="pill blue">기본</span>':"")+'</b><span class="ag-rd ellipsis">'+esc(m.note||"")+'</span></span>'+
    '<span class="ag-plans">'+plans.map(function(p){var why=m.block&&m.block[p]; return '<span class="pill '+(why?"":"green")+'"'+(why?' title="'+esc(why)+'"':"")+'>'+(why?I("close"):I("check"))+p+'</span>'}).join("")+'</span>'+
    agSw(!hid,'data-aghide="'+esc(h+"/"+m.id)+'"',m.name+" 선택 목록에 표시",def)+'</div>'}).join("");
  return '<section class="card ag-card"><div class="card-h"><span class="dot '+c.vendor+'"></span><span class="ellipsis">'+esc(h)+'</span><div class="spacer"></div><span class="pill">모델 '+c.list.length+'</span></div><div class="card-b ag-rows">'+
   agRow("기본 추론 강도","새 팀원을 만들 때 처음 고른 값입니다.",eff)+
   (Ember.modePicker?agRow("기본 권한 모드","새 팀원이 도구를 쓸 때 확인하는 방식입니다. 바이패스는 모든 명령을 묻지 않고 실행합니다.",'<span class="ag-mdwrap" data-agmodeh="'+esc(h)+'">'+Ember.modePicker.html({harness:h, mode:a.modeDef[h]||((D.modes||{}).def||{})[h], name:"agmd"})+'</span>'):"")+
   '<div class="ag-mhead"><span>기본</span><span>모델</span><span>플랜별 사용</span><span>목록에 표시</span></div>'+rows+'</div></section>'}).join("");
 var nd=a.newDefault||{harness:"Claude Code",model:a.modelDef["Claude Code"],effort:a.effortDef["Claude Code"],account:"ncmnd · Max"};
 return agCard("plus","새 팀원 기본값",'<p class="ag-p">홈과 새 팀원 만들기, 루틴에서 처음 보이는 하네스와 모델입니다. 고른 계정의 플랜에서 쓸 수 없는 모델은 흐리게 표시됩니다.</p><div class="flex ag-wrap"><span id="agPick">'+(Ember.modelPicker?Ember.modelPicker.html({harness:nd.harness,model:nd.model,effort:nd.effort,account:nd.account,name:"agnew"}):"")+'</span><select class="field ag-sel" data-agndacct aria-label="계정">'+agOpt(agAccts().map(function(x){var k=x.id+" · "+x.tier;return [k,x.name+" "+k]}),nd.account)+'</select></div>')+
  '<div class="ag-grid2 ag-mt">'+cards+'</div>'}

function agAcctTile(x,extra){return '<div class="inset ag-acc">'+agRing(x.left,"ag-ring")+'<div class="col ag-acc-t"><div class="flex"><span class="dot '+x.vendor+'"></span><b class="ellipsis">'+esc(x.id)+'</b><span class="pill">'+esc(x.tier)+'</span></div><span class="ag-rd">'+esc(x.name)+' · 리셋 '+esc(x.reset)+'</span>'+
 (x.five==null?'<span class="ag-rd">5시간 한도를 알려 주지 않습니다</span>':'<div class="flex ag-five"><span class="ag-rd">5h</span><span class="track"><i style="width:'+x.five+'%;background:'+(x.five>80?"var(--c-orange)":"var(--c-green)")+'"></i></span><span class="ag-num">'+x.five+'%</span></div>')+(extra||"")+'</div></div>'}
/* ---------- 3. profiles: account set + switch rules + optional proxy, one default per harness (v11) ---------- */
function agPf(id){return agS().profiles.filter(function(p){return p.id===id})[0]}
function agPfUsers(p){return Object.keys(agS().harnessProfile).filter(function(h){return agS().harnessProfile[h]===p.id})}
function agPfPick(p){var L=p.accounts.map(agAcct).filter(Boolean), skip=[], pick=null;
 if(p.mode==="fixed") return {pick:L[0]||null, skip:[]};
 var cand=p.policy==="least"?L.slice().sort(function(x,y){return (y.left==null?-1:y.left)-(x.left==null?-1:x.left)}):L;
 cand.forEach(function(x){if(pick) return; if(x.left!=null&&x.left<100-p.at) skip.push(x); else pick=x});
 return {pick:pick, skip:skip}}
function agPfSel(h,attr,label){var a=agS(), L=a.profiles.filter(function(p){return p.harness===h});
 return '<select class="field ag-sel" '+attr+' aria-label="'+esc(label)+'">'+agOpt([["","프로필 없음"]].concat(L.map(function(p){return [p.id,p.name]})),a.harnessProfile[h]||"")+'</select>'}
function agPfCard(p){var a=agS(), users=agPfUsers(p), v=AG_VEND[p.harness]||"", same=agAccts().filter(function(x){return agHarnessOf(x.vendor)===p.harness}), rest=same.filter(function(x){return p.accounts.indexOf(agKey(x))<0}), r=agPfPick(p);
 var accts=p.mode==="fixed"?
   agRow("계정","이 계정 하나로만 실행됩니다.",'<select class="field ag-sel" data-agpffix="'+p.id+'" aria-label="고정 계정">'+agOpt(same.map(function(x){return [agKey(x),x.id+" · "+x.tier]}),p.accounts[0])+'</select>')
  :'<div class="ag-row ag-colrow"><div class="col ag-rl"><span class="ag-rt">전환 순서</span><span class="ag-rd">위에서부터 씁니다. 끌어서 순서를 바꾸세요.</span></div></div><ol class="ag-dlist">'+p.accounts.map(function(k,i){var x=agAcct(k); if(!x) return "";
   return '<li class="ag-drow" draggable="true" data-agdrag="'+p.id+'" data-i="'+i+'"><span class="ag-handle no-press" aria-hidden="true">'+I("menu")+'</span><span class="ag-rank">'+(i+1)+'</span>'+agRing(x.left,"ag-ring-s")+'<span class="col ag-acc-t"><b class="ellipsis">'+esc(x.id)+' · '+esc(x.tier)+'</b><span class="ag-rd">리셋 '+esc(x.reset)+'</span></span><span class="spacer"></span>'+
    '<button class="icon-btn sm" data-agmv="'+p.id+'" data-i="'+i+'" data-d="-1" aria-label="위로"'+(i===0?" disabled":"")+'>'+I("up")+'</button><button class="icon-btn sm" data-agmv="'+p.id+'" data-i="'+i+'" data-d="1" aria-label="아래로"'+(i===p.accounts.length-1?" disabled":"")+'>'+I("down")+'</button><button class="icon-btn sm" data-agrm="'+p.id+'" data-i="'+i+'" aria-label="빼기"'+(p.accounts.length<2?" disabled":"")+'>'+I("close")+'</button></li>'}).join("")+'</ol>'+
   (rest.length?'<div class="flex ag-wrap ag-mt"><select class="field ag-sel" data-agaddsel="'+p.id+'" aria-label="추가할 계정">'+agOpt(rest.map(function(x){return [agKey(x),x.id+" · "+x.tier]}),"")+'</select><button class="btn sm" data-agadd="'+p.id+'">'+I("plus")+'추가</button></div>':"");
 var rules=(p.mode==="auto"?agRow("다음 계정으로 넘어가는 때","이 사용률에 닿은 계정은 건너뛰고 다음 계정에서 시작합니다.",'<select class="field ag-sel" data-agat="'+p.id+'" aria-label="전환 사용률">'+agOpt([70,80,85,90,95].map(function(n){return [String(n),n+"%"]}),String(p.at))+'</select>')+
   agRow("라우팅 정책","새 세션의 계정을 고르는 방법입니다.",'<div class="seg" role="radiogroup" aria-label="라우팅 정책">'+[["order","순서대로"],["least","많이 남은 계정"]].map(function(x){return '<button role="radio" aria-checked="'+(p.policy===x[0])+'" class="'+(p.policy===x[0]?"on":"")+'" data-agpol="'+p.id+'" data-v="'+x[0]+'">'+x[1]+'</button>'}).join("")+'</div>'):"")+
  agRow("실패할 때","속도 제한이나 인증 오류가 나면 이렇게 처리합니다.",'<select class="field ag-sel" data-agfail="'+p.id+'" aria-label="실패 시 처리">'+agOpt((p.mode==="auto"?[["next","다음 계정으로"]]:[]).concat([["wait","리셋까지 기다림"],["stop","멈추고 알림"]]),p.onFail)+'</select>')+
  agRow("모델 프록시",p.proxy?'<span class="mono">'+esc(p.proxy.env)+'='+esc(p.proxy.url)+'</span>':"하네스의 API 주소를 프록시로 돌려 다른 출처의 모델을 씁니다. 프록시 서버는 Ember에 들어 있지 않습니다.",agSw(!!p.proxy,'data-agpx="'+p.id+'"',p.name+" 모델 프록시"))+
  (p.proxy?'<div class="ag-pxf"><label class="ag-lab">프록시 주소<input class="field ag-sel mono" data-agpxu="'+p.id+'" value="'+esc(p.proxy.url)+'"></label><label class="ag-lab">모델<input class="field ag-sel" data-agpxm="'+p.id+'" value="'+esc(p.proxy.model)+'"></label></div>':"");
 return '<section class="card ag-card ag-pf"><div class="card-h"><span class="dot '+v+'"></span><input class="field ag-pfname" data-agpfname="'+p.id+'" value="'+esc(p.name)+'" aria-label="프로필 이름"><div class="spacer"></div>'+(users.length?'<span class="pill blue">'+esc(users.join(", "))+' 기본</span>':"")+'<button class="icon-btn sm" data-agpfdel="'+p.id+'" aria-label="프로필 지우기"'+(users.length?' disabled title="기본으로 쓰는 하네스가 있어 지울 수 없습니다"':"")+'>'+I("close")+'</button></div><div class="card-b ag-rows">'+
  agRow("대상 하네스","",'<select class="field ag-sel" data-agpfh="'+p.id+'" aria-label="대상 하네스">'+agOpt(Object.keys(AG_VEND),p.harness)+'</select>')+
  agRow("계정 방식","",'<div class="seg" role="radiogroup" aria-label="계정 방식">'+[["fixed","고정"],["auto","자동 전환"]].map(function(x){return '<button role="radio" aria-checked="'+(p.mode===x[0])+'" class="'+(p.mode===x[0]?"on":"")+'" data-agpfmode="'+p.id+'" data-v="'+x[0]+'"'+(x[0]==="auto"&&same.length<2?' disabled title="이 하네스의 계정이 하나뿐입니다"':"")+'>'+x[1]+'</button>'}).join("")+'</div>')+
  accts+rules+
  '<div class="inset ag-preview ag-mt"><span class="ag-rd">지금 새 세션을 열면</span><div class="flex ag-wrap">'+(r.pick?agRing(r.pick.left,"ag-ring-s")+'<b>'+esc(r.pick.id)+' · '+esc(r.pick.tier)+'</b><span class="pill green">이 계정에서 시작됩니다</span>':'<b>쓸 수 있는 계정이 없습니다</b>')+(p.proxy?'<span class="pill">'+I("arrow")+esc(p.proxy.model)+'</span>':"")+'</div>'+
  r.skip.map(function(x){return '<span class="ag-rd">'+I("warn")+esc(x.id)+'는 사용률이 '+p.at+'%를 넘어 건너뜁니다</span>'}).join("")+'</div></div></section>'}
function agProfiles(){var a=agS();
 var intro='<div class="ag-formula"><span class="chip ag-fchip">'+I("user")+'계정 묶음</span><span class="ag-op">+</span><span class="chip ag-fchip">'+I("repeat")+'전환 규칙</span><span class="ag-op">+</span><span class="chip ag-fchip ag-opt">'+I("arrow")+'모델 프록시 (선택)</span><span class="ag-op">=</span><span class="chip ag-fchip">'+I("spark")+'프로필</span></div><p class="ag-p">하네스마다 프로필 하나를 정하면 그 하네스로 만든 팀원이 모두 그 프로필을 따릅니다. 팀원마다 따로 설정하지 않습니다.</p>';
 var accts=agCard("user","계정",'<div class="ag-accs">'+agAccts().map(function(x){return agAcctTile(x,x.left!=null&&x.left<15?'<span class="pill orange">한도 가까움</span>':"")}).join("")+'</div>','<button class="btn sm" data-agact="addacct">'+I("plus")+'<span class="hide-sm">계정 추가</span></button>');
 var assign=agCard("cpu","하네스별 기본 프로필",'<div class="ag-rows">'+Object.keys(AG_VEND).map(function(h){var p=agPf(a.harnessProfile[h]); return agRow('<span class="flex"><span class="dot '+AG_VEND[h]+'"></span>'+esc(h)+'</span>',p?(p.mode==="auto"?"계정 "+p.accounts.length+"개 자동 전환":"고정 계정")+(p.proxy?" · 프록시 "+esc(p.proxy.model):""):"프로필이 없어 팀원을 만들 때 계정을 직접 고릅니다.",agPfSel(h,'data-agpfof="'+esc(h)+'"',h+" 기본 프로필"))}).join("")+'</div>');
 return intro+'<div class="ag-stack">'+accts+assign+'<div class="flex"><h2 class="ag-h2 spacer">프로필 '+a.profiles.length+'개</h2><button class="btn sm" data-agact="addpf">'+I("plus")+'새 프로필</button></div><div class="ag-grid2">'+a.profiles.map(agPfCard).join("")+'</div></div>'}

/* ---------- 4. network egress ---------- */
function agNetwork(){var e=agS().egress;
 var flow=['<span class="chip ag-fchip">'+I("code")+'에이전트 CLI</span>','<span class="chip ag-fchip">'+I("shield")+'세션 프록시 (루프백)</span>','<span class="chip ag-fchip">'+I("pc")+'작업 중인 컴퓨터</span>','<span class="chip ag-fchip">'+I("globe")+'모델 공급자</span>'].join('<span class="ag-op">'+I("arrow")+'</span>');
 var hero=agCard("globe","API 트래픽 내보내기",agRow("작업 중인 컴퓨터로 내보내기",e.on?"에이전트의 API 요청이 그 세션이 일하는 컴퓨터에서 나가므로 공급자는 그 컴퓨터의 IP를 봅니다.":"꺼져 있습니다(EMBER_AGENT_EGRESS=off). 모든 요청이 메인 서버 IP로 나갑니다.",agSw(e.on,'data-agegress','API 트래픽 내보내기'))+
  '<div class="ag-formula ag-mt'+(e.on?"":" off")+'">'+flow+'</div><p class="ag-p ag-mt">컴퓨터를 바꾸면 새 연결부터 새 컴퓨터를 따르고, 이미 열린 연결은 원래 컴퓨터를 씁니다.</p>','<span class="pill orange" title="실제 CLI와 두 컴퓨터에서 확인하지 않았습니다 (#130)">검증 전</span>');
 var tiles=e.computers.map(function(c){var info=(D.computers||[]).filter(function(x){return x.id===c.id})[0]||{}, st=c.state==="ok"?'<span class="pill green">정상</span>':c.state==="lost"?'<span class="pill red">끊김</span>':'<span class="pill">확인 중</span>';
  return '<div class="inset ag-ctile'+(c.state==="ok"&&e.on?"":" dim")+'"><div class="flex"><span class="ag-cico">'+I(info.kind==="laptop"?"pc":"server")+'</span><span class="col ag-pn"><b class="ellipsis">'+esc(c.id)+'</b><span class="ag-rd ellipsis">'+esc(info.os||"")+(c.via==="main"?" · 메인 서버":"")+'</span></span>'+st+'</div>'+
   '<div class="ag-ip mono">'+(c.ip&&e.on?esc(c.ip):c.ip?"메인 서버 IP 사용":"IP 없음")+'</div>'+
   '<div class="ag-cstat"><span><b class="ag-num">'+c.sessions+'</b><span class="ag-rd">세션</span></span><span><b class="ag-num">'+c.tunnels+'</b><span class="ag-rd">열린 연결</span></span><button class="btn sm" data-agip="'+c.id+'"'+(c.state!=="ok"?" disabled":"")+'>IP 확인</button></div>'+
   (c.note?'<span class="ag-rd">'+esc(c.note)+'</span>':"")+'</div>'}).join("");
 return '<div class="ag-stack">'+hero+agCard("server","컴퓨터별 상태",'<div class="ag-ctiles">'+tiles+'</div>')+'</div>'}

/* ---------- 5. usage detail ---------- */
function agUsage(){var U=agS().usage, n=agPeriod, days=U.days.slice(-n), c=U.c.slice(-n), x=U.x.slice(-n), g=U.g.slice(-n);
 var tot=days.map(function(_,i){return c[i]+x[i]+g[i]}), mx=Math.max.apply(null,tot), today=tot[tot.length-1], avg=tot.reduce(function(a,b){return a+b},0)/tot.length;
 var bars=days.map(function(d,i){return '<div class="bcol" title="'+d+' '+tot[i].toFixed(1)+'M"><div class="bar" style="height:'+Math.round(tot[i]/mx*100)+'%"><i class="c" style="flex:'+c[i]+'"></i><i class="x" style="flex:'+x[i]+'"></i><i class="g" style="flex:'+g[i]+'"></i></div><span'+(i===n-1?' class="today"':"")+'>'+(n>7&&i%2&&i!==n-1?"":d.split("/")[1])+'</span></div>'}).join("");
 var sum=function(l){return l.reduce(function(a,b){return a+b},0).toFixed(1)};
 var hero='<section class="card ag-card"><div class="card-b ag-hero"><div class="col ag-hero-l"><span class="muted">오늘 사용량</span><div class="big">'+today.toFixed(1)+'M</div><span class="muted small">토큰 · '+n+'일 평균 '+avg.toFixed(1)+'M</span><div class="legend ag-mt"><span class="flex"><span class="dot claude"></span>Claude '+sum(c)+'M</span><span class="flex"><span class="dot codex"></span>Codex '+sum(x)+'M</span><span class="flex"><span class="dot gemini"></span>Gemini '+sum(g)+'M</span></div></div><div class="ag-bars"><div class="bars2">'+bars+'</div></div></div></section>';
 var sp=U.split, st=sp.input+sp.output+sp.cacheRead+sp.cacheWrite, kinds=[["입력",sp.input,"var(--c-blue)"],["출력",sp.output,"var(--c-orange)"],["캐시 읽기",sp.cacheRead,"var(--c-sky)"],["캐시 쓰기",sp.cacheWrite,"var(--c-green)"]];
 var split=agCard("chart","오늘 토큰 종류",'<div class="ag-split" role="img" aria-label="토큰 종류 비율">'+kinds.map(function(k){return '<i style="flex:'+k[1]+';background:'+k[2]+'"></i>'}).join("")+'</div><div class="ag-kinds">'+kinds.map(function(k){return '<div class="col"><span class="flex ag-rd"><span class="dot" style="background:'+k[2]+'"></span>'+k[0]+'</span><b class="ag-kv">'+k[1].toFixed(1)+'M</b><span class="ag-rd">'+Math.round(k[1]/st*100)+'%</span></div>'}).join("")+'</div><p class="ag-p ag-mt">하네스가 알려 준 실제 토큰 수입니다. 알려 주지 않는 하네스는 추정하지 않고 모름으로 표시됩니다.</p>');
 var groups={}; agAccts().forEach(function(a){(groups[a.vendor]=groups[a.vendor]||[]).push(a)});
 var rings='<div class="ag-vgrid">'+Object.keys(groups).map(function(v){var l=groups[v];
  return '<section class="card ag-card ag-vcard'+(l.length>1?" wide":"")+'"><div class="card-b"><div class="flex"><span class="dot '+v+'"></span><b class="ag-vt">'+esc(l[0].name)+'</b><span class="pill">'+l.map(function(a){return a.tier}).filter(function(t,i,s){return s.indexOf(t)===i}).join(", ")+'</span></div><div class="ag-accs ag-mt">'+l.map(function(a){return agAcctTile(a)}).join("")+'</div></div></section>'}).join("")+'</div>';
 var C=U.cost, known=C.filter(function(r){return r.usd!=null}), mxu=Math.max.apply(null,known.map(function(r){return r.usd})), usd=known.reduce(function(a,r){return a+r.usd},0);
 var byH={}; known.forEach(function(r){byH[r.harness]=(byH[r.harness]||0)+r.usd});
 var col=function(h){return AG_VEND[h]?"var(--v-"+AG_VEND[h]+")":"var(--c-grey)"};
 var cost=agCard("chart","하네스와 모델별 비용 (이번 달)",'<div class="ag-cost"><div class="ag-donut">'+donut(Object.keys(byH).map(function(h){return {v:byH[h],c:col(h),l:h}}),132,"$"+Math.round(usd),"이번 달")+'<div class="legend col">'+Object.keys(byH).map(function(h){return '<span class="flex"><span class="dot" style="background:'+col(h)+'"></span>'+esc(h)+' $'+byH[h].toFixed(0)+'</span>'}).join("")+'</div></div>'+
  '<div class="ag-hbars">'+C.map(function(r){return '<div class="ag-hb"><span class="col ag-pn"><b class="ellipsis">'+esc(r.model)+'</b><span class="ag-rd ellipsis">'+esc(r.harness)+' · '+(r.tok==null?"토큰 모름":r.tok.toFixed(1)+"M 토큰")+'</span></span><span class="track"><i style="width:'+(r.usd==null?0:r.usd/mxu*100)+'%;background:'+col(r.harness)+'"></i></span><b class="ag-num">'+(r.usd==null?'<span class="muted">모름</span>':"$"+r.usd.toFixed(1))+'</b></div>'}).join("")+'</div></div>');
 return '<div class="seg ag-period" role="radiogroup" aria-label="기간">'+[7,14].map(function(v){return '<button role="radio" aria-checked="'+(agPeriod===v)+'" class="'+(agPeriod===v?"on":"")+'" data-agper="'+v+'">'+v+'일</button>'}).join("")+'</div>'+
  '<div class="ag-stack">'+hero+'<div class="ag-grid2 ag-g12">'+split+cost+'</div><h2 class="ag-h2">계정별 남은 구독량</h2>'+rings+'</div>'}

/* ---------- shell ---------- */
var AG_FN={harness:agHarness,models:agModels,profiles:agProfiles,network:agNetwork,usage:agUsage};
function agFromHash(){var h=location.hash.slice(1); if(h==="routing") h="profiles"; if(AG_FN[h]) agSec=h}
function agBody(){var b=$("#agBody"); if(!b) return; b.innerHTML=AG_FN[agSec]();
 [].slice.call(document.querySelectorAll("[data-agsec]")).forEach(function(n){var on=n.dataset.agsec===agSec; n.classList.toggle("on",on); n.setAttribute("aria-selected",on)})}
function agents(){agS(); agFromHash();
 var nav=AG_SECS.map(function(s){return '<button class="ag-nb" role="tab" data-agsec="'+s[0]+'" aria-selected="'+(s[0]===agSec)+'">'+I(s[2])+'<span>'+s[1]+'</span></button>'}).join("");
 render(shell("agents",'<main class="main ag">'+head("에이전트","하네스와 모델, 프로필, 네트워크, 사용량을 설정합니다","","",{primary:false})+'<div class="ag-layout"><nav class="ag-nav" role="tablist" aria-label="에이전트 설정">'+nav+'</nav><div class="ag-body" id="agBody" role="tabpanel"></div></div></main>'));
 agBody()}
function agRedraw(msg){save(); var y=$(".main")?$(".main").scrollTop:0; agBody(); if($(".main")) $(".main").scrollTop=y; if(msg) toast(msg)}
function agH(id){return agS().harnesses.filter(function(h){return h.id===id})[0]}
function agPl(id){return agPf(id)}
Ember.screens.agents={render:agents, onHash:function(){agFromHash(); agBody()}, click:function(e,t){var a, s=agS();
 if((a=t.closest("[data-agsec]"))){if(location.hash.slice(1)!==a.dataset.agsec) history.replaceState(null,"","#"+a.dataset.agsec); agSec=a.dataset.agsec; agBody(); if($(".main")) $(".main").scrollTop=0; return true}
 if((a=t.closest("[data-agact]"))){var k=a.dataset.agact;
  if(k==="detect"){var h=agH(a.dataset.h); toast(h.installed?h.name+" v"+h.version+"을 찾았습니다":h.name+"을 찾지 못했습니다. 설치 뒤 다시 찾으세요")}
  else if(k==="addacct") toast("계정 추가: 로그인 창이 열립니다 (데모)");
  else if(k==="addpf"){var f0=agAccts().filter(function(x){return x.vendor==="claude"})[0]; s.profiles.push({id:"pf"+Date.now(),name:"새 프로필",harness:"Claude Code",mode:"fixed",accounts:[agKey(f0)],at:90,onFail:"wait",policy:"order",proxy:null}); agRedraw("새 프로필을 만들었습니다. 이름을 바꾸세요")}
  return true}
 if((a=t.closest("[data-agsandbox]"))){var h1=agH(a.dataset.agsandbox); h1.sandbox=!h1.sandbox; agRedraw(h1.sandbox?"샌드박스를 켰습니다. 셸 쓰기가 실패합니다":"샌드박스를 껐습니다"); return true}
 if((a=t.closest("[data-agshim]"))){if(a.disabled) return true; var h2=agH(a.dataset.agshim); h2.shim=!h2.shim; agRedraw(h2.shim?"상주 shim을 켰습니다":"shim을 껐습니다. 서버가 멈추면 이 하네스의 세션도 멈춥니다"); return true}
 if((a=t.closest("[data-ageff]"))){s.effortDef[a.dataset.ageff]=a.dataset.e; agRedraw(); return true}
 if((a=t.closest("[data-agdef]"))){s.modelDef[a.dataset.agdef]=a.dataset.m; delete s.hidden[a.dataset.agdef+"/"+a.dataset.m]; agRedraw("기본 모델을 바꿨습니다"); return true}
 if((a=t.closest("[data-aghide]"))){if(a.disabled) return true; var hk=a.dataset.aghide; if(s.hidden[hk]) delete s.hidden[hk]; else s.hidden[hk]=true; agRedraw(); return true}
 if((a=t.closest("[data-agmv]"))){var p=agPf(a.dataset.agmv), i=+a.dataset.i, j=i+(+a.dataset.d); if(j<0||j>=p.accounts.length) return true; var tmp=p.accounts[i]; p.accounts[i]=p.accounts[j]; p.accounts[j]=tmp; agRedraw(); return true}
 if((a=t.closest("[data-agrm]"))){var p2=agPf(a.dataset.agrm); if(p2.accounts.length>1){p2.accounts.splice(+a.dataset.i,1); agRedraw("전환 순서에서 뺐습니다")} return true}
 if((a=t.closest("[data-agadd]"))){var p3=agPf(a.dataset.agadd), sel=$('[data-agaddsel="'+p3.id+'"]'); if(sel&&sel.value){p3.accounts.push(sel.value); agRedraw("전환 순서 끝에 추가했습니다")} return true}
 if((a=t.closest("[data-agpol]"))){agPf(a.dataset.agpol).policy=a.dataset.v; agRedraw(); return true}
 if((a=t.closest("[data-agpfmode]"))){if(a.disabled) return true; var p5=agPf(a.dataset.agpfmode); p5.mode=a.dataset.v;
  if(p5.mode==="auto"){agAccts().forEach(function(x){if(agHarnessOf(x.vendor)===p5.harness&&p5.accounts.indexOf(agKey(x))<0) p5.accounts.push(agKey(x))}); p5.onFail="next"} else {p5.accounts=p5.accounts.slice(0,1); if(p5.onFail==="next") p5.onFail="wait"}
  agRedraw(); return true}
 if((a=t.closest("[data-agpx]"))){var p6=agPf(a.dataset.agpx); p6.proxy=p6.proxy?null:{name:"프록시", env:p6.harness==="Codex"?"OPENAI_BASE_URL":"ANTHROPIC_BASE_URL", url:"http://127.0.0.1:4000", model:"모델 이름"}; agRedraw(p6.proxy?"모델 프록시를 켰습니다":"모델 프록시를 껐습니다"); return true}
 if((a=t.closest("[data-agpfdel]"))){if(a.disabled) return true; var p7=agPf(a.dataset.agpfdel); s.profiles.splice(s.profiles.indexOf(p7),1); agRedraw(p7.name+" 프로필을 지웠습니다"); return true}
 if((a=t.closest("[data-agegress]"))){s.egress.on=!s.egress.on; agRedraw(s.egress.on?"작업 중인 컴퓨터로 내보냅니다":"메인 서버에서 직접 내보냅니다"); return true}
 if((a=t.closest("[data-agip]"))){var c=s.egress.computers.filter(function(x){return x.id===a.dataset.agip})[0]; toast(c.id+" 세션이 본 IP: "+(s.egress.on?c.ip:"메인 서버 IP")); return true}
 if((a=t.closest("[data-agper]"))){agPeriod=+a.dataset.agper; agBody(); return true}
 return false}};
/* change events: selects, ranges, text inputs, model picker */
document.addEventListener("input",function(e){var t=e.target; if(t.dataset&&t.dataset.agmem){var o=t.parentNode.querySelector("output"); if(o) o.textContent=t.value+" GB"}});
document.addEventListener("change",function(e){if(document.body.dataset.screen!=="agents") return; var t=e.target, d=t.dataset||{}, s=agS();
 if(d.agmem){agH(d.agmem).mem=+t.value; agRedraw("메모리 한도를 "+t.value+" GB로 바꿨습니다")}
 else if(d.agmode){agH(d.agmode).mode=t.value; agRedraw("실행 방식을 바꿨습니다. 다음 세션부터 적용됩니다")}
 else if(d.agat){agPf(d.agat).at=+t.value; agRedraw()}
 else if(d.agfail){agPf(d.agfail).onFail=t.value; save()}
 else if(d.agpffix){agPf(d.agpffix).accounts=[t.value]; agRedraw()}
 else if(d.agpfname){var pn=agPf(d.agpfname); pn.name=t.value.trim()||pn.name; agRedraw("이름을 바꿨습니다")}
 else if(d.agpfh){var ph=agPf(d.agpfh), f1=agAccts().filter(function(x){return agHarnessOf(x.vendor)===t.value})[0]; Object.keys(s.harnessProfile).forEach(function(h){if(s.harnessProfile[h]===ph.id) s.harnessProfile[h]=""}); ph.harness=t.value; ph.mode="fixed"; ph.accounts=f1?[agKey(f1)]:[]; if(ph.onFail==="next") ph.onFail="wait"; agRedraw()}
 else if(d.agpfof!==undefined){s.harnessProfile[d.agpfof]=t.value; agRedraw(t.value?d.agpfof+" 팀원이 "+agPf(t.value).name+" 프로필을 따릅니다":d.agpfof+" 기본 프로필을 뺐습니다")}
 else if(d.agpxu){agPf(d.agpxu).proxy.url=t.value; save()}
 else if(d.agpxm){agPf(d.agpxm).proxy.model=t.value; agRedraw()}
 else if(d.agndacct!==undefined){var nd=s.newDefault||{harness:"Claude Code",model:s.modelDef["Claude Code"],effort:s.effortDef["Claude Code"]}; nd.account=t.value; s.newDefault=nd; agRedraw()}});
document.addEventListener("modechange",function(e){if(document.body.dataset.screen!=="agents") return; var w=e.target.closest&&e.target.closest("[data-agmodeh]"); if(!w) return; agS().modeDef[w.dataset.agmodeh]=e.detail.mode; save(); toast(w.dataset.agmodeh+" 기본 권한 모드: "+e.detail.modeName)});
document.addEventListener("modelchange",function(e){if(document.body.dataset.screen!=="agents"||!e.target.closest("#agPick")) return; var v=e.detail, s=agS(); s.newDefault={harness:v.harness,model:v.model,effort:v.effort,account:(s.newDefault||{}).account||"ncmnd · Max"}; save()});
/* drag to reorder a profile's accounts */
document.addEventListener("dragstart",function(e){var r=e.target.closest&&e.target.closest("[data-agdrag]"); if(!r) return; agDrag={pl:r.dataset.agdrag,i:+r.dataset.i}; r.classList.add("dragging"); try{e.dataTransfer.effectAllowed="move"; e.dataTransfer.setData("text/plain",r.dataset.i)}catch(err){}});
document.addEventListener("dragover",function(e){var r=e.target.closest&&e.target.closest("[data-agdrag]"); if(!r||!agDrag||r.dataset.agdrag!==agDrag.pl) return; e.preventDefault(); [].slice.call(document.querySelectorAll(".ag-drow.over")).forEach(function(n){n.classList.remove("over")}); r.classList.add("over")});
document.addEventListener("drop",function(e){var r=e.target.closest&&e.target.closest("[data-agdrag]"); if(!r||!agDrag||r.dataset.agdrag!==agDrag.pl) return; e.preventDefault(); var p=agPf(agDrag.pl), to=+r.dataset.i, it=p.accounts.splice(agDrag.i,1)[0]; p.accounts.splice(to,0,it); agDrag=null; agRedraw("순서를 바꿨습니다")});
document.addEventListener("dragend",function(){agDrag=null; [].slice.call(document.querySelectorAll(".ag-drow.dragging,.ag-drow.over")).forEach(function(n){n.classList.remove("dragging","over")})});
