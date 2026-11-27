"use strict";
/* ---------- tools (INTENT D58, owner 2026-11-27) ----------
   ember-server's Tools section: integrations (moved here from the Home dashboard), MCP servers, and CLIs registered on the
   server. Every agent CLI runs on ember-server; a node receives work and finds a server CLI through its terminal
   environment (environment variable first, then PATH). Access to each tool is set per project and per agent (team member):
   허용 (allow), 요청 시 확인 (ask: every use waits for approval), 차단 (deny).
   Rule: a member's own setting comes before its project's; a project set to 차단 blocks every member in it.
   Hash for captures: tools.html#int, #mcp, #cli filter the list; tools.html#int:github, #mcp:playwright, #cli:gh open that
   tool's access sheet (add /rules on an integration for its rules tab, e.g. #int:calendar/rules). */
if(!D.tools) D.tools=JSON.parse(JSON.stringify(window.EMBER_DEMO.tools));
if(!D.integrations) D.integrations=JSON.parse(JSON.stringify(window.EMBER_DEMO.integrations));
var TL_ST={allow:["허용","green"],ask:["요청 시 확인","orange"],deny:["차단","red"]};
var TL_SHORT={allow:"허용",ask:"확인",deny:"차단"};
var TL_KIND={int:["연동","globe"],mcp:["MCP 서버","server-stack"],cli:["서버 CLI","cmd"]};
var tlFilter="all", tlSel=null; /* tlSel = {key, tab:"access"|"rules", proj:"all"|projectId} */

/* ---- access model ---- */
function tlAcc(key){var A=D.tools.access; if(!A[key]) A[key]={def:"ask",projects:{},agents:{}}; return A[key]}
function tlProj(key,pid){var a=tlAcc(key); return a.projects[pid]||a.def}
function tlMember(key,s){var p=tlProj(key,s.project), own=tlAcc(key).agents[s.id]; return p==="deny"?"deny":(own||p)}
function tlTool(key){var k=key.split(":"), id=k[1];
 if(k[0]==="int"){var g=intGet(id); return g&&{key:key,kind:"int",id:id,name:g.name,icon:tlGlyph(g.glyph),sub:g.state==="off"?"연결 안 됨":g.account,state:g.state}}
 if(k[0]==="mcp"){var m=D.tools.mcp.filter(function(x){return x.id===id})[0]; return m&&{key:key,kind:"mcp",id:id,name:m.name,icon:I("server-stack"),sub:m.transport+" · 도구 "+m.tools+"개",state:m.state}}
 if(k[0]==="cli"){var c=D.tools.clis.filter(function(x){return x.id===id})[0]; return c&&{key:key,kind:"cli",id:id,name:c.name,icon:I("cmd"),sub:c.ver+" · "+c.path,state:c.state}}
 return null}
var TL_STATE={on:["사용 가능","green"],warn:["확인 필요","orange"],off:["꺼짐",""],busy:["연결 중","blue"]};

/* summary for a row: project counts per state and the number of member exceptions */
function tlSum(key){var c={allow:0,ask:0,deny:0}; D.projects.forEach(function(p){c[tlProj(key,p.id)]++}); var ex=Object.keys(tlAcc(key).agents).length;
 return '<span class="tl-sum" title="프로젝트 허용 '+c.allow+', 요청 시 확인 '+c.ask+', 차단 '+c.deny+(ex?", 팀원 예외 "+ex:"")+'">'+
  ["allow","ask","deny"].map(function(k){return '<span class="tl-c o-'+k+(c[k]?"":" zero")+'"><i></i>'+TL_SHORT[k]+' '+c[k]+'</span>'}).join("")+
  (ex?'<span class="tl-c ex">'+I("user")+'예외 '+ex+'</span>':"")+'</span>'}
function tlAccBtn(key,label){return '<button class="btn sm tl-accbtn" data-tlacc="'+key+'" aria-label="'+esc(label)+' 접근 권한">'+I("shield")+'<span class="lbl">접근 권한</span></button>'}

