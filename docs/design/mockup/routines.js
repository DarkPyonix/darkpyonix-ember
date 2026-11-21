"use strict";
/* ---------- routines: dashboard composition (ref1 numbers and pills, ref3 rings and bars) ---------- */
var rtSel=null, rtFilter="all", rtMode="view", rtOpen=false;
if(!D.routines) D.routines=JSON.parse(JSON.stringify(window.EMBER_DEMO.routines));
if(!D.runsWeek) D.runsWeek=JSON.parse(JSON.stringify(window.EMBER_DEMO.runsWeek));
function rtLast(r){return r.runs[0]}
function trig(r){return r.trigger==="time"?'<span class="pill">'+I("clock")+'시간</span>':'<span class="pill sky">'+I("zap")+'이벤트</span>'}
function rate(r){var n=r.runs.filter(function(x){return x.status!=="run"}); return n.length?Math.round(n.filter(function(x){return x.status==="done"}).length/n.length*100):null}
function bigWhen(r){if(r.trigger!=="time") return '<span class="when ev">'+I("zap")+'<span>'+esc((r.cron||"").replace(/^event:\s*/,""))+'</span></span>';
 var m=(r.cron||"").split(" "); var hh=m[1]||"?", mm=("0"+(m[0]||"0")).slice(-2); return '<span class="when">'+(/^\d+$/.test(hh)?("0"+hh).slice(-2)+":"+mm:esc(r.cron))+'</span>'}
function rtCard(r,wide){var rt=rate(r), l=rtLast(r), dots=r.runs.slice(0,8).reverse().map(function(x){return '<i class="rd '+({done:"ok",fail:"fail",run:"run",wait:"wait",unread:"ok"}[x.status])+'" title="'+esc(x.time)+'"></i>'}).join("");
 return '<article class="rtcard'+(wide?" wide":"")+(r.enabled?"":" off")+'" data-open="'+r.id+'" tabindex="0" aria-label="'+esc(r.name)+'">'+
 '<div class="flex"><span class="tile">'+I(r.trigger==="time"?"clock":"zap")+'</span><span class="col spacer" style="min-width:0"><b class="ellipsis">'+esc(r.name)+'</b><span class="small muted ellipsis">'+proj(r.project).name+'</span></span><button class="switch'+(r.enabled?" on":"")+'" data-rtoggle="'+r.id+'" role="switch" aria-checked="'+r.enabled+'" aria-label="'+esc(r.name)+' 켜기/끄기"></button></div>'+
 '<div class="rt-mid"><div class="col" style="min-width:0">'+bigWhen(r)+'<span class="small muted ellipsis">'+esc(r.schedule)+'</span></div>'+
 '<div class="ring rt-ring" style="--p:'+(rt==null?0:rt)+';--c:'+(rt!=null&&rt<60?"var(--c-orange)":"var(--c-blue)")+'"><b>'+(rt==null?"-":rt+"%")+'</b></div></div>'+
 '<div class="rdots" aria-label="최근 실행">'+(dots||'<span class="small muted">실행 기록 없음</span>')+'</div>'+
 '<div class="flex rt-foot">'+bot(r.agent,l&&l.status==="run"?"run":(r.enabled?"done":"fail"),26,r.agent)+'<span class="small muted ellipsis spacer">'+r.agent+' · '+r.computer+'</span><span class="pill'+(r.enabled?" blue":"")+'">'+(r.enabled?esc(r.next):"꺼짐")+'</span></div></article>'}
function rtVisible(q){return D.routines.filter(function(r){return (rtFilter==="all"||r.project===rtFilter)&&(!q||(r.name+r.schedule+r.prompt).toLowerCase().indexOf(q)>=0)})}
function rtGrid(q){var l=rtVisible(q); if(!l.length) return '<div class="empty">루틴이 없습니다</div>';
 var maxRuns=Math.max.apply(null,l.map(function(r){return r.runs.length}));
 return l.map(function(r){return rtCard(r,r.runs.length===maxRuns&&maxRuns>1)}).join("")}
