"use strict";
/* ---------- home (launcher + home merged) ---------- */
function home(){
 var run=D.sessions.filter(function(s){return s.status==="run"}), wait=D.sessions.filter(function(s){return s.status==="wait"}), online=D.computers.filter(function(c){return c.status==="online"}).length;
 var crew=run.concat(wait).slice(0,7).map(function(s){return '<a class="crew-m" href="conversation.html#'+s.id+'" title="'+esc(mname(s))+': '+esc(s.title)+'">'+bot(s,52)+'<span class="hm-crewn">'+esc(mname(s))+'</span></a>'}).join("");
 var projTiles=D.projects.map(function(p){var l=projSessions(p.id), n=l.length||1, seg=function(st){return l.filter(function(s){return s.status===st}).length/n*100};
  var a=seg("run"),b=a+seg("wait"),c=b+seg("unread")+seg("done"),d=100;
  return '<a class="ptile" href="conversation.html#p:'+p.id+'"><div class="ring mini-ring" style="background:radial-gradient(closest-side,var(--surface) 74%,transparent 75%),conic-gradient(var(--s-run) 0 '+a+'%,var(--s-wait) 0 '+b+'%,var(--c-sky) 0 '+c+'%,var(--c-red) 0 '+d+'%)"><b>'+l.length+'</b></div><div class="col" style="min-width:0"><b class="ellipsis">'+p.name+'</b><div class="botrow">'+l.slice(0,5).map(function(s){return bot(s,20)}).join("")+'</div></div></a>'}).join("");
 var sugg=[["spark","develop CI 실패 요약해줘"],["git","열린 PR 리뷰해줘"],["chart","이번 주 사용량 정리해줘"]];
 var main='<main class="main home">'+
 '<section class="hero"><div class="hero-bots" aria-hidden="true">'+bot("claude","done",40)+bot("codex","run",40)+bot("gemini","wait",40)+'</div><h1 class="greet">BREW, 무엇을 도와줄까요?</h1>'+
 '<div class="bigcomposer"><textarea id="quickText" placeholder="무엇이든 물어보세요. 새 대화는 고른 프로젝트의 팀원이 됩니다." aria-label="메시지"></textarea>'+
 '<div class="flex"><button class="icon-btn sm" data-act="attach" title="첨부" aria-label="첨부">'+I("plus")+'</button>'+
 '<label class="pickchip hm-sel qproj" title="프로젝트">'+I("folder")+'<select id="quickProj" aria-label="프로젝트">'+hmProjOrder().map(function(p){return '<option value="'+p.id+'">'+esc(hmProjName(p))+'</option>'}).join("")+'</select></label>'+
 '<span class="hm-pick" id="quickPick"></span><span class="hm-pick hm-mode" id="quickMode"></span>'+
 '<label class="pickchip hm-sel qcomp" title="컴퓨터">'+I("pc")+'<select id="quickComp" aria-label="컴퓨터"></select></label>'+
 '<div class="spacer"></div><button class="sendround" data-act="quick" title="보내기" aria-label="보내기">'+I("send")+'</button></div></div>'+
 '<div class="sugg">'+sugg.map(function(x){return '<button class="chip outline" data-suggest="'+esc(x[1])+'">'+I(x[0])+esc(x[1])+'</button>'}).join("")+
 ''+'</div>'+
 '<a class="downhint" href="#dash" aria-label="아래로"><span>오늘의 팀</span>'+I("chev")+'</a></section>'+
 '<section id="dash" class="hm-dash"><div class="hm-sh"><div><h2>오늘의 팀</h2><span class="muted small">10월 25일 토요일 · 연동 신호 포함</span></div></div>'+hmKpis()+
 '<div class="hm-grid">'+hmTrend()+hmStatus()+hmUsage()+
  '<div class="card b-crew"><div class="card-b"><a class="crew-link" href="conversation.html#p:'+lastProj()+'" aria-label="세션으로">'+I("arrow")+'</a><div class="flex"><span class="muted">지금 일하는 팀원</span><div class="spacer"></div><span class="pill blue">실행 '+run.length+'</span>'+(wait.length?'<span class="pill orange">승인 대기 '+wait.length+'</span>':"")+'</div><div class="crew">'+crew+'</div><div class="flex small muted" style="margin-top:8px">'+I("pc")+'컴퓨터 '+online+'/'+D.computers.length+' 온라인</div></div></div>'+
  hmEff()+hmSuccess()+hmWait()+hmLoad()+
  '<div class="card b-proj"><div class="card-b"><div class="flex" style="margin-bottom:12px"><b>프로젝트</b><div class="spacer"></div><a class="btn sm" href="project.html">모두 보기</a></div><div class="ptiles">'+projTiles+'</div></div></div>'+
 '</div>'+intSection()+
 '</section></main>';
 render(shell("home",main));
 var qt=$("#quickText"); qt.addEventListener("keydown",function(e){if(e.key==="Enter"&&!e.shiftKey&&!e.isComposing){e.preventDefault(); var v=qt.value.trim(); if(v) hmSend(v)}});
 $("#quickProj").addEventListener("change",function(){hmFillProj(this.value)}); hmFillProj($("#quickProj").value);
 qt.focus();
}
Ember.screens.home={render:home,onModalClose:function(){},click:function(e,t){var a;
 if((a=t.closest("[data-hmrange]"))){hmRange=+a.dataset.hmrange; home(); return true}
 if(intClick(t)) return true;
 if((a=t.closest("[data-suggest]"))){var qt=$("#quickText"); qt.value=a.dataset.suggest; qt.focus(); return true}
 if((a=t.closest("[data-act=quick]"))){var v=$("#quickText").value.trim(); if(v) hmSend(v); return true}
}};
/* ---------- v8 dashboard pieces (prefix hm*, int*) ---------- */
function hmGlyph(g){
 if(g==="cal"&&window.EMBER_ICONS&&EMBER_ICONS.calendar) return I("calendar");
 if(g==="mail"&&window.EMBER_ICONS&&EMBER_ICONS.envelope) return I("envelope");
 var p={cal:'<rect x="3.75" y="5.25" width="16.5" height="15" rx="3"/><path d="M3.75 9.75h16.5M8.25 3v4.5M15.75 3v4.5"/>',
  mail:'<rect x="3" y="5.25" width="18" height="13.5" rx="3"/><path d="m3.75 7.5 8.25 6 8.25-6"/>',
  line:'<path d="M4.5 13.5 10.5 19.5M4.5 9.75l9.75 9.75M6.75 6.75l10.5 10.5M10.5 4.5l9 9"/>'}[g];
 return p?'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">'+p+'</svg>':I(g)}