/* three-state control; cur is the explicit value ("" = follows the parent), eff the value it follows */
function tlTri(attr,val,cur,eff,label,dis){
 return '<div class="tl-tri'+(dis?" dis":"")+(cur?"":" inherit")+'" role="radiogroup" aria-label="'+esc(label)+'">'+["allow","ask","deny"].map(function(k){var on=cur===k, ghost=!cur&&eff===k;
  return '<button type="button" role="radio" aria-checked="'+on+'" class="tl-opt o-'+k+(on?" on":"")+(ghost?" ghost":"")+'" '+attr+'="'+esc(val)+'" data-v="'+k+'"'+(dis?" disabled":"")+' title="'+TL_ST[k][0]+(on?" (다시 누르면 기본값으로)":"")+'">'+TL_SHORT[k]+'</button>'}).join("")+'</div>'}

/* ---------- screen ---------- */
function tlRender(){
 var L=D.integrations.list, T=D.tools, onInt=L.filter(function(g){return g.state==="on"}).length, onMcp=T.mcp.filter(function(m){return m.state==="on"}).length;
 var f=[["all","전체"],["int","연동 "+L.length],["mcp","MCP 서버 "+T.mcp.length],["cli","서버 CLI "+T.clis.length]];
 var main='<main class="main tools">'+
 head("도구","ember-server에 등록한 연동, MCP 서버, CLI · 프로젝트와 팀원마다 쓸 수 있는지 정합니다",null,"",{primary:'<button class="btn primary" data-tladd title="도구 추가" aria-label="도구 추가">'+I("plus")+'<span class="lbl">도구 추가</span></button>'})+
 '<div class="card tl-server"><div class="card-b"><div class="tl-srv-l"><span class="tile">'+I("server")+'</span><div class="col" style="min-width:0;gap:2px"><b>ember-server</b><span class="small muted">mac-mini · 모든 에이전트 CLI가 여기서 실행됩니다</span></div></div>'+
  '<p class="small muted tl-srv-note">노드는 서버가 보낸 작업을 받아 수행합니다. 서버에 등록한 CLI는 노드의 터미널 환경에서 찾습니다(환경 변수 먼저, 그다음 PATH).</p>'+
  '<div class="tl-srv-k"><div><b>'+onInt+'<small>/'+L.length+'</small></b><span class="small muted">연동 연결</span></div><div><b>'+onMcp+'<small>/'+T.mcp.length+'</small></b><span class="small muted">MCP 사용 가능</span></div><div><b>'+T.clis.length+'</b><span class="small muted">서버 CLI</span></div></div></div></div>'+
 '<div class="seg scrollx tl-filter" role="tablist" aria-label="도구 종류">'+f.map(function(x){return '<button role="tab" aria-selected="'+(tlFilter===x[0])+'" data-tlf="'+x[0]+'" class="'+(tlFilter===x[0]?"on":"")+'">'+x[1]+'</button>'}).join("")+'</div>'+
 (tlFilter==="all"||tlFilter==="int"?tlIntSection():"")+
 (tlFilter==="all"||tlFilter==="mcp"?tlMcpSection():"")+
 (tlFilter==="all"||tlFilter==="cli"?tlCliSection():"")+
 '<p class="small muted tl-rule">'+I("shield")+'<span>팀원 설정이 프로젝트 설정보다 앞섭니다. 프로젝트가 차단이면 그 프로젝트의 팀원은 쓸 수 없습니다. 요청 시 확인은 쓸 때마다 승인 대기로 묻습니다.</span></p>'+
 '</main>';
 render(shell("tools",main));
}
function tlSecHead(id,title,sub,btn){return '<div class="hm-sh tl-sh" id="sec-'+id+'"><div><h2>'+title+'</h2><span class="muted small">'+sub+'</span></div><div class="spacer"></div>'+(btn||"")+'</div>'}