function rtTop(){var on=D.routines.filter(function(r){return r.enabled}), all=[].concat.apply([],D.routines.map(function(r){return r.runs})), fin=all.filter(function(x){return x.status!=="run"}), ok=fin.filter(function(x){return x.status==="done"}).length, pr=fin.length?Math.round(ok/fin.length*100):0, running=all.filter(function(x){return x.status==="run"}).length;
 var W=D.runsWeek, mx=Math.max.apply(null,W.ok.map(function(v,i){return v+W.fail[i]}));
 var bars=W.labels.map(function(l,i){return '<div class="bcol"><div class="bar" style="height:'+Math.round((W.ok[i]+W.fail[i])/mx*100)+'%"><i style="flex:'+W.ok[i]+';background:var(--c-blue)"></i><i style="flex:'+W.fail[i]+';background:var(--c-orange)"></i></div><span'+(i==6?' class="today"':"")+'>'+l+'</span></div>'}).join("");
 var nextR=on.filter(function(r){return r.trigger==="time"})[0];
 return '<div class="kpis"><div class="kpi"><div class="l">켜진 루틴</div><div class="v">'+on.length+'<span class="pill">전체 '+D.routines.length+'</span></div></div>'+
 '<div class="kpi"><div class="l">지금 실행 중</div><div class="v">'+running+'<span class="pill blue">'+(running?"진행":"없음")+'</span></div></div>'+
 '<div class="kpi"><div class="l">이번 주 실행</div><div class="v">'+W.ok.concat(W.fail).reduce(function(a,b){return a+b})+'<span class="pill orange">실패 '+W.fail.reduce(function(a,b){return a+b})+'</span></div></div>'+
 '<div class="kpi"><div class="l">다음 실행</div><div class="v">'+(nextR?esc(nextR.next):"-")+'</div><div class="small muted ellipsis">'+(nextR?esc(nextR.name):"")+'</div></div></div>'+
 '<div class="rt-hero"><div class="card"><div class="card-b usage-top"><div class="spacer"><div class="muted">성공률 (전체 기록)</div><div class="flex" style="gap:16px;margin-top:8px"><div class="ring big-ring" style="--p:'+pr+';--c:var(--c-blue)"><b>'+pr+'%</b></div><div class="legend col" style="gap:6px"><span class="flex"><span class="dot" style="background:var(--c-blue)"></span>성공</span><span class="flex"><span class="dot" style="background:var(--c-orange)"></span>실패</span></div></div></div><div class="barwrap"><div class="muted small" style="margin-bottom:6px">요일별 실행</div><div class="bars2">'+bars+'</div></div></div></div></div>'}
