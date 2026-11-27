"use strict";
/* ---------- home (launcher + home merged) ----------
   Owner 2026-11-27 (INTENT D58): the dashboard is no longer below the composer. It sits above the page and pours out when the
   main page is pulled down from the top (wheel or touch overscroll; a partial pull shows a hint, a full pull opens it) or when
   the DarkPyonix mark in the app bar is pressed. It closes with the close button, Escape, or pushing past its bottom edge.
   Integrations moved to the Tools screen (tools.html). For captures: home.html#dash opens it at once, home.html#pull shows a
   partial pull; Ember.dash.open() / close() / pull(px) do the same from script. */
function home(){
 var run=D.sessions.filter(function(s){return s.status==="run"}), wait=D.sessions.filter(function(s){return s.status==="wait"}), online=D.computers.filter(function(c){return c.status==="online"}).length;
 var crew=run.concat(wait).slice(0,7).map(function(s){return '<a class="crew-m" href="conversation.html#'+s.id+'" title="'+esc(mname(s))+': '+esc(s.title)+'">'+bot(s,52)+'<span class="hm-crewn">'+esc(mname(s))+'</span></a>'}).join("");
 var projTiles=D.projects.map(function(p){var l=projSessions(p.id), n=l.length||1, seg=function(st){return l.filter(function(s){return s.status===st}).length/n*100};
  var a=seg("run"),b=a+seg("wait"),c=b+seg("unread")+seg("done"),d=100;
  return '<a class="ptile" href="conversation.html#p:'+p.id+'"><div class="ring mini-ring" style="background:radial-gradient(closest-side,var(--surface) 74%,transparent 75%),conic-gradient(var(--s-run) 0 '+a+'%,var(--s-wait) 0 '+b+'%,var(--c-sky) 0 '+c+'%,var(--c-red) 0 '+d+'%)"><b>'+l.length+'</b></div><div class="col" style="min-width:0"><b class="ellipsis">'+p.name+'</b><div class="botrow">'+l.slice(0,5).map(function(s){return bot(s,20)}).join("")+'</div></div></a>'}).join("");
 var sugg=[["spark","develop CI 실패 요약해줘"],["git","열린 PR 리뷰해줘"],["chart","이번 주 사용량 정리해줘"]];
 var L0=$("#dash"), wasOpen=dash.open, keepTop=L0?L0.scrollTop:0;
 var main='<main class="main home" id="homeMain">'+
 '<button class="dash-grip" data-act="dash" title="아래로 당기거나 DarkPyonix 로고를 눌러도 열립니다" aria-label="대시보드 열기: 오늘의 팀" aria-controls="dash" aria-expanded="false"><span class="dash-grip-l">오늘의 팀</span>'+I("chev")+'</button>'+
 '<div class="dash-hint" aria-hidden="true"><span class="dash-ring"></span><span class="dash-hint-t">더 당기면 대시보드가 열립니다</span></div>'+
 '<section class="hero"><div class="hero-bots" aria-hidden="true">'+bot("claude","done",40)+bot("codex","run",40)+bot("gemini","wait",40)+'</div><h1 class="greet">BREW, 무엇을 도와줄까요?</h1>'+
 '<div class="bigcomposer"><textarea id="quickText" placeholder="무엇이든 물어보세요. 새 대화는 고른 프로젝트의 팀원이 됩니다." aria-label="메시지"></textarea>'+
 '<div class="flex"><button class="icon-btn sm" data-act="attach" title="첨부" aria-label="첨부">'+I("plus")+'</button>'+
 '<label class="pickchip hm-sel qproj" title="프로젝트">'+I("folder")+'<select id="quickProj" aria-label="프로젝트">'+hmProjOrder().map(function(p){return '<option value="'+p.id+'">'+esc(hmProjName(p))+'</option>'}).join("")+'</select></label>'+
 '<span class="hm-pick" id="quickPick"></span><span class="hm-pick hm-mode" id="quickMode"></span>'+
 '<label class="pickchip hm-sel qcomp" title="컴퓨터">'+I("pc")+'<select id="quickComp" aria-label="컴퓨터"></select></label>'+
 '<div class="spacer"></div><button class="sendround" data-act="quick" title="보내기" aria-label="보내기">'+I("send")+'</button></div></div>'+
 '<div class="sugg">'+sugg.map(function(x){return '<button class="chip outline" data-suggest="'+esc(x[1])+'">'+I(x[0])+esc(x[1])+'</button>'}).join("")+'</div>'+
 '</section></main>'+
 /* phone has no sidebar brand: the mark sits at the top left of Home, opposite the menu button */
 '<button class="hm-plogo" data-act="dash" title="대시보드 열기" aria-label="대시보드 열기">'+brandMark(24)+'</button>'+
 '<div class="dash-layer" id="dash" role="region" aria-label="대시보드: 오늘의 팀" aria-hidden="true" tabindex="-1"><section class="hm-dash">'+
 '<div class="hm-sh"><div><h2>오늘의 팀</h2><span class="muted small">10월 25일 토요일</span></div><div class="spacer"></div>'+
  '<a class="btn sm hide-sm" href="tools.html">'+I("wrench")+'연동은 도구에서</a><button class="icon-btn" data-dashclose title="닫기 (Esc)" aria-label="대시보드 닫기">'+I("close")+'</button></div>'+hmKpis()+
 '<div class="hm-grid">'+hmTrend()+hmStatus()+hmUsage()+
  '<div class="card b-crew"><div class="card-b"><a class="crew-link" href="conversation.html#p:'+lastProj()+'" aria-label="세션으로">'+I("arrow")+'</a><div class="flex"><span class="muted">지금 일하는 팀원</span><div class="spacer"></div><span class="pill blue">실행 '+run.length+'</span>'+(wait.length?'<span class="pill orange">승인 대기 '+wait.length+'</span>':"")+'</div><div class="crew">'+crew+'</div><div class="flex small muted" style="margin-top:8px">'+I("pc")+'컴퓨터 '+online+'/'+D.computers.length+' 온라인</div></div></div>'+
  hmEff()+hmSuccess()+hmWait()+hmLoad()+
  '<div class="card b-proj"><div class="card-b"><div class="flex" style="margin-bottom:12px"><b>프로젝트</b><div class="spacer"></div><a class="btn sm" href="project.html">모두 보기</a></div><div class="ptiles">'+projTiles+'</div></div></div>'+
 '</div>'+
 '<button class="dash-foot" data-dashclose aria-label="대시보드 닫기">'+I("chev")+'<span class="dash-foot-t">끝에서 더 밀어 올리면 닫힙니다</span></button>'+
 '</section></div>';
 render(shell("home",main));
 /* pour order: header, KPIs, then cards in reading order */
 [].forEach.call(document.querySelectorAll(".dash-layer .hm-sh,.dash-layer .hm-kpi,.dash-layer .hm-grid>.card,.dash-layer .dash-foot"),function(el,i){el.style.setProperty("--i",Math.min(i,14))});
 dashBind();
 var qt=$("#quickText"); qt.addEventListener("keydown",function(e){if(e.key==="Enter"&&!e.shiftKey&&!e.isComposing){e.preventDefault(); var v=qt.value.trim(); if(v) hmSend(v)}});
 $("#quickProj").addEventListener("change",function(){hmFillProj(this.value)}); hmFillProj($("#quickProj").value);
 if(wasOpen){dash.open=false; dashOpen({instant:true,keepFocus:true}); $("#dash").scrollTop=keepTop}
 else if(location.hash==="#dash") dashOpen({instant:true});
 else if(location.hash==="#pull") dashSetPull(DASH_T*0.62);
 else qt.focus();
}
Ember.screens.home={render:home,onModalClose:function(){},
 onHash:function(){if(location.hash==="#dash") dashOpen(); else if(dash.open) dashClose(); else if(location.hash==="#pull") dashSetPull(DASH_T*0.62); else dashSpring()},
 click:function(e,t){var a;
 if((a=t.closest("[data-hmrange]"))){hmRange=+a.dataset.hmrange; home(); return true}
 if(t.closest("[data-dashclose]")){dashClose(); return true}
 if((a=t.closest("[data-suggest]"))){var qt=$("#quickText"); qt.value=a.dataset.suggest; qt.focus(); return true}
 if((a=t.closest("[data-act=quick]"))){var v=$("#quickText").value.trim(); if(v) hmSend(v); return true}
}};