function hmDelta(v,good,suffix){var up=v>0; return '<span class="hm-delta '+((up===good)?"good":"bad")+'">'+(up?"+":"")+v+(suffix||"%")+'</span>'}
function hmKpis(){var h=D.homeStats, n=h.cost.length, c=h.cost[n-1], pc=h.cost[n-2], t=h.tok[n-1], ok=h.ok[n-1];
 var pend=D.sessions.filter(function(s){return s.status==="wait"}).length;
 var runN=D.sessions.filter(function(s){return s.status==="run"}).length, onN=D.computers.filter(function(x){return x.status==="online"}).length;
 var k=[["실행 중 팀원",runN+'<small>명</small>','<span class="hm-delta good">대기 '+pend+'</span>'],
  ["온라인 컴퓨터",onN+'<small>/'+D.computers.length+'</small>',""],
  ["성공률",ok+'<small>%</small>','<span class="hm-delta good">7일 '+Math.round(h.ok.slice(-7).reduce(function(a,b){return a+b})/7)+'%</span>'],
  ["승인 대기 중앙값",h.wait.median+'<small>분</small>','<span class="hm-delta good">-'+(h.wait.prev-h.wait.median).toFixed(1)+'분</span>'+(pend?'<span class="hm-delta bad">지금 '+pend+'건</span>':"")]];
 return '<div class="hm-kpis">'+k.map(function(x){return '<div class="hm-kpi"><span class="muted">'+x[0]+'</span><div class="hm-kv"><b>'+x[1]+'</b>'+x[2]+'</div></div>'}).join("")+'</div>'}