function rtDetail(){
 var box=$("#rtDetail"); if(!box) return;
 if(rtMode!=="view"){box.innerHTML=rtForm(rtMode==="edit"?rtSel:null); return}
 if(!rtSel){box.innerHTML='<div class="empty">루틴을 고르세요</div>'; return}
 var r=rtSel, rt=rate(r);
 box.innerHTML=('<div class="topbar rt-dh"><button class="icon-btn sm backbtn" data-act="rtback" aria-label="목록으로">'+I("back")+'</button><b class="ellipsis crumb-t">'+esc(r.name)+'</b>'+trig(r)+'<div class="spacer"></div><button class="switch'+(r.enabled?" on":"")+'" data-rtoggle="'+r.id+'" role="switch" aria-checked="'+r.enabled+'" aria-label="켜기/끄기"></button><button class="btn sm" data-act="rtedit" title="편집" aria-label="편집">'+I("edit")+'<span class="hide-sm">편집</span></button><button class="btn primary sm" data-act="rtrun" title="지금 실행" aria-label="지금 실행">'+I("play")+'<span class="hide-sm">지금 실행</span></button></div>'+
 '<div class="rdetail"><div class="wrapd">'+
 '<div class="rt-mid rt-hero-when"><div class="col">'+bigWhen(r)+'<span class="rt-sched">'+esc(r.schedule)+' · <span class="cron">'+esc(r.cron)+'</span></span><span class="rt-next">다음 실행 '+(r.enabled?esc(r.next):"꺼짐")+'</span></div><div class="ring big-ring" style="--p:'+(rt==null?0:rt)+';--c:var(--c-blue)"><b>'+(rt==null?"-":rt+"%")+'</b></div></div>'+
 '<div class="kv">'+[["프로젝트",proj(r.project).name],["에이전트",'<span class="flex">'+bot(r.agent,"done",20)+r.agent+'</span><span class="small muted">'+r.model+'</span>'],["컴퓨터 · 프로필",r.computer+'<div class="small muted">'+esc((Ember.profileOf(r.agent,r.project)||{}).name||r.account||"")+'</div>']].map(function(x){return '<div class="inset"><div class="l">'+x[0]+'</div><div class="v">'+x[1]+'</div></div>'}).join("")+'</div>'+
 '<div class="inset"><div class="l small muted">프롬프트</div><div class="prompt">'+esc(r.prompt)+'</div></div>'+
 '<div><b>실행 기록</b><div class="runtiles">'+(r.runs.length?r.runs.map(function(x){var s=sess(x.session);return '<a class="runtile st-'+x.status+'" href="conversation.html#'+x.session+'">'+bot(r.agent,x.status==="done"?"done":x.status,30)+'<span class="col" style="min-width:0"><b class="small">'+esc(x.time)+'</b><span class="small muted ellipsis">'+(s?esc(mname(s)):"세션")+'</span></span>'+pill(x.status)+'</a>'}).join(""):'<div class="empty">아직 실행하지 않았습니다</div>')+'</div></div>'+
 '</div></div>')}
function opt(list,v){return list.map(function(x){var k=x[0]||x,l=x[1]||x;return '<option value="'+esc(k)+'"'+(k===v?" selected":"")+'>'+esc(l)+'</option>'}).join("")}
function rtForm(r){
 r=r||{name:"",project:rtFilter!=="all"?rtFilter:"ember",trigger:"time",schedule:"매일 오전 9시",cron:"0 9 * * *",agent:"Claude Code",model:"claude-sonnet",computer:"mac-mini",account:"ncmnd · Max",prompt:""};
 return '<div class="topbar sheet-h"><b class="sheet-t">'+(rtMode==="edit"?"루틴 편집":"새 루틴")+'</b><div class="sheet-acts"><button class="btn sm" data-act="rtcancel">취소</button><button class="btn primary sm" data-act="rtsave">저장</button></div><button class="icon-btn sm" data-act="rtcancel" aria-label="닫기">'+I("close")+'</button></div>'+
 '<div class="rdetail"><div class="wrapd"><form class="form card card-b" id="rtf" onsubmit="return false">'+
 '<label class="full">이름<input class="field" name="name" required value="'+esc(r.name)+'" placeholder="예: develop CI 실패 요약"></label>'+
 '<label>프로젝트<select class="field" name="project">'+opt(D.projects.map(function(p){return [p.id,p.name]}),r.project)+'</select></label>'+
 '<label>트리거<select class="field" name="trigger">'+opt([["time","시간 (일정)"],["event","이벤트"]],r.trigger)+'</select></label>'+
 '<label>일정 설명<input class="field" name="schedule" value="'+esc(r.schedule)+'" placeholder="매일 오전 9시 / PR이 열릴 때"></label>'+
 '<label>cron 또는 이벤트<input class="field mono" name="cron" value="'+esc(r.cron)+'"></label>'+
 '<div class="flabel">하네스 · 모델 · 추론 강도'+Ember.modelPicker.html({harness:r.agent,model:r.model,project:r.project,onChange:function(v){var el=$("#rtProf"); if(el) el.textContent="프로필: "+(v.profile||"없음")+" (계정은 프로필을 따릅니다)"}})+'<span class="small muted" id="rtProf">프로필: '+esc((Ember.profileOf(r.agent,r.project)||{}).name||"없음")+' (계정은 프로필을 따릅니다)</span></div>'+
 '<div class="flabel">권한 모드'+Ember.modePicker.html({harness:r.agent,mode:r.mode})+'</div>'+
 '<label>컴퓨터<select class="field" name="computer">'+opt(D.computers.map(function(c){return c.id}),r.computer)+'</select></label>'+
 '<label class="full">프롬프트<textarea class="field" name="prompt" placeholder="에이전트에게 매번 줄 지시">'+esc(r.prompt)+'</textarea></label>'+
 '</form></div></div>'}