/* ---------- dashboard reveal: pull down from the top (wheel, touch) or press the mark ----------
   The pull is resisted (half the finger or wheel distance) and drawn as --pull on the main page (the hero follows it)
   and --pp (0..1) on the hint ring. Reaching DASH_T opens the layer at once ("pours out"); stopping short springs back.
   Momentum guard: a gesture counts only when the scroller has been still for SETTLE ms, so a fling that reaches the
   top or the end does not open or close the layer by itself. */
var DASH_T=140, SETTLE=180, dash={open:false,pull:0,push:0,tPull:0,tPush:0,mScroll:0,lScroll:0};
function dashSetPull(px){var m=$("#homeMain"); if(!m) return; dash.pull=Math.max(0,Math.min(px,DASH_T*1.2)); var p=Math.min(1,dash.pull/DASH_T);
 m.style.setProperty("--pull",(dash.pull*0.5).toFixed(1)); m.style.setProperty("--pp",p.toFixed(3));
 m.classList.toggle("pulling",dash.pull>0); m.classList.toggle("ready",p>=1);
 var t=m.querySelector(".dash-hint-t"); if(t) t.textContent=p>=1?"대시보드를 엽니다":"더 당기면 대시보드가 열립니다"}
function dashSpring(){var m=$("#homeMain"); if(!m) return; clearTimeout(dash.tPull); if(!dash.pull){m.classList.remove("pulling","ready"); return}
 m.classList.add("springing"); dashSetPull(0); setTimeout(function(){m.classList.remove("springing")},320)}