function hmStatus(){var S=D.sessions, c=function(st){return S.filter(function(s){return s.status===st}).length};
 var b=[["실행","run","var(--c-blue)",c("run")],["승인 대기","wait","var(--c-orange)",c("wait")],["새 결과","unread","var(--c-sky)",c("unread")],["완료","done","var(--c-grey)",c("done")]];
 var fail=c("fail");
 return '<div class="card hm-status"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("team")+'</span><b>팀원 상태</b><div class="spacer"></div><a class="hm-go" href="conversation.html#p:'+lastProj()+'" aria-label="세션으로">'+I("arrow")+'</a></div>'+
 '<div class="hm-alert"><span>승인 대기 '+c("wait")+'명</span>'+(fail?'<span class="hm-delta bad">실패 '+fail+'</span>':'<span class="hm-delta good">실패 0</span>')+'</div>'+
 '<div class="hm-blocks">'+b.map(function(x){return '<i style="flex:'+Math.max(x[3],.6)+';--bc:'+x[2]+'" title="'+x[0]+' '+x[3]+'"></i>'}).join("")+'</div>'+
 '<div class="hm-blegend">'+b.map(function(x){return '<div class="flex"><span class="dot" style="background:'+x[2]+'"></span><span class="muted small">'+x[0]+'</span><div class="spacer"></div><b>'+x[3]+'</b></div>'}).join("")+'</div></div></div>'}
function hmSuccess(){var h=D.homeStats, st=h.status, tot=st.ok+st.retry+st.fail, a=st.ok/tot*100, b=a+st.retry/tot*100;
 var spark=h.ok.slice(-7).map(function(v){return '<i style="height:'+((v-80)/20*100)+'%" title="'+v+'%"></i>'}).join("");
 return '<div class="card hm-succ"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("check")+'</span><b>작업 성공률</b></div>'+
 '<div class="hm-succ-b"><div class="hm-donut" style="--a:'+a+'%;--b:'+b+'%"><div><b>'+Math.round(a)+'%</b><span class="muted small">'+tot+'건</span></div></div>'+
 '<div class="col hm-succ-l"><div class="flex small"><span class="dot ok"></span>한 번에 성공<div class="spacer"></div><b>'+st.ok+'</b></div><div class="flex small"><span class="dot wait"></span>재시도 뒤 성공<div class="spacer"></div><b>'+st.retry+'</b></div><div class="flex small"><span class="dot fail"></span>실패<div class="spacer"></div><b>'+st.fail+'</b></div><div class="hm-spark" aria-label="최근 7일 성공률">'+spark+'</div><span class="muted small">CI 결과는 GitHub 연동에서 옵니다</span></div></div></div></div>'}
function hmWait(){var w=D.homeStats.wait, mx=Math.max.apply(null,w.buckets.map(function(b){return b[1]})), hm=Math.max.apply(null,w.hours);
 return '<div class="card hm-wait"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("clock")+'</span><b>승인 대기 시간</b></div>'+
 '<div class="hm-kv" style="margin:6px 0 14px"><b class="hm-num">'+w.median+'<small>분</small></b><span class="muted small">중앙값 · 지난주 '+w.prev+'분</span></div>'+
 '<div class="hm-hist">'+w.buckets.map(function(b,i){return '<div class="hm-hcol"><span class="small">'+b[1]+'</span><i class="'+(i>2?"slow":"")+'" style="height:'+(b[1]/mx*100)+'%"></i><span class="muted">'+b[0]+'</span></div>'}).join("")+'</div>'+
 '<div class="hm-heat" aria-label="시간대별 승인 요청">'+w.hours.map(function(v,i){return '<i style="opacity:'+(v?(.18+v/hm*.82).toFixed(2):.06)+'" title="'+i+'시 '+v+'건"></i>'}).join("")+'</div><div class="hm-heat-x muted"><span>0시</span><span>12시</span><span>23시</span></div>'+
 '<div class="hm-insight">'+hmGlyph("cal")+'<span>회의 중에는 승인이 <b>'+w.meet+'배</b> 늦습니다. 캘린더 연동이 회의 동안 알림을 묶습니다.</span></div></div></div>'}