/* list: the v6 rows (rt items, grouped by project) */
function rtRow(r){var l=rtLast(r);
 return '<div class="rt'+(rtSel&&rtSel.id===r.id?" on":"")+(r.enabled?"":" off")+'" data-open="'+r.id+'" tabindex="0" aria-label="'+esc(r.name)+'"><div class="flex"><span class="t ellipsis spacer">'+esc(r.name)+'</span><button class="switch'+(r.enabled?" on":"")+'" data-rtoggle="'+r.id+'" role="switch" aria-checked="'+r.enabled+'" aria-label="'+esc(r.name)+' 켜기/끄기"></button></div>'+
 '<div class="meta">'+trig(r)+'<span>'+esc(r.schedule)+'</span></div>'+
 '<div class="meta"><span class="dot '+vendorOf(r.agent)+'"></span>'+r.agent+' · '+r.computer+'<span class="spacer"></span>'+(l?pill(l.status)+'<span>'+esc(l.time)+'</span>':'<span>실행 기록 없음</span>')+'</div></div>'}
function rtList(q){var out=""; D.projects.forEach(function(p){if(rtFilter!=="all"&&rtFilter!==p.id) return; var l=D.routines.filter(function(r){return r.project===p.id&&(!q||(r.name+r.schedule+r.prompt).toLowerCase().indexOf(q)>=0)}); if(l.length) out+='<div class="sec">'+p.name+' ('+l.length+')</div>'+l.map(rtRow).join("")}); return out||'<div class="empty">루틴이 없습니다</div>'}
function rtRefresh(){var g=$("#rtList"); if(g) g.innerHTML=rtList($("#rq2").value.toLowerCase()); var tp=$("#rtTop"); if(tp) tp.innerHTML=rtTop(); rtDetail(); save(); var side=$(".side"),t=document.createElement("div"); t.innerHTML=shell("routines",""); side.innerHTML=t.querySelector(".side").innerHTML}
function routines(){
 var seg='<div class="seg scrollx">'+'<button class="on" data-rtf="all">전체</button>'+D.projects.map(function(p){return '<button data-rtf="'+p.id+'">'+(p.id==="unsorted"?"기본 프로젝트":p.short)+'</button>'}).join("")+'</div>';
 render(shell("routines",'<main class="main">'+head("루틴","팀원이 일정이나 이벤트에 맞춰 스스로 하는 일").replace('data-act="new" title="새 팀원" aria-label="새 팀원">'+I("plus")+'<span class="lbl">새 팀원</span>','data-act="rtnew" title="새 루틴" aria-label="새 루틴">'+I("plus")+'<span class="lbl">새 루틴</span>')+
 '<div id="rtTop" class="rt-topwrap"></div><div class="rtsplit"><section class="rtlistpane"><div class="rtlist-h">'+seg+'<label class="searchbox" style="display:flex;width:100%">'+I("search")+'<input id="rq2" placeholder="루틴 검색" aria-label="루틴 검색"></label></div><div id="rtList"></div></section><section class="card rtdetail-pane" id="rtDetail"></section></div></main>'));
 var id=location.hash.slice(1);
 if(id.indexOf("p:")===0){rtFilter=id.slice(2); id=""; document.querySelectorAll("[data-rtf]").forEach(function(b){b.classList.toggle("on",b.dataset.rtf===rtFilter)})}
 rtSel=D.routines.filter(function(r){return r.id===id})[0]; if(rtSel) document.body.classList.add("view-detail"); else rtSel=D.routines.filter(function(r){return rtFilter==="all"||r.project===rtFilter})[0]||null;
 rtMode="view"; rtRefresh();
 $("#rq2").addEventListener("input",function(){$("#rtList").innerHTML=rtList(this.value.toLowerCase())});
}
document.addEventListener("keydown",function(e){if((e.key==="Enter"||e.key===" ")&&e.target.classList&&(e.target.classList.contains("rtcard")||e.target.classList.contains("rt"))){e.preventDefault(); e.target.click()}});
Ember.screens.routines={render:routines,click:function(e,t){var a;
 if((a=t.closest("[data-rtoggle]"))){var rr=D.routines.filter(function(x){return x.id===a.dataset.rtoggle})[0]; rr.enabled=!rr.enabled; rtRefresh(); toast(rr.name+(rr.enabled?" 켬":" 끔")); return true}
 if((a=t.closest("[data-rtf]"))){rtFilter=a.dataset.rtf; a.parentNode.querySelectorAll("button").forEach(function(b){b.classList.toggle("on",b===a)}); $("#rtList").innerHTML=rtList($("#rq2").value.toLowerCase()); return true}
 if((a=t.closest("[data-open]"))){rtSel=D.routines.filter(function(x){return x.id===a.dataset.open})[0]; rtMode="view"; document.body.classList.add("view-detail"); history.replaceState(null,"","#"+rtSel.id); rtRefresh(); var dp=$("#rtDetail"); if(dp&&dp.scrollIntoView&&matchMedia("(max-width:900px) and (min-width:561px)").matches) dp.scrollIntoView({block:"start"}); return true}
 if((a=t.closest("[data-act]"))){var act=a.dataset.act;
  if(act==="rtnew"){rtMode="new"; document.body.classList.add("view-detail"); rtDetail(); return true}
  if(act==="rtback"){document.body.classList.remove("view-detail"); return true}
  if(act==="rtclose"){rtOpen=false; rtMode="view"; history.replaceState(null,"",location.pathname); rtDetail(); return true}
  if(act==="rtedit"){rtMode="edit"; rtDetail(); return true}
  if(act==="rtcancel"){if(rtMode==="new"&&!rtSel) document.body.classList.remove("view-detail"); rtMode="view"; rtDetail(); return true}
  if(act==="rtsave"){var f=$("#rtf"); var E=function(k){return f.elements.namedItem(k)}; if(!E("name").value.trim()){E("name").focus(); toast("이름을 입력하세요"); return true}
   var o={}; ["name","project","trigger","schedule","cron","agent","model","mode","computer","prompt"].forEach(function(k){o[k]=E(k).value.trim()});
   if(rtMode==="edit"){Object.assign(rtSel,o)} else {o.id="rt"+Date.now(); o.enabled=true; o.next=o.trigger==="time"?o.schedule:"다음 이벤트"; o.runs=[]; D.routines.push(o); rtSel=o}
   rtMode="view"; history.replaceState(null,"","#"+rtSel.id); rtRefresh(); toast("저장했습니다"); return true}
  if(act==="rtrun"){var r=rtSel, sid="n"+Date.now();
   D.sessions.push({id:sid,title:r.name+" (루틴)",project:r.project,agent:r.agent,model:r.model,computer:r.computer,account:r.account,status:"run",t:maxT(),msgs:[{k:"user",text:r.prompt},{k:"working",text:"루틴 실행 중"}]});
   var run={status:"run",time:"방금 ("+nowTime()+")",session:sid}; r.runs.unshift(run); rtRefresh(); toast("지금 실행했습니다");
   setTimeout(function(){run.status="done"; var s=sess(sid); s.status="unread"; s.msgs=s.msgs.filter(function(m){return m.k!=="working"}); s.msgs.push({k:"agent",text:"루틴을 실행했습니다. 결과를 요약해 채널에 올렸습니다."}); rtRefresh()},2500); return true}
 }
}};