/* integrations: the cards that were on the Home dashboard, with an access summary in the foot */
function tlIntSection(){var L=D.integrations.list, on=L.filter(function(g){return g.state==="on"}).length, warn=L.filter(function(g){return g.state==="warn"}).length;
 var f=D.integrations.flow, fm=f[0][1];
 var flow='<div class="card int-flow"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("repeat")+'</span><b>연동이 만든 일</b><div class="spacer"></div><span class="muted small">이번 주</span></div><div class="int-fl">'+
  f.map(function(x,i){return '<div class="int-fs"><span class="muted small">'+x[0]+'</span><b class="hm-num">'+x[1]+'</b><div class="int-fb"><i style="height:'+(x[1]/fm*100)+'%"></i></div></div>'+(i<f.length-1?'<span class="int-arr">'+I("chev")+'</span>':"")}).join("")+
  '</div><p class="muted small" style="margin:10px 0 0">GitHub CI 실패 5건 중 4건을 루틴이 받아 3건을 팀원이 고쳤고, 2건이 PR로 머지됐습니다.</p>'+
  '<div class="hm-insight">'+tlGlyph("cal")+'<span>회의 중에는 승인이 <b>'+D.homeStats.wait.meet+'배</b> 늦습니다. 캘린더 연동이 회의 동안 알림을 묶습니다.</span></div></div></div>';
 return '<section class="int-sec tl-sec">'+tlSecHead("int","연동",on+'개 연결 · '+(warn?warn+'개 주의 · ':"")+'외부 신호가 팀원의 일이 됩니다','<button class="btn sm" data-intadd>'+I("plus")+'연동 추가</button>')+
 '<div class="int-grid">'+L.map(intCard).join("")+flow+'</div></section>'}
var INT_ST={on:["연결됨","green"],warn:["재인증 필요","orange"],off:["연결 안 됨",""],busy:["연결 중","blue"]};
function intCard(g){var st=INT_ST[g.state], big=g.id==="github";
 var body="";
 if(g.id==="github") body='<div class="int-ci" aria-label="최근 CI 14회">'+g.ci.map(function(v){return '<i class="'+(v?"ok":"fail")+'"></i>'}).join("")+'</div><div class="muted small" style="margin-top:4px">최근 CI 14회 · develop</div>';
 if(g.id==="calendar"){var tl=function(t){var p=t.split(":");return ((+p[0]+p[1]/60)-9)/10*100};
  body='<div class="int-day">'+g.day.map(function(d){return '<i class="'+d[2]+'" style="left:'+tl(d[0])+'%;width:'+(tl(d[1])-tl(d[0]))+'%" title="'+d[0]+'-'+d[1]+' '+d[3]+'"></i>'}).join("")+'<b class="int-now" style="left:'+tl("14:20")+'%"></b></div><div class="hm-heat-x muted"><span>9시</span><span>14시</span><span>19시</span></div>'}
 var stats=g.stats.length?'<div class="int-stats">'+g.stats.map(function(s){return '<div><b>'+s[1]+'</b><span class="muted small">'+s[0]+'</span></div>'}).join("")+'</div>':'<p class="muted small int-pitch">'+esc(g.scope)+'</p>';
 var rule=g.rules.filter(function(r){return r.on})[0]||g.rules[0];
 var act=g.state==="off"?'<button class="btn sm primary" data-intconnect="'+g.id+'">'+I("plus")+'연결</button>':g.state==="busy"?'<button class="btn sm" disabled><span class="spin"></span>연결 중</button>':g.state==="warn"?'<button class="btn sm primary" data-intconnect="'+g.id+'">'+I("refresh")+'다시 로그인</button>':'<button class="btn sm" data-intmanage="'+g.id+'">규칙</button>';
 return '<div class="card int-card int-'+g.id+(g.state==="off"?" is-off":"")+(big?" int-wide":"")+'"><div class="card-b"><div class="flex"><span class="int-logo">'+tlGlyph(g.glyph)+'</span><div class="col" style="min-width:0;gap:0"><b>'+g.name+'</b><span class="muted small int-acct">'+(g.state==="off"?"연결 안 됨":esc(g.account))+'</span>'+(g.state==="off"?"":'<span class="muted small">'+g.sync+' 동기화</span>')+'</div><div class="spacer"></div><span class="pill '+st[1]+'">'+st[0]+'</span></div>'+
 stats+body+
 '<div class="int-rule"><span class="int-when">'+esc(rule.when)+'</span>'+I("arrow")+'<span>'+esc(rule.then)+'</span></div>'+
 '<div class="tl-accrow">'+tlSum("int:"+g.id)+'</div>'+
 '<div class="flex int-foot">'+tlAccBtn("int:"+g.id,g.name)+'<div class="spacer"></div>'+act+'</div></div></div>'}