function hmEff(){var e=D.homeStats.eff, mx=Math.max.apply(null,e.map(function(x){return x.tasks}));
 return '<div class="card hm-eff"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("zap")+'</span><b>팀원별 효율</b><div class="spacer"></div><span class="muted small">이번 주 · 완료 작업</span></div><div class="hm-effg">'+
 e.map(function(x){var s=sess(x.id); if(!s) return ""; return '<a class="hm-er" href="conversation.html#'+s.id+'">'+bot(s,30)+'<div class="col" style="min-width:0;gap:4px"><div class="flex small"><b class="ellipsis">'+esc(mname(s))+'</b><div class="spacer"></div><span class="muted">$'+x.cost.toFixed(2)+'/작업</span></div><div class="hm-ebar"><i style="width:'+(x.tasks/mx*100)+'%;background:var(--v-'+vendorOf(s.agent)+')"></i><span>'+x.tasks+'</span></div></div><div class="mini-ring" style="background:radial-gradient(closest-side,var(--surface) 72%,transparent 73%),conic-gradient(var(--s-ok) '+x.ok+'%,var(--track) 0)" title="성공률 '+x.ok+'%"><b class="hm-ringnum">'+x.ok+'</b></div></a>'}).join("")+'</div></div></div>'}
function hmLoad(){var L=D.homeStats.load;
 var meter=function(v,lab){return '<div class="hm-meter'+(v>85?" hot":"")+'"><div class="hm-mt"><i style="height:'+v+'%"></i></div><span class="small"><b>'+v+'</b>%</span><span class="muted">'+lab+'</span></div>'};
 return '<div class="card hm-load"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("cpu")+'</span><b>컴퓨터 부하</b><div class="spacer"></div><a class="hm-go" href="project.html" aria-label="프로젝트 화면의 컴퓨터로">'+I("arrow")+'</a></div><div class="hm-loadg">'+
 L.map(function(c){return '<div class="hm-lc"><div class="flex small"><span class="dot ok"></span><b class="ellipsis">'+c.id+'</b></div><div class="flex hm-meters">'+meter(c.cpu,"CPU")+meter(c.mem,"메모리")+'</div><span class="muted small">팀원 '+c.run+'명 실행</span></div>'}).join("")+'</div></div></div>'}