function dashSetPush(px){var L=$("#dash"); if(!L) return; dash.push=Math.max(0,Math.min(px,DASH_T*1.2)); var p=Math.min(1,dash.push/DASH_T);
 L.style.setProperty("--push",(dash.push*0.5).toFixed(1)); L.style.setProperty("--qp",p.toFixed(3)); L.classList.toggle("pushing",dash.push>0);
 var t=L.querySelector(".dash-foot-t"); if(t) t.textContent=p>=1?"대시보드를 닫습니다":(dash.push?"조금 더 밀어 올리면 닫힙니다":"끝에서 더 밀어 올리면 닫힙니다")}
function dashUnpush(){var L=$("#dash"); if(!L) return; clearTimeout(dash.tPush); L.classList.add("springing"); dashSetPush(0); setTimeout(function(){L.classList.remove("springing")},320)}
function dashSync(){var on=dash.open; [].forEach.call(document.querySelectorAll('[data-act="dash"]'),function(b){b.setAttribute("aria-expanded",String(on)); b.setAttribute("aria-label",on?"대시보드 닫기":"대시보드 열기")})}
function dashOpen(o){o=o||{}; var L=$("#dash"); if(!L||dash.open) return; dash.open=true; dashSpring();
 L.classList.toggle("instant",!!o.instant); L.classList.add("open"); L.setAttribute("aria-hidden","false"); L.scrollTop=0; document.body.classList.add("dash-open"); dashSync();
 if(!o.keepFocus) setTimeout(function(){var c=L.querySelector(".hm-sh [data-dashclose]"); if(c) c.focus({preventScroll:true})},o.instant?0:80);
 if(location.hash!=="#dash"&&history.replaceState) history.replaceState(null,"","#dash")}
function dashClose(){var L=$("#dash"); if(!L||!dash.open) return; dash.open=false; L.classList.remove("open","instant"); L.setAttribute("aria-hidden","true"); document.body.classList.remove("dash-open"); dashSetPush(0); dashSync();
 if(location.hash==="#dash"&&history.replaceState) history.replaceState(null,"",location.pathname+location.search);
 var g=[].filter.call(document.querySelectorAll(".logobtn,.hm-plogo,.dash-grip"),function(x){return x.getClientRects().length})[0]; if(g) g.focus({preventScroll:true})}