function tlMcpSection(){var M=D.tools.mcp;
 return '<section class="tl-sec">'+tlSecHead("mcp","MCP 서버","ember-server가 띄우고 팀원의 에이전트 CLI에 붙입니다",'<button class="btn sm" data-tladd="mcp">'+I("plus")+'MCP 서버 추가</button>')+
 '<div class="card tl-list">'+M.map(function(m){var st=TL_STATE[m.state], key="mcp:"+m.id;
  return '<div class="tl-row'+(m.state==="off"?" is-off":"")+'"><span class="tile">'+I("server-stack")+'</span>'+
   '<div class="col tl-main"><div class="flex"><b class="ellipsis">'+esc(m.name)+'</b><span class="pill '+st[1]+'">'+st[0]+'</span></div><span class="mono muted ellipsis">'+esc(m.transport)+' · '+esc(m.cmd)+'</span><span class="small muted">도구 '+m.tools+'개 · '+esc(m.note)+'</span></div>'+
   '<div class="tl-side">'+tlSum(key)+tlAccBtn(key,m.name)+'</div></div>'}).join("")+'</div></section>'}

function tlCliSection(){var C=D.tools.clis, comps=D.computers.filter(function(c){return c.status!=="unknown"});
 return '<section class="tl-sec">'+tlSecHead("cli","서버 CLI","ember-server에 설치하고 등록한 명령 · 노드는 터미널 환경에서 찾습니다",'<button class="btn sm" data-tladd="cli">'+I("plus")+'CLI 등록</button>')+
 '<div class="card tl-list">'+C.map(function(c){var st=TL_STATE[c.state], key="cli:"+c.id;
  return '<div class="tl-row"><span class="tile">'+I("cmd")+'</span>'+
   '<div class="col tl-main"><div class="flex"><b class="mono tl-cliname">'+esc(c.name)+'</b><span class="small muted">'+esc(c.ver)+'</span><span class="pill '+st[1]+'">'+st[0]+'</span></div><span class="mono muted ellipsis">'+esc(c.path)+'</span><span class="small muted">'+esc(c.auth)+'</span>'+
   '<div class="tl-nodes" aria-label="노드에서 찾는 방법">'+comps.map(function(n){var v=c.nodes[n.id]||"없음", ok=v!=="없음";
     return '<span class="tl-node'+(ok?" ok":"")+'" title="'+esc(n.id)+': '+(ok?(v==="PATH"?"PATH에서 찾음":"환경 변수 "+v+"로 찾음"):"이 노드의 터미널 환경에 없음")+'">'+I("pc")+esc(n.id)+'<em>'+esc(v)+'</em></span>'}).join("")+'</div></div>'+
   '<div class="tl-side">'+tlSum(key)+tlAccBtn(key,c.name)+'</div></div>'}).join("")+'</div></section>'}

/* ---------- access sheet ---------- */
function tlOpen(key,tab){var t=tlTool(key); if(!t) return; tlSel={key:key,tab:tab||(tlSel&&tlSel.key===key?tlSel.tab:"access"),proj:(tlSel&&tlSel.key===key)?tlSel.proj:"all"}; if(t.kind!=="int") tlSel.tab="access"; tlDraw();
 var h="#"+key+(tlSel.tab==="rules"?"/rules":""); if(location.hash!==h&&history.replaceState) history.replaceState(null,"",h)}