/* ---------- integrations ---------- */
var INT_ST={on:["연결됨","green"],warn:["재인증 필요","orange"],off:["연결 안 됨",""],busy:["연결 중","blue"]};
function intCard(g){var st=INT_ST[g.state], big=g.id==="github";
 var body="";
 if(g.id==="github") body='<div class="int-ci" aria-label="최근 CI 14회">'+g.ci.map(function(v){return '<i class="'+(v?"ok":"fail")+'"></i>'}).join("")+'</div><div class="muted small" style="margin-top:4px">최근 CI 14회 · develop</div>';
 if(g.id==="calendar"){var tl=function(t){var p=t.split(":");return ((+p[0]+p[1]/60)-9)/10*100};
  body='<div class="int-day">'+g.day.map(function(d){return '<i class="'+d[2]+'" style="left:'+tl(d[0])+'%;width:'+(tl(d[1])-tl(d[0]))+'%" title="'+d[0]+'-'+d[1]+' '+d[3]+'"></i>'}).join("")+'<b class="int-now" style="left:'+tl("14:20")+'%"></b></div><div class="hm-heat-x muted"><span>9시</span><span>14시</span><span>19시</span></div>'}
 var stats=g.stats.length?'<div class="int-stats">'+g.stats.map(function(s){return '<div><b>'+s[1]+'</b><span class="muted small">'+s[0]+'</span></div>'}).join("")+'</div>':'<p class="muted small int-pitch">'+esc(g.scope)+'</p>';
 var rule=g.rules.filter(function(r){return r.on})[0]||g.rules[0];
 var act=g.state==="off"?'<button class="btn sm primary" data-intconnect="'+g.id+'">'+I("plus")+'연결</button>':g.state==="busy"?'<button class="btn sm" disabled><span class="spin"></span>연결 중</button>':g.state==="warn"?'<button class="btn sm primary" data-intconnect="'+g.id+'">'+I("refresh")+'다시 로그인</button>':'<button class="btn sm" data-intmanage="'+g.id+'">관리</button>';
 return '<div class="card int-card int-'+g.id+(g.state==="off"?" is-off":"")+(big?" int-wide":"")+'"><div class="card-b"><div class="flex"><span class="int-logo">'+hmGlyph(g.glyph)+'</span><div class="col" style="min-width:0;gap:0"><b>'+g.name+'</b><span class="muted small int-acct">'+(g.state==="off"?"연결 안 됨":esc(g.account))+'</span>'+(g.state==="off"?"":'<span class="muted small">'+g.sync+' 동기화</span>')+'</div><div class="spacer"></div><span class="pill '+st[1]+'">'+st[0]+'</span></div>'+
 stats+body+
 '<div class="int-rule"><span class="int-when">'+esc(rule.when)+'</span>'+I("arrow")+'<span>'+esc(rule.then)+'</span></div>'+
 '<div class="flex int-foot"><span class="muted small">지표</span>'+g.feeds.map(function(f){return '<span class="pill">'+f+'</span>'}).join("")+'<div class="spacer"></div>'+act+'</div></div></div>'}
function intSection(){var L=D.integrations.list, on=L.filter(function(g){return g.state==="on"}).length, warn=L.filter(function(g){return g.state==="warn"}).length;
 var f=D.integrations.flow, fm=f[0][1];
 var flow='<div class="card int-flow"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("repeat")+'</span><b>연동이 만든 일</b><div class="spacer"></div><span class="muted small">이번 주</span></div><div class="int-fl">'+
  f.map(function(x,i){return '<div class="int-fs"><span class="muted small">'+x[0]+'</span><b class="hm-num">'+x[1]+'</b><div class="int-fb"><i style="height:'+(x[1]/fm*100)+'%"></i></div></div>'+(i<f.length-1?'<span class="int-arr">'+I("chev")+'</span>':"")}).join("")+
  '</div><p class="muted small" style="margin:10px 0 0">GitHub CI 실패 5건 중 4건을 루틴이 받아 3건을 팀원이 고쳤고, 2건이 PR로 머지됐습니다.</p></div></div>';
 return '<section class="int-sec" id="integrations"><div class="hm-sh"><div><h2>연동</h2><span class="muted small">'+on+'개 연결 · '+(warn?warn+'개 주의 · ':"")+'외부 신호가 팀원의 일과 대시보드 지표가 됩니다</span></div><div class="spacer"></div><button class="btn sm" data-intadd>'+I("plus")+'연동 추가</button></div>'+
 '<div class="int-grid">'+L.map(intCard).join("")+flow+'</div></section>'}