function wheelPx(e){return e.deltaMode===1?e.deltaY*16:e.deltaMode===2?e.deltaY*innerHeight:e.deltaY}
function dashBind(){var m=$("#homeMain"), L=$("#dash"); if(!m||!L) return;
 m.addEventListener("scroll",function(){dash.mScroll=Date.now(); if(dash.pull) dashSpring()},{passive:true});
 L.addEventListener("scroll",function(){dash.lScroll=Date.now()},{passive:true});
 /* main page: wheel up at the top pulls the dashboard down */
 m.addEventListener("wheel",function(e){if(dash.open||e.ctrlKey) return; var dy=wheelPx(e);
  if(m.scrollTop>0||Date.now()-dash.mScroll<SETTLE){if(dash.pull) dashSpring(); return}
  if(dy>=0&&!dash.pull) return; e.preventDefault(); dashSetPull(dash.pull-dy*0.5); clearTimeout(dash.tPull);
  if(dash.pull>=DASH_T){dashOpen(); return} dash.tPull=setTimeout(dashSpring,260)},{passive:false});
 /* dashboard: wheel down past its end pushes it back up */
 L.addEventListener("wheel",function(e){if(!dash.open||e.ctrlKey) return; var dy=wheelPx(e), end=L.scrollTop+L.clientHeight>=L.scrollHeight-1;
  if(!end||Date.now()-dash.lScroll<SETTLE){if(dash.push) dashUnpush(); return}
  if(dy<=0&&!dash.push) return; e.preventDefault(); dashSetPush(dash.push+dy*0.5); clearTimeout(dash.tPush);
  if(dash.push>=DASH_T){dashClose(); return} dash.tPush=setTimeout(dashUnpush,260)},{passive:false});
 /* touch: drag down at the top of Home, drag up at the end of the dashboard */
 var t0=null;
 m.addEventListener("touchstart",function(e){t0=(!dash.open&&e.touches.length===1&&m.scrollTop<=0)?e.touches[0].clientY:null},{passive:true});
 m.addEventListener("touchmove",function(e){if(t0==null) return; var d=e.touches[0].clientY-t0; if(d<=0){if(dash.pull) dashSetPull(0); return}
  if(e.cancelable) e.preventDefault(); dashSetPull(d*0.95); if(dash.pull>=DASH_T){t0=null; dashOpen()}},{passive:false});
 m.addEventListener("touchend",function(){if(t0!=null&&!dash.open) dashSpring(); t0=null});
 var u0=null;
 L.addEventListener("touchstart",function(e){u0=(dash.open&&e.touches.length===1&&L.scrollTop+L.clientHeight>=L.scrollHeight-1)?e.touches[0].clientY:null},{passive:true});
 L.addEventListener("touchmove",function(e){if(u0==null) return; var d=u0-e.touches[0].clientY; if(d<=0){if(dash.push) dashSetPush(0); return}
  if(e.cancelable) e.preventDefault(); dashSetPush(d*0.95); if(dash.push>=DASH_T){u0=null; dashClose()}},{passive:false});
 L.addEventListener("touchend",function(){if(u0!=null&&dash.open) dashUnpush(); u0=null})}
/* Escape closes the dashboard when no sheet or picker is open (capture phase, before core's modal handler) */
window.addEventListener("keydown",function(e){if(e.key==="Escape"&&dash.open&&!$(".modal")&&!$(".mp-pop")){e.preventDefault(); dashClose()}},true);
Ember.dash={open:function(o){dashOpen(o)},close:dashClose,toggle:function(){if(dash.open) dashClose(); else dashOpen()},pull:function(px){if(dash.open) dashClose(); dashSetPull(px==null?DASH_T*0.62:px)},isOpen:function(){return dash.open}};