function tlDraw(){var t=tlTool(tlSel.key); if(!t) return; var key=t.key, a=tlAcc(key), m=$(".modal.tlmodal"), keep=m?m.querySelector(".sheet").scrollTop:0;
 var st=t.kind==="int"?INT_ST[t.state]:TL_STATE[t.state];
 var tabs=t.kind==="int"?'<div class="seg tl-tabs" role="tablist"><button role="tab" aria-selected="'+(tlSel.tab==="access")+'" class="'+(tlSel.tab==="access"?"on":"")+'" data-tltab="access">접근 권한</button><button role="tab" aria-selected="'+(tlSel.tab==="rules")+'" class="'+(tlSel.tab==="rules"?"on":"")+'" data-tltab="rules">규칙과 계정</button></div>':"";
 var body=tlSel.tab==="rules"?intRules(intGet(t.id)):tlAccessBody(t,a);
 sheet('<div class="sheet-h"><span class="'+(t.kind==="int"?"int-logo tl-slogo":"tile")+'">'+t.icon+'</span><div class="col tl-sh-t"><b class="sheet-t ellipsis">'+esc(t.name)+'</b><span class="small muted ellipsis">'+TL_KIND[t.kind][0]+' · '+esc(t.sub||"")+'</span></div><div class="sheet-acts"><span class="pill '+st[1]+' hide-sm">'+st[0]+'</span></div><button class="icon-btn sm" data-tlclose aria-label="닫기">'+I("close")+'</button></div>'+
  '<div class="tl-sbody">'+tabs+body+'</div>',"tlmodal");
 m=$(".modal.tlmodal"); if(m) m.querySelector(".sheet").scrollTop=keep}
function tlAccessBody(t,a){
 var intro=t.kind==="cli"?"ember-server에 등록한 명령입니다. 노드에서 작업할 때는 노드의 터미널 환경에서 찾습니다(환경 변수 먼저, 그다음 PATH).":
  t.kind==="mcp"?"ember-server가 이 MCP 서버를 띄우고, 허용된 팀원의 에이전트 CLI에만 붙입니다.":"ember-server가 이 연동의 토큰을 갖고 있고, 허용된 프로젝트와 팀원만 씁니다.";
 var projRows=D.projects.map(function(p){var own=a.projects[p.id]||"";
  return '<div class="tl-arow"><span class="tile sm-tile">'+I("folder")+'</span><div class="col tl-an"><b class="ellipsis">'+esc(p.id==="unsorted"?"기본 프로젝트":p.name)+'</b><span class="small muted ellipsis">'+(own?esc(p.repo):"기본값 따름: "+TL_ST[a.def][0])+'</span></div>'+tlTri("data-tlp",p.id,own,a.def,p.name+" 접근")+'</div>'}).join("");
 var members=D.sessions.filter(function(s){return tlSel.proj==="all"||s.project===tlSel.proj}).sort(function(x,y){return x.project<y.project?-1:x.project>y.project?1:0});
 var agentRows=members.map(function(s){var own=a.agents[s.id]||"", pst=tlProj(t.key,s.project), blocked=pst==="deny", pn=proj(s.project);
  var cap=blocked?"프로젝트에서 차단":own?esc(s.agent)+" · "+esc(s.computer):"프로젝트 따름: "+TL_ST[pst][0];
  return '<div class="tl-arow'+(blocked?" blocked":"")+'">'+bot(s,30)+'<div class="col tl-an"><b class="ellipsis">'+esc(mname(s))+'</b><span class="small muted ellipsis">'+cap+'</span></div>'+tlTri("data-tla",s.id,blocked?"":own,blocked?"deny":pst,mname(s)+" 접근",blocked)+'</div>'}).join("")||'<div class="empty">이 프로젝트에는 팀원이 없습니다</div>';
 var c={allow:0,ask:0,deny:0}; D.projects.forEach(function(p){c[tlProj(t.key,p.id)]++});
 var mc={allow:0,ask:0,deny:0}; D.sessions.forEach(function(s){mc[tlMember(t.key,s)]++});
 return '<p class="small muted tl-intro">'+intro+'</p>'+
 '<div class="tl-meta"><div><span class="muted small">프로젝트</span><b>허용 '+c.allow+' · 확인 '+c.ask+' · 차단 '+c.deny+'</b></div><div><span class="muted small">팀원 (실제 적용)</span><b>허용 '+mc.allow+' · 확인 '+mc.ask+' · 차단 '+mc.deny+'</b></div></div>'+
 '<div class="tl-arow tl-def"><span class="tile sm-tile">'+I("gear")+'</span><div class="col tl-an"><b>기본값</b><span class="small muted">따로 정하지 않은 프로젝트에 적용</span></div>'+tlTri("data-tldef","def",a.def,a.def,"기본값")+'</div>'+
 '<h3 class="tl-h3">프로젝트</h3>'+projRows+
 '<h3 class="tl-h3">팀원 (에이전트)</h3><p class="small muted tl-intro">팀원 설정이 프로젝트 설정보다 앞섭니다. 비워 두면 프로젝트를 따릅니다.</p>'+
 '<div class="seg scrollx tl-pf" role="tablist" aria-label="프로젝트로 거르기"><button data-tlpf="all" class="'+(tlSel.proj==="all"?"on":"")+'" aria-selected="'+(tlSel.proj==="all")+'" role="tab">전체 '+D.sessions.length+'</button>'+D.projects.map(function(p){var n=D.sessions.filter(function(s){return s.project===p.id}).length; return n?'<button data-tlpf="'+p.id+'" class="'+(tlSel.proj===p.id?"on":"")+'" aria-selected="'+(tlSel.proj===p.id)+'" role="tab">'+esc(p.short)+' '+n+'</button>':""}).join("")+'</div>'+
 agentRows}