function intManage(id){var g=D.integrations.list.filter(function(x){return x.id===id})[0]; if(!g) return;
 sheet('<div class="topbar"><span class="int-logo">'+hmGlyph(g.glyph)+'</span><b class="crumb-t">'+g.name+'</b><span class="pill '+INT_ST[g.state][1]+'">'+INT_ST[g.state][0]+'</span><div class="spacer"></div><button class="icon-btn sm" data-intclose aria-label="닫기">'+I("close")+'</button></div>'+
 '<div class="card-b int-sheet"><div class="int-meta"><div><span class="muted small">계정</span><b>'+esc(g.account||"없음")+'</b></div><div><span class="muted small">마지막 동기화</span><b>'+(g.sync||"없음")+'</b></div><div><span class="muted small">권한</span><b>'+esc(g.scope)+'</b></div></div>'+
 '<h3>규칙</h3><p class="muted small">이 연동의 신호가 팀원에게 어떤 일을 맡기는지 정합니다.</p>'+
 g.rules.map(function(r,i){return '<div class="int-rrow"><div class="col" style="min-width:0;gap:2px"><div class="int-rule" style="margin:0"><span class="int-when">'+esc(r.when)+'</span>'+I("arrow")+'<span>'+esc(r.then)+'</span></div><span class="muted small">'+esc(r.n)+'</span></div><button class="switch'+(r.on?" on":"")+'" role="switch" aria-checked="'+r.on+'" data-intrule="'+g.id+':'+i+'" aria-label="규칙 켜기/끄기"></button></div>'}).join("")+
 '<div class="flex" style="margin-top:16px"><button class="btn sm" data-intsync="'+g.id+'">'+I("refresh")+'지금 동기화</button><div class="spacer"></div><button class="btn sm danger" data-intoff="'+g.id+'">연결 끊기</button></div></div>',"intmodal")}
function intClick(t){var a;
 if((a=t.closest("[data-intconnect]"))){var g=intGet(a.dataset.intconnect); g.state="busy"; home(); setTimeout(function(){g.state="on"; g.sync="방금"; if(!g.account) g.account="ncmnd13"; g.rules.forEach(function(r){r.on=true; if(r.n==="연결 전"||r.n==="재인증 필요") r.n="방금 켜짐"}); save(); toast(g.name+" 연결했습니다 (데모)"); if(document.body.dataset.screen==="home"&&!document.querySelector(".modal.intmodal")) home()},1200); return true}
 if((a=t.closest("[data-intmanage]"))){intManage(a.dataset.intmanage); return true}
 if((a=t.closest("[data-intclose]"))){closeSheet("intmodal"); return true}
 if((a=t.closest("[data-intrule]"))){var p=a.dataset.intrule.split(":"), r=intGet(p[0]).rules[+p[1]]; r.on=!r.on; save(); home(); intManage(p[0]); return true}
 if((a=t.closest("[data-intsync]"))){intGet(a.dataset.intsync).sync="방금"; save(); toast("동기화했습니다 (데모)"); intManage(a.dataset.intsync); return true}
 if((a=t.closest("[data-intoff]"))){var g2=intGet(a.dataset.intoff); g2.state="off"; g2.sync=""; save(); closeSheet("intmodal"); home(); toast(g2.name+" 연결을 끊었습니다"); return true}
 if(t.closest("[data-intadd]")){toast("연동 목록은 설정에서 고릅니다 (데모)"); return true}
}
function intGet(id){return D.integrations.list.filter(function(x){return x.id===id})[0]}

/* ---------- composer project chip ---------- */
function hmProjName(p){return p.id==="unsorted"?"기본 프로젝트":p.name}
function hmProjOrder(){var last=function(p){return Math.max.apply(null,[0].concat(projSessions(p.id).map(function(s){return s.t||0})))};
 var rest=D.projects.filter(function(p){return p.id!=="unsorted"}).sort(function(a,b){return last(b)-last(a)});
 var u=D.projects.filter(function(p){return p.id==="unsorted"});
 return u.concat(rest)}