/* ---------- dashboard pieces (prefix hm*). Integration cards and their rules live on the Tools screen (tools.js). ---------- */
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
 '<div class="col hm-succ-l"><div class="flex small"><span class="dot ok"></span>한 번에 성공<div class="spacer"></div><b>'+st.ok+'</b></div><div class="flex small"><span class="dot wait"></span>재시도 뒤 성공<div class="spacer"></div><b>'+st.retry+'</b></div><div class="flex small"><span class="dot fail"></span>실패<div class="spacer"></div><b>'+st.fail+'</b></div><div class="hm-spark" aria-label="최근 7일 성공률">'+spark+'</div></div></div></div></div>'}
function hmWait(){var w=D.homeStats.wait, mx=Math.max.apply(null,w.buckets.map(function(b){return b[1]})), hm=Math.max.apply(null,w.hours);
 return '<div class="card hm-wait"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("clock")+'</span><b>승인 대기 시간</b></div>'+
 '<div class="hm-kv" style="margin:6px 0 14px"><b class="hm-num">'+w.median+'<small>분</small></b><span class="muted small">중앙값 · 지난주 '+w.prev+'분</span></div>'+
 '<div class="hm-hist">'+w.buckets.map(function(b,i){return '<div class="hm-hcol"><span class="small">'+b[1]+'</span><i class="'+(i>2?"slow":"")+'" style="height:'+(b[1]/mx*100)+'%"></i><span class="muted">'+b[0]+'</span></div>'}).join("")+'</div>'+
 '<div class="hm-heat" aria-label="시간대별 승인 요청">'+w.hours.map(function(v,i){return '<i style="opacity:'+(v?(.18+v/hm*.82).toFixed(2):.06)+'" title="'+i+'시 '+v+'건"></i>'}).join("")+'</div><div class="hm-heat-x muted"><span>0시</span><span>12시</span><span>23시</span></div>'+'</div></div>'}
function hmEff(){var e=D.homeStats.eff, mx=Math.max.apply(null,e.map(function(x){return x.tasks}));
 return '<div class="card hm-eff"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("zap")+'</span><b>팀원별 효율</b><div class="spacer"></div><span class="muted small">이번 주 · 완료 작업</span></div><div class="hm-effg">'+
 e.map(function(x){var s=sess(x.id); if(!s) return ""; return '<a class="hm-er" href="conversation.html#'+s.id+'">'+bot(s,30)+'<div class="col" style="min-width:0;gap:4px"><div class="flex small"><b class="ellipsis">'+esc(mname(s))+'</b><div class="spacer"></div><span class="muted">$'+x.cost.toFixed(2)+'/작업</span></div><div class="hm-ebar"><i style="width:'+(x.tasks/mx*100)+'%;background:var(--v-'+vendorOf(s.agent)+')"></i><span>'+x.tasks+'</span></div></div><div class="mini-ring" style="background:radial-gradient(closest-side,var(--surface) 72%,transparent 73%),conic-gradient(var(--s-ok) '+x.ok+'%,var(--track) 0)" title="성공률 '+x.ok+'%"><b class="hm-ringnum">'+x.ok+'</b></div></a>'}).join("")+'</div></div></div>'}
function hmLoad(){var L=D.homeStats.load;
 var meter=function(v,lab){return '<div class="hm-meter'+(v>85?" hot":"")+'"><div class="hm-mt"><i style="height:'+v+'%"></i></div><span class="small"><b>'+v+'</b>%</span><span class="muted">'+lab+'</span></div>'};
 return '<div class="card hm-load"><div class="card-b"><div class="hm-ch"><span class="hm-ico">'+I("cpu")+'</span><b>컴퓨터 부하</b><div class="spacer"></div><a class="hm-go" href="project.html" aria-label="프로젝트 화면의 컴퓨터로">'+I("arrow")+'</a></div><div class="hm-loadg">'+
 L.map(function(c){return '<div class="hm-lc"><div class="flex small"><span class="dot ok"></span><b class="ellipsis">'+c.id+'</b></div><div class="flex hm-meters">'+meter(c.cpu,"CPU")+meter(c.mem,"메모리")+'</div><span class="muted small">팀원 '+c.run+'명 실행</span></div>'}).join("")+'</div></div></div>'}

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