function intRules(g){
 return '<div class="int-sheet"><div class="int-meta"><div><span class="muted small">계정</span><b>'+esc(g.account||"없음")+'</b></div><div><span class="muted small">마지막 동기화</span><b>'+(g.sync||"없음")+'</b></div><div><span class="muted small">권한</span><b>'+esc(g.scope)+'</b></div></div>'+
 '<h3>규칙</h3><p class="muted small">이 연동의 신호가 팀원에게 어떤 일을 맡기는지 정합니다. 규칙이 맡기는 팀원도 접근 권한을 따릅니다.</p>'+
 g.rules.map(function(r,i){return '<div class="int-rrow"><div class="col" style="min-width:0;gap:2px"><div class="int-rule" style="margin:0"><span class="int-when">'+esc(r.when)+'</span>'+I("arrow")+'<span>'+esc(r.then)+'</span></div><span class="muted small">'+esc(r.n)+'</span></div><button class="switch'+(r.on?" on":"")+'" role="switch" aria-checked="'+r.on+'" data-intrule="'+g.id+':'+i+'" aria-label="규칙 켜기/끄기"></button></div>'}).join("")+
 '<div class="flex" style="margin-top:16px"><button class="btn sm" data-intsync="'+g.id+'">'+I("refresh")+'지금 동기화</button><div class="spacer"></div>'+(g.state==="off"?"":'<button class="btn sm danger" data-intoff="'+g.id+'">연결 끊기</button>')+'</div></div>'}