var hmPickV=null, hmModeV=null;
function hmFillProj(pid){var p=proj(pid)||{computers:[]}, l=projSessions(pid), cnt={};
 l.forEach(function(s){cnt[s.agent]=(cnt[s.agent]||0)+1});
 var d=p.defaults||{}, top=["Claude Code","Codex","Gemini CLI"].sort(function(a,b){return (cnt[b]||0)-(cnt[a]||0)})[0];
 var opts={harness:d.agent||p.agent||top, model:d.model||"", account:d.account||"ncmnd · Max", name:"quick", onChange:function(v){hmPickV=v}};
 var box=$("#quickPick"); box.innerHTML=Ember.modelPicker.html(opts); hmPickV=Ember.modelPicker.value(box);
 var mb=$("#quickMode"); if(Ember.modePicker){mb.innerHTML=Ember.modePicker.html({harness:hmPickV.harness, mode:d.mode||"", name:"quick", onChange:function(v){hmModeV=v}}); hmModeV=Ember.modePicker.value(mb)}
 var cs=(p.computers||[]).map(function(id){return D.computers.filter(function(c){return c.id===id})[0]}).filter(function(c){return c&&c.status==="online"});
 if(!cs.length) cs=D.computers.filter(function(c){return c.status==="online"});
 $("#quickComp").innerHTML=cs.map(function(c){return '<option>'+c.id+'</option>'}).join("")}
/* send with the picked harness, model and effort (core newSession has no model argument) */
function hmSend(text){var v=Ember.modelPicker.value($("#quickPick")), md=Ember.modePicker?Ember.modePicker.value($("#quickMode")):hmModeV, pid=$("#quickProj").value, comp=$("#quickComp").value;
 var eff=v.effort?" · "+v.effort.charAt(0).toUpperCase()+v.effort.slice(1):"";
 var s={id:"n"+Date.now(),title:text.slice(0,40),project:pid,agent:v.harness,model:v.modelName+eff,computer:comp||(proj(pid).computers||[])[0]||"mac-mini",account:v.account||"ncmnd · Max",mode:md?md.mode:"ask",modeName:md?md.modeName:"",status:"run",t:maxT(),msgs:[{k:"user",text:text},{k:"working",text:"작업 중"}]};
 D.sessions.push(s); save(); location.href="conversation.html#"+s.id}

var hmRange=14;
function hmTrend(){var h=D.homeStats, n=h.cost.length, s=n-hmRange, cost=h.cost.slice(s), tok=h.tok.slice(s), days=h.days.slice(s), m=cost.length;
 var cmax=Math.max.apply(null,cost)*1.15, tmax=Math.max.apply(null,tok)*1.15, W=700, H=200;
 var x=function(i){return (i+.5)*W/m}, y=function(v){return H-v/cmax*H};
 var pts=cost.map(function(v,i){return x(i).toFixed(1)+","+y(v).toFixed(1)}).join(" ");
 var area="M"+x(0)+","+H+" L"+pts.split(" ").join(" L")+" L"+x(m-1)+","+H+" Z";
 var bars=tok.map(function(v,i){return '<i class="'+(i===m-1?"now":"")+'" style="height:'+(v/tmax*100).toFixed(1)+'%" title="'+days[i]+' '+v+'M"></i>'}).join("");
 var tc=cost.reduce(function(a,b){return a+b}), tt=tok.reduce(function(a,b){return a+b});
 return '<div class="card hm-trend"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("chart")+'</span><b>비용과 토큰 추이</b><div class="spacer"></div><span class="legend hide-sm"><span class="flex"><span class="dot" style="background:var(--ink)"></span>비용</span><span class="flex"><span class="hm-hatch-dot"></span>토큰</span></span><div class="seg hm-seg" role="tablist"><button data-hmrange="7" class="'+(hmRange===7?"on":"")+'">7일</button><button data-hmrange="14" class="'+(hmRange===14?"on":"")+'">14일</button></div></div>'+
 '<div class="hm-sub"><div><span class="muted small">기간 비용</span><b>$'+tc.toFixed(1)+'</b></div><div><span class="muted small">토큰</span><b>'+tt.toFixed(1)+'M</b></div><div><span class="muted small">작업당 비용</span><b>$0.54</b></div><div class="hide-sm"><span class="muted small">캐시 적중</span><b>61%</b></div></div>'+
 '<div class="hm-plot"><div class="hm-tbars">'+bars+'</div><svg viewBox="0 0 '+W+' '+H+'" preserveAspectRatio="none" aria-label="일별 비용 선"><path d="'+area+'" class="hm-area"/><polyline points="'+pts+'" class="hm-line"/></svg>'+
 '<span class="hm-pt" style="left:'+(x(m-1)/W*100)+'%;top:'+(y(cost[m-1])/H*100)+'%"></span>'+
 '<div class="hm-tip" style="right:'+(100-x(m-1)/W*100+3).toFixed(1)+'%;top:4%"><span class="muted small">'+days[m-1]+' 오늘</span><div class="flex small"><span class="dot" style="background:var(--ink)"></span>비용<div class="spacer"></div><b>$'+cost[m-1]+'</b></div><div class="flex small"><span class="hm-hatch-dot"></span>토큰<div class="spacer"></div><b>'+tok[m-1]+'M</b></div></div></div>'+
 '<div class="hm-xl">'+days.map(function(d,i){return '<span'+(i===m-1?' class="today"':"")+'>'+((m>8&&i%2)?"":d)+'</span>'}).join("")+'</div></div></div>'}
/* weekday stacked bars by harness + account rings (detail lives in agents.html#usage) */
function hmUsage(){var w=D.week, tot=w.c.map(function(c,i){return c+w.x[i]+w.g[i]}), mx=Math.max.apply(null,tot), avg=tot.reduce(function(a,b){return a+b})/7;
 var bars=w.labels.map(function(l,i){return '<div class="bcol"><div class="bar" style="height:'+Math.round(tot[i]/mx*100)+'%" title="'+l+' '+tot[i].toFixed(1)+'M"><i class="c" style="flex:'+w.c[i]+'"></i><i class="x" style="flex:'+w.x[i]+'"></i><i class="g" style="flex:'+w.g[i]+'"></i></div><span'+(i==6?' class="today"':"")+'>'+l+'</span></div>'}).join("");
 var rings=D.accounts.map(function(a){var p=a.left==null?0:a.left;
  return '<div class="hm-ar"><div class="mini-ring lg" style="background:radial-gradient(closest-side,var(--surface) 74%,transparent 75%),conic-gradient(var(--v-'+a.vendor+') '+p+'%,var(--track) 0)"><b class="hm-ringnum">'+(a.left==null?"?":a.left)+'</b></div><div class="col" style="min-width:0;gap:0"><span class="hm-ar-n ellipsis">'+a.name+' '+a.tier+'</span><span class="muted hm-ar-m ellipsis">'+esc(a.id)+' · '+(a.left==null?"알 수 없음":esc(a.reset))+'</span>'+(a.five!=null?'<div class="track hm-five" title="5시간 창 '+a.five+'%"><i style="width:'+a.five+'%;background:var(--v-'+a.vendor+')"></i></div>':"")+'</div></div>'}).join("");
 return '<div class="card hm-usage"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("chart")+'</span><b>하네스별 사용량</b><div class="spacer"></div><a class="btn sm" href="agents.html#usage">자세히</a></div>'+
 '<div class="usage-top"><div class="spacer"><span class="muted">오늘</span><div class="hm-kv"><b>'+tot[6].toFixed(1)+'<small>M</small></b></div><span class="muted small">토큰 · 7일 평균 '+avg.toFixed(1)+'M</span><div class="legend" style="margin-top:12px"><span class="flex"><span class="dot claude"></span>Claude '+w.c[6]+'M</span><span class="flex"><span class="dot codex"></span>Codex '+w.x[6]+'M</span><span class="flex"><span class="dot gemini"></span>Gemini '+w.g[6]+'M</span></div></div><div class="barwrap"><div class="bars2">'+bars+'</div></div></div>'+
 '<div class="hm-rings">'+rings+'</div></div></div>'}