function tlCloseSheet(){closeSheet("tlmodal"); tlSel=null; if(/^#(int|mcp|cli):/.test(location.hash)&&history.replaceState) history.replaceState(null,"",location.pathname+location.search)}

/* ---------- glyphs (were hmGlyph on Home) ---------- */
function tlGlyph(g){
 if(g==="cal"&&window.EMBER_ICONS&&EMBER_ICONS.calendar) return I("calendar");
 if(g==="mail"&&window.EMBER_ICONS&&EMBER_ICONS.envelope) return I("envelope");
 var p={cal:'<rect x="3.75" y="5.25" width="16.5" height="15" rx="3"/><path d="M3.75 9.75h16.5M8.25 3v4.5M15.75 3v4.5"/>',
  mail:'<rect x="3" y="5.25" width="18" height="13.5" rx="3"/><path d="m3.75 7.5 8.25 6 8.25-6"/>',
  line:'<path d="M4.5 13.5 10.5 19.5M4.5 9.75l9.75 9.75M6.75 6.75l10.5 10.5M10.5 4.5l9 9"/>'}[g];
 return p?'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">'+p+'</svg>':I(g)}
function intGet(id){return D.integrations.list.filter(function(x){return x.id===id})[0]}

/* ---------- hash, clicks ---------- */
function tlHash(){var h=decodeURIComponent(location.hash.slice(1)), m=/^(int|mcp|cli):([\w-]+)(\/rules)?$/.exec(h);
 if(m){tlOpen(m[1]+":"+m[2],m[3]?"rules":"access"); return}
 if(/^(int|mcp|cli|all)$/.test(h)&&tlFilter!==h){tlFilter=h; tlRender()}
 if(!m&&$(".modal.tlmodal")){closeSheet("tlmodal"); tlSel=null}}
/* render() replaces the body, so the open sheet is drawn again and keeps its scroll position */
function tlRefresh(){var m=$(".modal.tlmodal"), keep=m?m.querySelector(".sheet").scrollTop:0; tlRender(); if(tlSel){tlDraw(); var n=$(".modal.tlmodal"); if(n) n.querySelector(".sheet").scrollTop=keep}}
function tlSetState(map,k,v){if(map[k]===v) delete map[k]; else map[k]=v; save(); tlRefresh()}
Ember.screens.tools={
 render:function(){var h=location.hash.slice(1); if(/^(int|mcp|cli)$/.test(h)) tlFilter=h; tlRender(); tlHash()},
 onHash:tlHash,
 onModalClose:function(m){if(m&&m.classList.contains("tlmodal")){tlSel=null; if(/^#(int|mcp|cli):/.test(location.hash)&&history.replaceState) history.replaceState(null,"",location.pathname+location.search)}},
 click:function(e,t){var a;
  if((a=t.closest("[data-tlf]"))){tlFilter=a.dataset.tlf; tlRender(); if(history.replaceState) history.replaceState(null,"",tlFilter==="all"?location.pathname:"#"+tlFilter); return true}
  if((a=t.closest("[data-tlacc]"))){tlOpen(a.dataset.tlacc,"access"); return true}
  if((a=t.closest("[data-intmanage]"))){tlOpen("int:"+a.dataset.intmanage,"rules"); return true}
  if((a=t.closest("[data-tlclose]"))){tlCloseSheet(); return true}
  if((a=t.closest("[data-tltab]"))){tlSel.tab=a.dataset.tltab; tlOpen(tlSel.key,tlSel.tab); return true}
  if((a=t.closest("[data-tlpf]"))){tlSel.proj=a.dataset.tlpf; tlDraw(); return true}
  if((a=t.closest("[data-tldef]"))){var acc=tlAcc(tlSel.key); if(a.dataset.v!==acc.def){acc.def=a.dataset.v; save(); tlRefresh(); toast("기본값: "+TL_ST[acc.def][0])} return true}
  if((a=t.closest("[data-tlp]"))){var ap=tlAcc(tlSel.key).projects, pv=a.dataset.v; tlSetState(ap,a.dataset.tlp,pv); return true}
  if((a=t.closest("[data-tla]"))){if(a.disabled) return true; tlSetState(tlAcc(tlSel.key).agents,a.dataset.tla,a.dataset.v); return true}
  if((a=t.closest("[data-intconnect]"))){var g=intGet(a.dataset.intconnect); g.state="busy"; tlRefresh(); setTimeout(function(){g.state="on"; g.sync="방금"; if(!g.account) g.account="ncmnd13"; g.rules.forEach(function(r){r.on=true; if(r.n==="연결 전"||r.n==="재인증 필요") r.n="방금 켜짐"}); save(); toast(g.name+" 연결했습니다 (데모)"); if(document.body.dataset.screen==="tools") tlRefresh()},1200); return true}
  if((a=t.closest("[data-intrule]"))){var p=a.dataset.intrule.split(":"), r=intGet(p[0]).rules[+p[1]]; r.on=!r.on; save(); tlRefresh(); return true}
  if((a=t.closest("[data-intsync]"))){intGet(a.dataset.intsync).sync="방금"; save(); toast("동기화했습니다 (데모)"); tlRefresh(); return true}
  if((a=t.closest("[data-intoff]"))){var g2=intGet(a.dataset.intoff); g2.state="off"; g2.sync=""; save(); tlCloseSheet(); tlRender(); toast(g2.name+" 연결을 끊었습니다"); return true}
  if(t.closest("[data-intadd]")){toast("연동 목록에서 골라 연결합니다 (데모)"); return true}
  if((a=t.closest("[data-tladd]"))){var k=a.dataset.tladd; toast((k==="mcp"?"MCP 서버":k==="cli"?"ember-server에 설치된 CLI":"연동, MCP 서버 또는 CLI")+"를 등록합니다 (데모)"); return true}
 }};
