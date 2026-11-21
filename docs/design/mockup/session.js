"use strict";
/* ---------- conversation.html: session screen (multi with tabs, single with right panel, status dashboard).
   Owned by the session agent. Names are prefixed (s*, st*, tl*, rp*) so they do not clash with other screens. ---------- */
var cur=null;
var sessView="multi", sessP=null, paneMode={}, STABS=null;
var SX=(Ember.data&&Ember.data.session)||{};
var SGRIP='<svg class="i sgrip" viewBox="0 0 24 24"><path d="M9 6h.01M15 6h.01M9 12h.01M15 12h.01M9 18h.01M15 18h.01" stroke-width="2.6"/></svg>';
function sHash(str){var h=7; for(var i=0;i<str.length;i++) h=(h*31+str.charCodeAt(i))|0; return Math.abs(h)}
function sRng(seed){var a=sHash(String(seed))||1; return function(){a=(a*1103515245+12345)&0x7fffffff; return a/0x7fffffff}}
function members(){return projSessions(sessP.id)}
function sLeadFirst(l){return l.slice().sort(function(a,b){return (b.lead?1:0)-(a.lead?1:0)})}
function sIp(c){return (SX.ips||{})[c]||"192.168.0.10"}

/* ---------- left side tab: session search, one section per project, visible in every view ---------- */
var stQ="", stClosed=JSON.parse(lsGet("ember-mock-stclosed","{}")||"{}");
function stItem(s){
 return '<div class="sthread'+(sessView==="single"&&cur&&s.id===cur.id?" on":"")+'" data-open="'+s.id+'" data-sdrag="'+s.id+'" title="'+esc(mname(s)+" · "+s.title)+'">'+bot(s,26)+
 '<span class="col st-txt"><span class="st-n ellipsis">'+esc(mname(s))+'</span><span class="st-s ellipsis">'+esc(s.title)+'</span></span>'+sdot(s.status)+'</div>'}
function renderSTab(){
 var el=$("#stlist"); if(!el) return; var q=stQ.toLowerCase(), html="";
 D.projects.forEach(function(p){
  var l=sLeadFirst(projSessions(p.id)).filter(function(s){return !q||(mname(s)+" "+s.title+" "+p.name+" "+s.agent).toLowerCase().indexOf(q)>=0});
  if(q&&!l.length) return;
  var closed=!q&&!!stClosed[p.id], wt=l.filter(function(s){return s.status==="wait"}).length;
  html+='<div class="stsec'+(p.id===sessP.id?" here":"")+(closed?" closed":"")+'"><button class="stsec-h" data-stsec="'+p.id+'" aria-expanded="'+!closed+'"><svg class="i chev" viewBox="0 0 24 24">'+IC.chev+'</svg><span class="ellipsis">'+esc(p.name)+'</span><span class="st-c">'+l.length+'</span>'+(wt?'<span class="redbadge" title="승인 대기">'+wt+'</span>':"")+'</button>'+
  (closed?"":'<div class="stsec-b">'+l.map(stItem).join("")+'</div>')+'</div>'});
 el.innerHTML=html||'<div class="empty">일치하는 세션이 없습니다</div>';
}
function stShell(){
 return '<aside class="sside" aria-label="세션 탭"><div class="st-top"><b class="st-title">세션</b><div class="spacer"></div><button class="icon-btn sm st-rail" data-act="strail" title="세션 탭 접기" aria-label="세션 탭 접기">'+I("sidebar-close")+'</button></div>'+
 '<label class="searchbox st-q">'+I("search")+'<input id="stq" placeholder="세션 검색" aria-label="세션 검색" value="'+esc(stQ)+'"></label>'+
 '<div class="st-hint small muted">팀원을 끌어 칸에 놓을 수 있습니다</div><div class="stlist" id="stlist"></div></aside><div class="sscrim" data-act="sdrawer"></div>'}

/* ---------- tabs: each tab keeps its own pane arrangement ---------- */
function stKey(){return "ember-mock-stabs:"+sessP.id}
function stChunk(ids){var n=ids.length; if(n<=3) return n?[ids]:[]; if(n===4) return [ids.slice(0,2),ids.slice(2)]; return [ids.slice(0,3),ids.slice(3,6)]}
function stLoad(){
 var o=null; try{o=JSON.parse(lsGet(stKey(),"null"))}catch(e){}
 var ms=sLeadFirst(members()), ids=ms.map(function(s){return s.id});
 if(!o||!o.tabs||!o.tabs.length){
  var lead=ms.filter(function(s){return s.lead})[0]||ms[0], rev=ms.filter(function(s){return s.status==="unread"||s.status==="fail"})[0];
  o={on:"t1",n:3,tabs:[{id:"t1",name:"작업 보드",rows:stChunk(ids.slice(0,4))},{id:"t2",name:(ms.some(function(x){return x.lead})?"리더와 검토":"검토"),rows:lead?[[lead.id].concat(rev&&rev!==lead?[rev.id]:[])]:[]}]};
 }
 o.tabs.forEach(function(t){t.rows=(t.rows||[]).map(function(r){return r.filter(function(id){return ids.indexOf(id)>=0})}).filter(function(r){return r.length})});
 if(!o.tabs.filter(function(t){return t.id===o.on})[0]) o.on=o.tabs[0].id;
 STABS=o}
function stSave(){lsSet(stKey(),JSON.stringify(STABS))}
function curTab(){return STABS.tabs.filter(function(t){return t.id===STABS.on})[0]}
function stFind(rows,id){for(var r=0;r<rows.length;r++){var c=rows[r].indexOf(id); if(c>=0) return [r,c]} return null}
function tabsHtml(){
 return '<div class="stabs" role="tablist" aria-label="칸 배치 탭">'+STABS.tabs.map(function(t){var n=t.rows.reduce(function(a,r){return a+r.length},0);
  return '<div class="stab'+(t.id===STABS.on?" on":"")+'" role="tab" aria-selected="'+(t.id===STABS.on)+'" data-stab="'+t.id+'" tabindex="0" title="두 번 눌러 이름 바꾸기"><span class="stab-n ellipsis">'+esc(t.name)+'</span><span class="stab-c">'+n+'</span>'+(STABS.tabs.length>1?'<button class="stab-x" data-stabx="'+t.id+'" title="탭 닫기" aria-label="'+esc(t.name)+' 탭 닫기">'+I("close")+'</button>':"")+'</div>'}).join("")+
 '<button class="stab-add" data-act="stabadd" title="새 탭" aria-label="새 탭">'+I("plus")+'</button><div class="spacer"></div><span class="small muted stab-help">칸 머리를 끌어 옮길 수 있습니다. 가운데에 놓으면 바꾸고, 가장자리에 놓으면 나눕니다</span></div>'}

/* ---------- multi view ---------- */
function paneInner(s){
 var o=cur; cur=s; var mode=paneMode[s.id]||"chat", busy=s.status==="run"||s.status==="wait";
 var h='<div class="pane-h" data-pdrag="'+s.id+'" title="끌어서 옮기기">'+SGRIP+bot(s,28)+'<span class="col spacer" style="min-width:0"><b class="ellipsis" title="'+esc(s.title)+'">'+esc(mname(s))+'</b><span class="small muted ellipsis">'+esc(s.title)+'</span></span><span class="ph-pill">'+pill(s.status)+'</span><span class="ph-dot" title="'+ST[s.status][0]+'" aria-label="'+ST[s.status][0]+'">'+sdot(s.status)+'</span>'+
 '<button class="icon-btn sm ph-mode" data-pmode="'+(mode==="term"?"chat":"term")+'" title="'+(mode==="term"?"대화로 보기":"터미널로 보기")+'" aria-label="'+(mode==="term"?"대화로 보기":"터미널로 보기")+'">'+I(mode==="term"?"chat":"cmd")+'</button>'+
 '<div class="mini-seg ph-seg" role="group" aria-label="보기"><button class="'+(mode==="chat"?"on":"")+'" data-pmode="chat">대화</button><button class="'+(mode==="term"?"on":"")+'" data-pmode="term">터미널</button></div>'+
 '<button class="icon-btn sm ph-focus" data-act="focus" title="싱글 뷰로 크게" aria-label="싱글 뷰로 크게">'+I("arrow")+'</button><button class="icon-btn sm pclose" data-act="pclose" title="이 탭에서 빼기" aria-label="이 탭에서 빼기">'+I("close")+'</button></div>'+
 '<div class="pane-b">'+(mode==="term"?termHtml(s):'<div class="doc">'+s.msgs.map(msgHtml).join("")+'</div>')+'</div>'+
 '<div class="pane-c" data-pick-anchor><span class="smodel pc-model" data-msid="'+s.id+'">'+sPicker(s)+sModeChip(s)+'</span><input class="pane-in" data-pin placeholder="'+(mode==="term"?"터미널에 입력 (데모)":(busy?"대기열에 넣기":"메시지"))+'" aria-label="'+esc(s.title)+'에 메시지">'+
 (busy?'<button class="stop" data-act="stop" title="중단" aria-label="중단"><i></i></button>':'<button class="sendround sm" data-act="psend" title="보내기" aria-label="보내기">'+I("send")+'</button>')+'</div>';
 cur=o; return h}
function updatePane(id){var el=document.querySelector('.pane[data-sid="'+id+'"]'); if(!el) return; var inp=el.querySelector("[data-pin]"), keep=inp&&inp.value; el.innerHTML=paneInner(sess(id)); if(typeof sqRefresh==="function") sqRefresh(el); var b=el.querySelector(".pane-b"); b.scrollTop=b.scrollHeight; if(keep) el.querySelector("[data-pin]").value=keep}
function renderMulti(){
 var t=curTab(), sz=t.sz||{}, body;
 if(!t.rows.length){
  body='<div class="mv mv-empty" id="mv" data-sdrop="empty"><div class="mve-in"><b>빈 탭</b><span class="small muted">왼쪽 세션 탭에서 팀원을 끌어 놓거나 아래에서 고르세요</span><div class="mve-add">'+sLeadFirst(members()).map(function(s){return '<button class="chip outline" data-padd="'+s.id+'">'+bot(s,20)+esc(mname(s))+'</button>'}).join("")+'</div></div></div>';
 } else body='<div class="mv" id="mv" data-layout="'+t.rows.map(function(r){return r.join(",")}).join("|")+'" data-panes="'+t.rows.reduce(function(a,r){return a+r.length},0)+'">'+t.rows.map(function(row,ri){
  return '<div class="mvrow" style="flex-grow:'+((sz.r||[])[ri]||1)+'">'+row.map(function(id,i){var s=sess(id); return '<section class="pane" data-sid="'+id+'" tabindex="-1" style="flex-grow:'+((sz["c"+ri]||[])[i]||1)+'">'+paneInner(s)+'</section>'}).join('<div class="gut gv" role="separator" aria-orientation="vertical" tabindex="0" aria-label="크기 조절"></div>')+'</div>'}).join('<div class="gut gh" role="separator" aria-orientation="horizontal" tabindex="0" aria-label="크기 조절"></div>')+'</div>';
 $("#sb").innerHTML='<div class="mvwrap">'+tabsHtml()+body+'</div>';
 document.querySelectorAll(".pane-b").forEach(function(b){b.scrollTop=b.scrollHeight});
}
function saveSizes(){var o={r:[]}; document.querySelectorAll("#mv>.mvrow").forEach(function(r,ri){o.r.push(+r.style.flexGrow||1); o["c"+ri]=[].map.call(r.querySelectorAll(":scope>.pane"),function(p){return +p.style.flexGrow||1})}); curTab().sz=o; stSave()}
/* drop: centre swaps (or replaces when the member comes from the side tab), an edge splits */
function stDrop(sid,target,zone){
 var t=curTab(), rows=t.rows.map(function(r){return r.slice()}), from=stFind(rows,sid);
 if(zone==="empty"){t.rows=[[sid]]; t.sz=null; stSave(); renderMulti(); return}
 var to=stFind(rows,target); if(!to||sid===target) return;
 if(zone==="center"){ if(from){rows[from[0]][from[1]]=target} rows[to[0]][to[1]]=sid }
 else {
  if(from){rows[from[0]].splice(from[1],1); rows=rows.filter(function(r){return r.length})}
  to=stFind(rows,target);
  if(zone==="left"||zone==="right"){ if(rows[to[0]].length>=4){toast("한 줄에는 4칸까지 둘 수 있습니다"); return} rows[to[0]].splice(to[1]+(zone==="right"?1:0),0,sid) }
  else { if(rows.length>=3){toast("3줄까지 둘 수 있습니다"); return} rows.splice(to[0]+(zone==="bottom"?1:0),0,[sid]) }
 }
 t.rows=rows; t.sz=null; stSave(); renderMulti(); renderSTab();
 toast(zone==="center"?(from?"두 칸을 바꿨습니다":"칸을 바꿔 넣었습니다"):"칸을 나눠 넣었습니다");
}
var sDrag=null, sDragEnd=0;
document.addEventListener("pointerdown",function(e){
 if(e.button>0||!e.target.closest) return;
 var h=e.target.closest("[data-pdrag]"), st=e.target.closest(".sthread[data-sdrag]");
 if(h&&!e.target.closest("button,input")) sDrag={sid:h.dataset.pdrag,x:e.clientX,y:e.clientY,on:false};
 else if(st&&sessView==="multi") sDrag={sid:st.dataset.sdrag,x:e.clientX,y:e.clientY,on:false,side:true};
});
function stZone(el,x,y){
 var r=el.getBoundingClientRect(), dl=x-r.left, dr=r.right-x, dt=y-r.top, db=r.bottom-y, ex=Math.max(48,r.width*.25), ey=Math.max(48,r.height*.25);
 var c=[["left",dl/ex],["right",dr/ex],["top",dt/ey],["bottom",db/ey]].sort(function(a,b){return a[1]-b[1]})[0]; return c[1]<1?c[0]:"center"}
var ZLBL={center:"바꾸기",left:"왼쪽에 나누기",right:"오른쪽에 나누기",top:"위에 나누기",bottom:"아래에 나누기",empty:"여기에 놓기"};
document.addEventListener("pointermove",function(e){
 if(!sDrag) return;
 if(!sDrag.on){ if(Math.abs(e.clientX-sDrag.x)+Math.abs(e.clientY-sDrag.y)<7) return;
  sDrag.on=true; document.body.classList.add("sdragging"); var s=sess(sDrag.sid);
  var g=document.createElement("div"); g.className="sghost sq-off"; g.innerHTML=bot(s,24)+'<b>'+esc(mname(s))+'</b>'; document.body.appendChild(g);
  var z=document.createElement("div"); z.className="sdz sq-off"; z.innerHTML='<span></span>'; document.body.appendChild(z);
  var src=document.querySelector('.pane[data-sid="'+sDrag.sid+'"]'); if(src) src.classList.add("dragsrc");
  var sd=$(".sess"); if(sd&&sd.classList.contains("sdrawer-open")&&sDrag.side) sd.classList.remove("sdrawer-open") }
 var g=$(".sghost"), z=$(".sdz"); g.style.transform="translate("+(e.clientX+12)+"px,"+(e.clientY+12)+"px)";
 var el=document.elementFromPoint(e.clientX,e.clientY), p=el&&el.closest&&(el.closest(".pane[data-sid]")||el.closest("[data-sdrop]"));
 if(!p||(p.dataset.sid===sDrag.sid)){z.classList.remove("show"); sDrag.target=null; return}
 var zone=p.dataset.sdrop?"empty":stZone(p,e.clientX,e.clientY), r=p.getBoundingClientRect(), L=r.left,T=r.top,W=r.width,H=r.height;
 if(zone==="left") W=W/2; else if(zone==="right"){L+=W/2; W=W/2} else if(zone==="top") H=H/2; else if(zone==="bottom"){T+=H/2; H=H/2}
 z.style.left=L+4+"px"; z.style.top=T+4+"px"; z.style.width=W-8+"px"; z.style.height=H-8+"px"; z.className="sdz sq-off show z-"+zone; z.firstChild.textContent=ZLBL[zone];
 sDrag.target=p.dataset.sid||null; sDrag.zone=zone;
});
function stDragStop(e){
 if(!sDrag) return; var d=sDrag; sDrag=null; if(!d.on) return;
 sDragEnd=Date.now(); document.body.classList.remove("sdragging");
 [".sghost",".sdz"].forEach(function(c){var x=$(c); if(x) x.remove()}); var src=$(".pane.dragsrc"); if(src) src.classList.remove("dragsrc");
 if(e.type==="pointerup"&&d.zone&&(d.target||d.zone==="empty")) stDrop(d.sid,d.target,d.zone);
}
document.addEventListener("pointerup",stDragStop); document.addEventListener("pointercancel",stDragStop);
/* ---------- messages, terminal, sending (unchanged) ---------- */
function msgHtml(m,i){
 var chev='<svg class="i chev" viewBox="0 0 24 24">'+IC.chev+'</svg>';
 if(m.k==="user") return '<div class="ubub">'+esc(m.text)+'</div>';
 if(m.k==="agent") return '<div class="agent"><p>'+esc(m.text)+'</p></div>';
 if(m.k==="working") return '<div class="working">'+esc(m.text)+'</div>';
 if(m.k==="sys") return '<div class="small" style="align-self:center;color:var(--muted)">'+esc(m.text)+'</div>';
 if(m.k==="cmds") return '<details><summary class="cmd">'+I("cmd")+(m.running?'<span class="spin"></span>실행 중 <span class="mono">'+esc(m.items[0].cmd)+'</span>':'명령 '+m.items.length+'개 실행')+chev+'</summary>'+m.items.map(function(c){return '<details><summary class="cmd" style="padding-left:28px"><span class="mono">'+esc(c.cmd)+'</span>'+chev+'</summary><div class="out">'+esc(c.out)+'</div></details>'}).join("")+'</details>';
 if(m.k==="edit") return '<details open><summary class="cmd">'+I("code")+'<span class="mono">'+esc(m.file)+'</span><span class="pill green">+'+m.add+'</span><span class="pill red">-'+m.del+'</span>'+chev+'</summary><div class="out">'+m.diff.map(function(d){return '<span class="'+(d[0]==="+"?"add":"del")+'">'+esc(d)+'</span>'}).join("\n")+'</div></details>';
 if(m.k==="approval"){
  if(m.state!=="pending") return '<div class="cmd">'+I("shield")+'<span class="mono">'+esc(m.cmd)+'</span><span class="pill '+(m.state==="deny"?"red":"green")+'">'+(m.state==="deny"?"거부함":m.state==="always"?"항상 허용":"허용함")+'</span></div>';
  return '<div class="approve"><div class="flex">'+I("shield")+'<b>'+cur.computer+'에서 명령을 실행할까요?</b><div class="spacer"></div><span class="pill orange">대기 중</span></div><div class="out" style="padding-left:0">'+esc(m.cmd)+'</div><div class="flex wrap"><button class="btn primary sm" data-approve="allow" data-i="'+i+'">한 번 허용</button><button class="btn sm" data-approve="always" data-i="'+i+'">이 프로젝트에서 항상 허용</button><button class="btn sm" style="color:var(--c-red)" data-approve="deny" data-i="'+i+'">거부</button></div></div>';
 }
 return "";
}
function termHtml(s){
 var L=[], v=vendorOf(s.agent), dir="~/"+proj(s.project).name;
 function add(c,t){L.push('<span class="'+c+'">'+esc(t)+'</span>')}
 if(v==="codex"){add("tb",">_ OpenAI Codex (v0.46)"); add("tm","model: "+s.model+"   directory: "+dir); add("tm","")}
 else {add("tb","╭────────────────────────────────────╮"); add("tb","│ ✻ Welcome to "+(v==="gemini"?"Gemini CLI":"Claude Code")+"          │"); add("tm","│   cwd: "+dir); add("tb","╰────────────────────────────────────╯"); add("tm","")}
 s.msgs.forEach(function(m){
  if(m.k==="user") add("tu",(v==="codex"?"› ":"> ")+m.text);
  else if(m.k==="agent") add("ta",(v==="codex"?"• ":"⏺ ")+m.text);
  else if(m.k==="cmds") m.items.forEach(function(c){add("tt",(v==="codex"?"• Ran ":"⏺ Bash(")+c.cmd+(v==="codex"?"":")")); add("tm",(v==="codex"?"  └ ":"  ⎿  ")+String(c.out).split("\n")[0])});
  else if(m.k==="edit"){add("tt",(v==="codex"?"• Edited ":"⏺ Update(")+m.file+(v==="codex"?"":")")); add("tm","  ⎿  +"+m.add+" -"+m.del); m.diff.forEach(function(d){add(d[0]==="+"?"tadd":"tdel","     "+d)})}
  else if(m.k==="approval"&&m.state==="pending"){add("tw",v==="codex"?"Allow command? "+m.cmd:"Bash command: "+m.cmd); if(v==="codex") add("tw","  [y] yes  [a] always  [n] no"); else {add("tw","  Do you want to proceed?"); add("tsel","  ❯ 1. Yes"); add("tm","    2. Yes, and don't ask again for this command"); add("tm","    3. No, and tell Claude what to do differently")}}
  else if(m.k==="working") add("tw",v==="codex"?"• Working (esc to interrupt)":"✻ Working… (esc to interrupt)");
  else if(m.k==="sys") add("tm","  "+m.text);
 });
 add("tm",""); add("tb",v==="codex"?"▌ Ask Codex to do anything":"> ");
 add("tm",v==="codex"?"  ⏎ send   ⌃J newline   ⌃C quit":"  ? for shortcuts");
 return '<pre class="term" aria-label="'+esc(s.agent)+' 터미널 화면 (데모)">'+L.join("\n")+'</pre>'}
function finishTurn(s,reply){setTimeout(function(){s.msgs=s.msgs.filter(function(m){return m.k!=="working"}); s.msgs.push({k:"agent",text:reply}); s.status="done"; refreshFor(s)},1400)}
function sendConv(txt){
 var ta=$("#msg"), v=(txt!=null?txt:(ta?ta.value:"")).trim(); if(!v) return; var s=cur;
 s.msgs.push({k:"user",text:v}); s.t=maxT();
 if(s.status==="run"||s.status==="wait"){s.msgs.push({k:"sys",text:"메시지를 대기열에 넣었습니다. 이번 턴이 끝나면 전달합니다."})}
 else{s.msgs.push({k:"working",text:"작업 중"}); s.status="run"; finishTurn(s,"확인했습니다. \""+v.slice(0,30)+"\" 요청을 처리했고 바뀐 파일은 없습니다.")}
 refreshConv();
}
/* ---------- pane gutters, double-click to single, pane input ---------- */
document.addEventListener("pointerdown",function(e){var g=e.target.closest&&e.target.closest(".gut"); if(!g) return; e.preventDefault();
 var v=g.classList.contains("gv"), par=g.parentNode, items=[].filter.call(par.children,function(x){return !x.classList.contains("gut")});
 items.forEach(function(x){var r=x.getBoundingClientRect(); x.style.flexGrow=v?r.width:r.height});
 var A=g.previousElementSibling, B=g.nextElementSibling, a0=+A.style.flexGrow, b0=+B.style.flexGrow, p0=v?e.clientX:e.clientY, MIN=v?180:140;
 g.setPointerCapture(e.pointerId); g.classList.add("drag");
 function mv(ev){var d=(v?ev.clientX:ev.clientY)-p0, na=Math.max(MIN,Math.min(a0+b0-MIN,a0+d)); A.style.flexGrow=na; B.style.flexGrow=a0+b0-na}
 function up(){g.removeEventListener("pointermove",mv); g.removeEventListener("pointerup",up); g.classList.remove("drag"); saveSizes()}
 g.addEventListener("pointermove",mv); g.addEventListener("pointerup",up)});
document.addEventListener("keydown",function(e){var g=e.target.closest&&e.target.closest(".gut"); if(!g||["ArrowLeft","ArrowRight","ArrowUp","ArrowDown"].indexOf(e.key)<0) return; e.preventDefault();
 var A=g.previousElementSibling,B=g.nextElementSibling,v=g.classList.contains("gv"),ra=A.getBoundingClientRect(),rb=B.getBoundingClientRect(),a=v?ra.width:ra.height,b=v?rb.width:rb.height,d=(e.key==="ArrowLeft"||e.key==="ArrowUp")?-24:24;
 [].forEach.call(g.parentNode.children,function(x){if(!x.classList.contains("gut")){var r=x.getBoundingClientRect(); x.style.flexGrow=v?r.width:r.height}});
 A.style.flexGrow=Math.max(120,a+d); B.style.flexGrow=Math.max(120,b-d); saveSizes()});
document.addEventListener("dblclick",function(e){var p=e.target.closest&&e.target.closest(".pane[data-sid]"); if(!p||e.target.closest("input,button")) return; cur=sess(p.dataset.sid); setView("single")});
document.addEventListener("keydown",function(e){var i=e.target.closest&&e.target.closest("[data-pin]"); if(!i||e.key!=="Enter"||e.isComposing) return; e.preventDefault(); var p=i.closest(".pane"); cur=sess(p.dataset.sid); var v=i.value; i.value=""; sendConv(v)});

/* ---------- single view: conversation plus the right panel (environment, context, outputs, changes) ---------- */
function renderConv(){
 var s=cur, p=proj(s.project), d=$(".detail");
 d.innerHTML='<div class="topbar"><button class="icon-btn sm backbtn" data-back>'+I("back")+'</button><span class="crumb-bot">'+bot(s,24)+'</span><b class="ellipsis crumb-t" title="'+esc(s.title)+'">'+esc(mname(s))+'</b>'+pill(s.status)+'<div class="spacer"></div><button class="icon-btn sm rp-btn" data-act="rptoggle" title="환경과 변경 사항 패널" aria-label="환경과 변경 사항 패널">'+I("sidebar-open")+'</button></div>'+
 '<div class="scroll"><div class="doc">'+s.msgs.map(msgHtml).join("")+'</div></div>'+
 '<div class="composer"><textarea id="msg" placeholder="무엇이든 물어보세요. @로 파일, /로 명령"></textarea><div class="flex"><button class="pickchip" data-act="attach" title="첨부" aria-label="첨부">'+I("plus")+'</button><span class="smodel" data-msid="'+cur.id+'">'+sModeChip(cur)+'</span><div class="spacer"></div><span class="smodel" data-msid="'+s.id+'">'+sPicker(s)+'</span>'+
 (s.status==="run"||s.status==="wait"?'<button class="stop" title="중단" aria-label="중단" data-act="stop"><i></i></button>':'<button class="icon-btn sm" style="background:var(--ink);color:var(--ink-text)" data-act="send" title="보내기" aria-label="보내기">'+I("send")+'</button>')+'</div></div>';
 var sc=$(".scroll"); sc.scrollTop=sc.scrollHeight;
 var ta=$("#msg"); ta.addEventListener("keydown",function(e){if(e.key==="Enter"&&!e.shiftKey&&!e.isComposing){e.preventDefault();sendConv()}});
 renderRPanel();
}
function rpCtx(s){
 var r=sRng(s.id+"ctx"), v=vendorOf(s.agent), cap=v==="codex"?400:1000, k=function(a,b){return Math.round((a+r()*(b-a))*10)/10};
 var parts=[["메시지","var(--c-blue)",k(60,220)+s.msgs.length*4],["시스템 도구","var(--c-orange)",k(12,22)],["스킬","var(--c-green)",k(3,9)],["메모리 파일 (AGENTS.md)","var(--c-sky)",k(2,6)],["시스템 프롬프트","var(--c-grey)",3],["MCP 서버 지시","var(--c-grey)",k(1,3)],["마지막 압축 요약","var(--c-grey)",k(0,60)],["자동 압축 버퍼","var(--track)",33]];
 parts.forEach(function(p){p[2]=Math.round(p[2]*10)/10; p[3]=Math.round(p[2]/cap*1000)/10});
 var used=Math.round(parts.reduce(function(a,p){return a+p[2]},0)*10)/10, usedP=Math.round(parts.reduce(function(a,p){return a+p[3]},0)*10)/10;
 return {cap:cap,parts:parts,used:used,usedP:usedP,free:Math.round((cap-used)*10)/10,freeP:Math.round((100-usedP)*10)/10}}
function rpChanges(s){
 var r=sRng(s.id+"chg"), files=[], seen={};
 s.msgs.forEach(function(m){if(m.k==="edit"&&!seen[m.file]){seen[m.file]=1; files.push([m.file,m.add,m.del])}});
 var pool=["ember/server/src/router.rs","ember/client/src/session.rs","docs/SPEC.md","ember/proxy/src/shim.rs",".github/workflows/test.yml","ember/node/src/agent.rs"];
 var n=s.status==="done"&&!files.length?0:1+Math.floor(r()*3); for(var i=0;i<n;i++){var f=pool[Math.floor(r()*pool.length)]; if(!seen[f]){seen[f]=1; files.push([f,5+Math.floor(r()*140),Math.floor(r()*40)])}}
 return files}
function rpSec(id,title,body,right){var off=lsGet("ember-mock-rp:"+id,"1")==="0";
 return '<section class="rpsec'+(off?" off":"")+'"><button class="rpsec-h" data-rpsec="'+id+'" aria-expanded="'+!off+'"><b>'+title+'</b><svg class="i chev" viewBox="0 0 24 24">'+IC.chev+'</svg><div class="spacer"></div>'+(right||"")+'</button><div class="rpsec-b">'+body+'</div></section>'}
function renderRPanel(){
 var rp=$("#rp"); if(!rp) return; var s=cur, p=proj(s.project), c=(D.computers||[]).filter(function(x){return x.id===s.computer})[0]||{os:""};
 var branch=(D.prs||[]).filter(function(x){return x.sid===s.id})[0], br=branch?branch.branch:(s.lead?"develop":"feat/"+p.short+"-"+(s.role||"work"));
 var env='<div class="rprow">'+I("pc")+'<span class="spacer ellipsis">'+esc(s.computer)+' <span class="muted">· '+esc(c.os)+'</span></span><span class="mono muted">'+sIp(s.computer)+'</span></div>'+
  '<div class="rprow">'+I("folder")+'<span class="spacer ellipsis">DarkPyonix/darkpyonix-'+esc(p.short)+'</span></div>'+
  '<div class="rprow">'+I("git")+'<span class="spacer ellipsis mono">'+esc(br)+'</span>'+(s.lead?"":'<span class="pill">worktree</span>')+'</div>'+
  '<div class="rprow">'+bot(s.agent,"done",18)+'<span class="spacer ellipsis">'+esc(s.agent)+' <span class="muted">· '+esc(s.model)+'</span></span></div>'+
  '<div class="rprow">'+I("shield")+'<span class="spacer ellipsis">'+esc(sModeName(s))+' <span class="muted">· '+esc(s.account)+'</span></span></div>';
 var cx=rpCtx(s), pct=Math.round(cx.usedP);
 var ctx='<div class="ctx-top"><b>'+cx.usedP.toFixed(1)+'% 사용됨</b><span class="muted mono">'+cx.used+'k / '+(cx.cap>=1000?"1M":cx.cap+"k")+'</span></div><div class="ctxbar" role="img" aria-label="컨텍스트 '+pct+'% 사용">'+cx.parts.map(function(q){return '<i style="width:'+(q[2]/cx.cap*100)+'%;background:'+q[1]+'" title="'+q[0]+' '+q[2]+'k"></i>'}).join("")+'</div>'+
  '<div class="ctxleg">'+cx.parts.map(function(q){return '<div class="ctxrow"><span class="sw" style="background:'+q[1]+'"></span><span class="spacer ellipsis">'+q[0]+'</span><span class="mono muted">'+q[2]+'k</span><span class="mono ctxp">'+q[3].toFixed(1)+'%</span></div>'}).join("")+
  '<div class="ctxrow"><span class="sw free"></span><span class="spacer">남은 공간</span><span class="mono muted">'+cx.free+'k</span><span class="mono ctxp">'+cx.freeP.toFixed(1)+'%</span></div></div>';
 var outs=[]; (D.prs||[]).filter(function(x){return x.sid===s.id}).forEach(function(x){outs.push([I("git"),"#"+x.num+" "+x.title,"PR","vcs.html#"+x.project])});
 ((SX.outputs||{})[s.id]||[]).forEach(function(o){outs.push([I(o.icon||"edit"),o.name,o.kind,""])});
 (sPorts(s).slice(0,1)).forEach(function(o){outs.push([I("globe"),"http://"+sIp(s.computer)+":"+o.port,"웹 페이지",""])});
 var out=outs.length?outs.map(function(o){return '<div class="rprow'+(o[3]?" link":"")+'"'+(o[3]?' data-href="'+o[3]+'"':"")+'>'+o[0]+'<span class="spacer ellipsis">'+esc(o[1])+'</span><span class="small muted">'+o[2]+'</span></div>'}).join(""):'<div class="small muted">아직 결과물이 없습니다</div>';
 var ch=rpChanges(s), add=ch.reduce(function(a,f){return a+f[1]},0), del=ch.reduce(function(a,f){return a+f[2]},0), made=lsGet("ember-mock-pr:"+s.id,"");
 var prBtn=branch?'<a class="btn sm" href="vcs.html#'+p.id+'">PR #'+branch.num+' 보기</a>':made?'<a class="btn sm" href="vcs.html#'+p.id+'">PR #'+made+' 열림</a>':(ch.length?'<button class="btn primary sm" data-act="mkpr">PR 생성</button>':"");
 var chg=ch.length?'<div class="rprow">'+I("edit")+'<span class="spacer"><span class="add">+'+add+'</span> <span class="del">-'+del+'</span> <span class="muted">· 파일 '+ch.length+'개</span></span>'+prBtn+'</div>'+ch.map(function(f){return '<div class="rpfile"><span class="mono ellipsis spacer" title="'+esc(f[0])+'">'+esc(f[0])+'</span><span class="mono add">+'+f[1]+'</span><span class="mono del">-'+f[2]+'</span></div>'}).join(""):'<div class="small muted">바뀐 파일이 없습니다</div>';
 rp.innerHTML='<div class="rp-h"><b>세션 정보</b><div class="spacer"></div><button class="icon-btn sm" data-act="rptoggle" title="패널 닫기" aria-label="패널 닫기">'+I("close")+'</button></div>'+
  rpSec("env","환경",env)+rpSec("ctx","컨텍스트 구성",ctx,'<span class="small muted">'+pct+'%</span>')+rpSec("out","결과물",out,'<span class="small muted">'+outs.length+'</span>')+rpSec("chg","변경 사항",chg,ch.length?'<span class="small"><span class="add">+'+add+'</span> <span class="del">-'+del+'</span></span>':"");
}
function renderSingle(){
 $("#sb").innerHTML='<div class="sgl'+(lsGet("ember-mock-rpoff","0")==="1"?" rp-off":"")+'"><section class="detail"></section><aside class="rpanel" id="rp" aria-label="세션 정보"></aside></div>';
 renderConv();
}

/* ---------- status view: dashboard plus the agent-to-agent communication timeline ---------- */
var KB=[["queue","대기"],["run","실행"],["wait","승인 대기"],["review","검토"],["done","완료"]];
function kcol(s){return {run:"run",wait:"wait",unread:"review",done:"done",fail:"queue"}[s.status]}
var TLC=["var(--c-blue)","var(--c-orange)","var(--c-green)","var(--c-sky)","var(--c-grey)"];
/* leaderless project (기본 프로젝트): members only, each lane on its own, no orchestrator and no handoffs */
function tlBuildFlat(all){var r=sRng(sessP.id+"tlf"), lanes=[], T=4;
 all.slice(0,5).forEach(function(w,i){var L={s:w,segs:[],c:TLC[i%TLC.length]}, t=.4+i*.8, end=t+5+r()*7, k=0;
  while(t<end-.3){var len=.5+r()*1.7, ty=k%2?"tool":"think"; if(w.status==="fail"&&k===1){ty="fail"; len=1.6} L.segs.push([t,Math.min(t+len,end),ty]); t+=len+.18; k++}
  if(w.status==="wait"){L.segs.push([end,end+1.6,"wait"]); end+=1.6}
  lanes.push(L); T=Math.max(T,end+.4)});
 var seq=lanes.reduce(function(a,L){return a+L.segs.filter(function(g){return g[2]!=="wait"}).reduce(function(b,g){return b+g[1]-g[0]},0)},0);
 return {lanes:lanes,hand:[],T:Math.round(T*10)/10,fast:(seq/T).toFixed(1),cost:(.04*lanes.length+r()*.05).toFixed(2),flat:true}}
/* builds a deterministic run: the leader thinks, hands work to members, waits, then hands a second phase */
function tlBuild(){
 var all=members(); if(!all.some(function(x){return x.lead})) return tlBuildFlat(all);
 var ms=sLeadFirst(all), lead=ms.filter(function(s){return s.lead})[0]||ms[0], W=ms.filter(function(s){return s!==lead}).slice(0,4), r=sRng(sessP.id+"tl");
 var lanes=[{s:lead,segs:[[0,2,"think"]],c:"var(--text-2)"}], hand=[], ends=[];
 var p1=W.length>1?W.slice(0,W.length-1):W, p2=W.length>1?W[W.length-1]:null;
 function work(w,li,start,dur){var L={s:w,segs:[],c:TLC[(li-1)%TLC.length]}, t=start+.3, end=start+dur, k=0;
  hand.push({a:0,b:li,t:start});
  while(t<end-.3){var len=.5+r()*1.7, ty=k%2?"tool":"think";
   if(w.status==="fail"&&k===1){ty="fail"; len=1.6} if(w.status==="fail"&&k===2){ty="retry"; len=1.2}
   L.segs.push([t,Math.min(t+len,end),ty]); t+=len+.18; k++}
  if(w.status==="wait"){L.segs.push([end,end+1.6,"wait"]); end+=1.6}
  hand.push({a:li,b:0,t:end+.1}); lanes.push(L); return end+.1}
 p1.forEach(function(w,i){ends.push(work(w,i+1,2.1+i*.45,6+r()*6))});
 var e1=Math.max.apply(null,ends.concat([4]))+.2; lanes[0].segs.push([2.1,e1,"wait"],[e1+.1,e1+2,"think"]);
 var T=e1+2;
 if(p2){var e2=work(p2,lanes.length,e1+2.1,5+r()*2); lanes[0].segs.push([e1+2.1,e2,"wait"],[e2+.1,e2+2,"think"],[e2+2.1,e2+3,"tool"]); T=e2+3}
 else lanes[0].segs.push([e1+2.1,e1+3,"tool"]), T=e1+3;
 var seq=lanes.reduce(function(a,L){return a+L.segs.filter(function(g){return g[2]!=="wait"}).reduce(function(b,g){return b+g[1]-g[0]},0)},0);
 return {lanes:lanes,hand:hand,T:Math.round(T*10)/10,fast:(seq/T).toFixed(1),cost:(.04*lanes.length+r()*.05).toFixed(2)}}
var tl=null, tlRun={t:null,on:false,x:1,raf:0,last:0};
var TLN={think:"생각",tool:"도구 호출",wait:"대기",fail:"실패",retry:"재시도"};
function tlHtml(){
 tl=tlBuild(); var T=tl.T, P=function(t){return (t/T*100).toFixed(3)+"%"}, LH=56;
 var labels=tl.lanes.map(function(L,i){var s=L.s; return '<div class="tl-lab" data-lane="'+s.id+'"'+(i===0&&!tl.flat?' data-lead':'')+' style="height:'+LH+'px">'+bot(s,30)+'<span class="col" style="min-width:0"><b class="ellipsis"><span class="tl-dot" style="background:'+L.c+'"></span>'+esc(mname(s))+(i===0&&!tl.flat?' <span class="pill tl-orch">오케스트레이터</span>':"")+'</b><span class="small muted ellipsis">'+esc(s.agent)+' · '+esc(s.model)+'</span></span></div>'}).join("");
 var rows=tl.lanes.map(function(L,i){return '<div class="tl-lane" data-lane="'+L.s.id+'"'+(i===0&&!tl.flat?' data-lead':'')+' aria-label="'+esc(mname(L.s))+'" style="height:'+LH+'px;--lc:'+L.c+'">'+L.segs.map(function(g){return '<i class="tl-seg k-'+g[2]+'" style="left:'+P(g[0])+';width:calc('+P(g[1]-g[0])+' - 2px)" title="'+TLN[g[2]]+' '+g[0].toFixed(1)+'s ~ '+g[1].toFixed(1)+'s">'+(g[2]==="fail"?I("close"):g[2]==="retry"?I("refresh"):"")+'</i>'}).join("")+'</div>'}).join("");
 var hs=tl.hand.map(function(h){var lo=Math.min(h.a,h.b), hi=Math.max(h.a,h.b), c=tl.lanes[Math.max(h.a,h.b)].c;
  return '<span class="tl-h'+(h.a===0?" down":" up")+'" style="left:'+P(h.t)+';top:'+(lo*LH+LH/2+9)+'px;height:'+((hi-lo)*LH-18)+'px;--lc:'+c+'"></span>'}).join("");
 var ticks=""; for(var t=0;t<=T;t+=5) ticks+='<span class="tl-tick" style="left:'+P(t)+'"><i></i>'+t+'s</span>';
 var leg=[["think","생각"],["tool","도구 호출"],["wait","대기"],["hand","핸드오프"],["fail","실패"]].map(function(x){return '<span class="tl-lg"><i class="tl-seg k-'+x[0]+'"'+(x[0]==="fail"?"":' style="--lc:var(--c-blue)"')+'></i>'+x[1]+'</span>'}).join("");
 return '<div class="card tlcard"><div class="card-h tl-head"><div class="col" style="min-width:0"><b class="tl-title">에이전트 간 통신</b><span class="small muted">'+I("check")+tl.lanes.length+'명 · '+T+'s · 순차 실행보다 '+tl.fast+'배 빠름 · $'+tl.cost+'</span></div></div>'+
 '<div class="tl-body"><div class="tl-labs" style="padding-top:4px">'+labels+'</div><div class="tl-scroll"><div class="tl-plot"><div class="tl-grid">'+ticks+'</div><div class="tl-lanes" style="padding-top:4px">'+rows+hs+'</div><div class="tl-cover"></div><div class="tl-head-line"><i></i></div></div></div></div>'+
 '<div class="tl-foot"><button class="sendround tl-play" data-tl="play" title="재생" aria-label="재생">'+I("play")+'</button><button class="icon-btn sm" data-tl="restart" title="처음부터" aria-label="처음부터">'+I("refresh")+'</button><div class="mini-seg tl-speed" role="group" aria-label="재생 속도"><button data-tlx="1" class="'+(tlRun.x===1?"on":"")+'">1x</button><button data-tlx="2" class="'+(tlRun.x===2?"on":"")+'">2x</button></div><span class="mono small tl-now">'+T+'s</span><div class="spacer"></div><div class="tl-legend">'+leg+'</div></div></div>'}
function tlPaint(){var c=$(".tl-cover"), h=$(".tl-head-line"), n=$(".tl-now"); if(!c||!tl) return; var t=tlRun.t==null?tl.T:tlRun.t, p=(t/tl.T*100)+"%";
 c.style.left=p; h.style.left=p; h.classList.toggle("show",tlRun.t!=null&&tlRun.t<tl.T); n.textContent=t.toFixed(1)+"s / "+tl.T+"s";
 var b=$(".tl-play"); if(b) {b.innerHTML=tlRun.on?I("pause"):I("play"); b.title=tlRun.on?"일시 정지":"재생"; b.setAttribute("aria-label",b.title)}}
function tlTick(now){ if(!tlRun.on) return; if(!$(".tl-cover")){tlRun.on=false; return}
 tlRun.t=Math.min(tl.T,tlRun.t+(now-tlRun.last)/1000*tlRun.x); tlRun.last=now; if(tlRun.t>=tl.T) tlRun.on=false; tlPaint(); if(tlRun.on) tlRun.raf=requestAnimationFrame(tlTick)}
function tlPlay(restart){ if(restart||tlRun.t==null||tlRun.t>=tl.T) tlRun.t=0; if(tlRun.on&&!restart){tlRun.on=false; tlPaint(); return}
 tlRun.on=true; tlRun.last=performance.now(); cancelAnimationFrame(tlRun.raf); tlRun.raf=requestAnimationFrame(tlTick); tlPaint()}
function renderStatus(){
 var ms=sLeadFirst(members()), rts=(D.routines||[]).filter(function(r){return r.project===sessP.id&&r.enabled});
 var cnt=function(st){return ms.filter(function(s){return s.status===st}).length}, r=sRng(sessP.id+"kpi");
 var tok=ms.map(function(s){return [s,Math.round(40+sRng(s.id+"tok")()*360)]}), tsum=tok.reduce(function(a,x){return a+x[1]},0), tmax=Math.max.apply(null,tok.map(function(x){return x[1]}));
 var kpi=[["팀원",ms.length,"리더 1 · 서브 에이전트 "+ms.reduce(function(a,s){return a+((D.subagents||{})[s.id]||[]).length},0),""],["실행 중",cnt("run"),"평균 턴 "+(2+Math.floor(r()*5))+"분","blue"],["승인 대기",cnt("wait"),cnt("wait")?"가장 오래 "+(3+Math.floor(r()*20))+"분":"없음","orange"],["실패",cnt("fail"),cnt("fail")?"재시도 가능":"없음","red"],["오늘 토큰",(tsum/1000).toFixed(1)+"M","$"+(tsum*0.011).toFixed(2),""],["성공률",Math.round(100-cnt("fail")/Math.max(1,ms.length)*100)+"%","최근 7일","green"]];
 var kh='<div class="skpis">'+kpi.map(function(k){return '<div class="skpi'+(k[3]?" t-"+k[3]:"")+'"><span class="l">'+k[0]+'</span><span class="v">'+k[1]+'</span><span class="small muted">'+k[2]+'</span></div>'}).join("")+'</div>';
 var cols=KB.map(function(c){var cards=ms.filter(function(s){return kcol(s)===c[0]}).map(function(s){return '<div class="kcard" data-act="focus" data-sid="'+s.id+'"><div class="flex">'+bot(s,28)+'<b class="ellipsis spacer">'+esc(mname(s))+'</b></div><div class="small muted ellipsis" style="margin-top:6px">'+esc(activity(s))+'</div><div class="small muted kmeta">'+(s.status==="fail"?'<span class="pill red">실패</span>':"")+'<span class="ellipsis">'+s.agent+' · '+s.computer+'</span></div></div>'});
  if(c[0]==="queue") cards=cards.concat(rts.map(function(r){return '<div class="kcard" data-href="routines.html#'+r.id+'"><div class="flex">'+I("repeat")+'<b class="ellipsis spacer">'+esc(r.name)+'</b></div><div class="small muted" style="margin-top:6px">루틴 · '+esc(r.next)+'</div></div>'}));
  return '<div class="kcol"><div class="kcol-h">'+c[1]+'<span class="pill">'+cards.length+'</span></div>'+(cards.join("")||'<div class="empty small">없음</div>')+'</div>'}).join("");
 var prs=(D.prs||[]).filter(function(p){return p.project===sessP.id});
 var CI={pass:["CI 통과","green"],fail:["CI 실패","red"],run:["CI 실행 중","blue"]};
 var prh=prs.length?prs.map(function(p){var s=sess(p.sid);return '<div class="list-row" data-href="conversation.html#'+p.sid+'"><div class="tile">'+I("git")+'</div><div class="spacer col"><span class="t ellipsis">#'+p.num+' '+esc(p.title)+'</span><span class="s ellipsis">'+(s?esc(mname(s))+' · ':"")+p.branch+'</span></div><div class="right"><span class="pill '+CI[p.ci][1]+'">'+CI[p.ci][0]+'</span><span class="small muted">'+(p.state==="merged"?"병합됨":p.review)+'</span></div></div>'}).join(""):'<div class="empty">PR이 없습니다</div>';
 function tree(n){return '<li><div class="tnode" '+(n.sid?'data-href="conversation.html#'+n.sid+'"':"")+'>'+(n.s?bot(n.s,22):n.status==="run"?'<span class="spin"></span>':'<span class="dot '+({wait:"wait",done:"ok",fail:"fail"}[n.status]||"")+'"></span>')+'<b class="ellipsis">'+esc(n.name)+'</b><span class="small muted ellipsis">'+esc(n.task||"")+'</span></div>'+(n.children&&n.children.length?'<ul>'+n.children.map(tree).join("")+'</ul>':"")+'</li>'}
 var roots=ms.filter(function(s){return D.subagents&&D.subagents[s.id]}).map(function(s){return {s:s,name:mname(s),sid:s.id,task:s.agent+" · "+s.model,children:D.subagents[s.id]}});
 var load='<div class="card-b sload">'+tok.sort(function(a,b){return b[1]-a[1]}).map(function(x){return '<div class="sl-row">'+bot(x[0],22)+'<span class="ellipsis sl-n">'+esc(mname(x[0]))+'</span><span class="track"><i style="width:'+(x[1]/tmax*100)+'%;background:'+VC[vendorOf(x[0].agent)]+'"></i></span><span class="mono small muted">'+x[1]+'k</span></div>'}).join("")+'</div>';
 var comps=(D.computers||[]).filter(function(c){return sessP.computers.indexOf(c.id)>=0}).map(function(c){var u=c.status==="online"?Math.round(20+sRng(c.id)()*70):0, n=ms.filter(function(s){return s.computer===c.id&&s.status==="run"}).length;
  return '<div class="sl-row">'+I("pc")+'<span class="ellipsis sl-n">'+c.id+'<span class="small muted"> · '+sIp(c.id)+'</span></span><span class="track"><i style="width:'+u+'%;background:'+(u>75?"var(--c-orange)":"var(--c-green)")+'"></i></span><span class="mono small muted">'+(c.status==="online"?u+"% · "+n+"실행":"오프라인")+'</span></div>'}).join("");
 $("#sb").innerHTML='<div class="statusv">'+kh+tlHtml()+
 '<div class="card skan"><div class="card-h"><div class="ico">'+I("grid")+'</div>칸반<div class="spacer"></div><span class="small muted">팀원 '+ms.length+' · 루틴 '+rts.length+'</span></div><div class="kanban">'+cols+'</div></div>'+
 '<div class="grid g3 sgrid">'+card("git","PR과 CI",prh,'<span class="pill">'+prs.length+'</span>')+
 card("team","서브 에이전트 스레드",'<div class="card-b">'+(roots.length?'<ul class="tree">'+roots.map(tree).join("")+'</ul>':'<div class="empty">서브 에이전트가 없습니다</div>')+'</div>')+
 card("cpu","부하",'<div class="card-b sload"><div class="small muted">팀원별 오늘 토큰</div></div>'+load+'<div class="card-b sload"><div class="small muted">컴퓨터</div>'+comps+'</div>')+'</div></div>';
 tlRun.on=false; tlRun.t=null; tlPaint();
}

/* ---------- refresh ---------- */
function refreshSide(){var side=$(".side"),t=document.createElement("div"); t.innerHTML=shell("sessions",""); side.innerHTML=t.querySelector(".side").innerHTML}
function refreshConv(){ if(sessView==="multi") updatePane(cur.id); else if(sessView==="status") renderStatus(); else renderConv(); save(); refreshSide(); renderSTab() }
function refreshFor(s){var o=cur; cur=s; if(sessView!=="single"||o===s) refreshConv(); else {save(); renderSTab(); refreshSide()} cur=(sessView==="single"?o:s)}

/* ---------- web page and IDE menus (top bar, far right) ---------- */
var IDES=[["ember","Ember 에디터","Ember 자체 에디터 코어, 네이티브 창으로 엽니다",""],["vscode","VS Code","그 컴퓨터의 code serve-web을 Ember IDE 창에 웹 IDE로 엽니다","OSE"],["jetbrains","JetBrains Gateway","그 컴퓨터에 설치된 Gateway에 열기 명령을 보냅니다",""]];
function sIde(){var id=lsGet("ember-mock-ide","ember"); var x=IDES.filter(function(x){return x[0]===id})[0]; if(!x){lsSet("ember-mock-ide","ember"); x=IDES[0]} return x}
function sVsRt(){return lsGet("ember-mock-vsrt","ose")}
function sPorts(s){return ((SX.ports||{})[s.id]||[])}
function sTarget(){return (cur&&cur.project===sessP.id)?cur:sLeadFirst(members())[0]}
function webMenu(){
 var s=sTarget(), ip=sIp(s.computer), l=[];
 members().filter(function(m){return m.computer===s.computer}).forEach(function(m){sPorts(m).forEach(function(p){l.push([m,p])})});
 return '<div class="sdrop-h"><b>웹 페이지 열기</b><span class="small muted">'+esc(s.computer)+' · '+ip+'</span></div>'+
 (l.length?l.map(function(x){var u="http://"+ip+":"+x[1].port; return '<button class="sdrop-i" data-web="'+u+'">'+bot(x[0],22)+'<span class="col" style="min-width:0;flex:1"><span class="mono ellipsis">'+u+'</span><span class="small muted ellipsis">'+esc(x[1].proc)+(x[1].label?' · '+esc(x[1].label):"")+' · '+esc(mname(x[0]))+'</span></span><span class="pill green">감지됨</span></button>'}).join(""):'<div class="small muted sdrop-e">이 컴퓨터에서 열린 포트를 찾지 못했습니다</div>')+
 '<form class="sdrop-f" data-webform><span class="mono small muted">'+ip+':</span><input class="pane-in" name="port" inputmode="numeric" placeholder="포트" aria-label="포트 직접 입력"><button class="btn sm">열기</button></form><div class="small muted sdrop-n">같은 네트워크가 아니면 Ember 터널로 엽니다</div>'}
function ideMenu(){var on=sIde()[0], s=sTarget();
 return '<div class="sdrop-h"><b>어느 IDE로 열까요</b><span class="small muted">'+esc(s.computer)+':~/'+esc(proj(s.project).name)+'</span></div>'+IDES.map(function(x){return '<button class="sdrop-i'+(x[0]===on?" on":"")+'" data-ide="'+x[0]+'">'+I("code")+'<span class="col" style="min-width:0;flex:1"><b>'+x[1]+(x[0]==="vscode"?' <span class="pill sky">'+(sVsRt()==="ose"?"OSE":"공식 VS Code")+'</span>':"")+'</b><span class="small muted">'+x[2]+'</span></span>'+(x[0]===on?I("check"):"")+'</button>'}).join("")+
 '<div class="sdrop-f ide-rt"><span class="small muted spacer">VS Code 런타임</span><div class="mini-seg" role="group" aria-label="VS Code 런타임"><button type="button" data-vsrt="ose" class="'+(sVsRt()==="ose"?"on":"")+'">OSE</button><button type="button" data-vsrt="official" class="'+(sVsRt()==="official"?"on":"")+'">공식</button></div></div>'}
function sDropClose(){document.querySelectorAll(".sdrop-wrap.open").forEach(function(w){w.classList.remove("open")})}
function sDropOpen(w,html){var was=w.classList.contains("open"); sDropClose(); if(was) return; w.querySelector(".sdrop").innerHTML=html; w.classList.add("open")}
function headRight(){var ide=sIde();
 return '<div class="sdrop-wrap" data-wrap="web"><button class="btn sm" data-act="webmenu" aria-haspopup="true" title="웹 페이지 열기">'+I("globe")+'<span class="hide-xl">웹 페이지</span><svg class="i chev dn" viewBox="0 0 24 24">'+IC.chev+'</svg></button><div class="sdrop" role="menu"></div></div>'+
 '<div class="sdrop-wrap idesplit" data-wrap="ide"><button class="btn sm ide-main" data-act="ideopen" title="'+ide[1]+'로 열기">'+I("code")+'<span>IDE 열기</span><span class="ide-n hide-lg">'+ide[1]+'</span></button><button class="btn sm ide-chev" data-act="idemenu" aria-haspopup="true" title="IDE 고르기" aria-label="IDE 고르기"><svg class="i chev dn" viewBox="0 0 24 24">'+IC.chev+'</svg></button><div class="sdrop" role="menu"></div></div>'}

/* ---------- views ---------- */
function setView(v){sessView=v; lsSet("ember-mock-view:"+sessP.id,v); sDropClose();
 document.querySelectorAll("[data-view]").forEach(function(b){b.classList.toggle("on",b.dataset.view===v); b.setAttribute("aria-pressed",b.dataset.view===v)});
 $(".sess").dataset.sessview=v; if(v!=="single") lsSet("ember-mock-view-last:"+sessP.id,v);
 var bl=$(".sesshead .backlink"); if(bl){bl.href=v==="single"?"conversation.html#p:"+sessP.id:"project.html"; bl.title=v==="single"?sessP.name+" 세션으로":"프로젝트 목록으로"; bl.querySelector(".bl-t").textContent=v==="single"?sessP.name:"프로젝트"}
 if(v==="multi") renderMulti(); else if(v==="status") renderStatus(); else {if(!cur||cur.project!==sessP.id) cur=sLeadFirst(members())[0]; renderSingle(); if(matchMedia("(max-width:560px)").matches) document.body.classList.add("view-detail")}
 if(v!=="single") document.body.classList.remove("view-detail");
 renderSTab();
 history.replaceState(null,"",v==="single"&&cur?"#"+cur.id:"#p:"+sessP.id)}
function sLastView(){var l=lsGet("ember-mock-view-last:"+sessP.id,"multi"); return l==="single"?"multi":l}
function conversation(){
 var h=location.hash.slice(1), forced=null;
 if(h.indexOf("p:")===0){sessP=proj(h.slice(2))||D.projects[0]; cur=sLeadFirst(members())[0]}
 else {cur=sess(h)||sorted()[0]; sessP=proj(cur.project); if(sess(h)) forced="single"}
 if(cur&&cur.status==="unread") cur.status="done";
 lsSet("ember-mock-lastproj",sessP.id); stLoad();
 var ms=members(), wt=ms.filter(function(s){return s.status==="wait"}).length, rn=ms.filter(function(s){return s.status==="run"}).length;
 render(shell("sessions",'<div class="sess'+(lsGet("ember-mock-strail","0")==="1"?" srail":"")+'"><div class="sbody">'+stShell()+'<div class="smain"><header class="sesshead"><button class="icon-btn sm sdrawer-btn" data-act="sdrawer" title="세션 탭" aria-label="세션 탭 열기">'+I("sidebar-open")+'</button>'+
 '<div class="col" style="min-width:0"><label class="projsw"><select data-projsw aria-label="프로젝트 전환">'+D.projects.map(function(p){return '<option value="'+p.id+'"'+(p.id===sessP.id?" selected":"")+'>'+p.name+'</option>'}).join("")+'</select>'+I("chev")+'</label><span class="small muted ellipsis">세션 · 팀원 '+ms.length+(wt?' · 승인 대기 '+wt:"")+(rn?' · 실행 중 '+rn:"")+'</span></div><div class="spacer"></div>'+
 '<div class="seg viewseg" role="group" aria-label="보기 전환"><button data-view="multi">'+I("grid")+'<span class="lbl">멀티</span></button><button data-view="single">'+I("user")+'<span class="lbl">싱글</span></button><button data-view="status">'+I("chart")+'<span class="lbl">상태</span></button></div>'+
 '<button class="btn sm newbtn" data-act="newin" data-pid="'+sessP.id+'" title="새 팀원" aria-label="새 팀원">'+I("plus")+'<span class="hide-xl">새 팀원</span></button><button class="btn sm vcsbtn" data-act="vcsopen" title="버전 관리" aria-label="버전 관리">'+I("git")+'<span class="hide-xl">버전 관리</span></button>'+headRight()+'</header>'+
 '<main class="sessbody" id="sb"></main></div></div></div>'));
 renderSTab();
 $("#stq").addEventListener("input",function(){stQ=this.value; renderSTab()});
 setView(forced||lsGet("ember-mock-view:"+sessP.id,"multi"));
 save();
 if(cur&&cur.id[0]==="n"&&cur.status==="run"&&cur.msgs.length&&cur.msgs[cur.msgs.length-1].k==="working") finishTurn(cur,"기본 프로젝트에서 시작했습니다. 요청을 확인했고 이어서 진행할 수 있습니다.");
}
document.addEventListener("change",function(e){if(e.target.matches&&e.target.matches("[data-projsw]")){location.hash="p:"+e.target.value}});
document.addEventListener("submit",function(e){if(e.target.matches("[data-webform]")){e.preventDefault(); var p=+e.target.port.value; if(!(p>0&&p<65536)){toast("포트 번호를 입력하세요"); return} toast("새 탭에서 엽니다: http://"+sIp(sTarget().computer)+":"+p+" (데모)"); sDropClose()}});
/* tab rename: double-click a tab, type, Enter */
document.addEventListener("dblclick",function(e){var tb=e.target.closest&&e.target.closest(".stab"); if(!tb||e.target.closest("button")) return; var t=STABS.tabs.filter(function(x){return x.id===tb.dataset.stab})[0], n=tb.querySelector(".stab-n");
 n.innerHTML='<input class="stab-in" value="'+esc(t.name)+'" aria-label="탭 이름">'; var inp=n.firstChild; inp.focus(); inp.select();
 function done(){t.name=inp.value.trim()||t.name; stSave(); renderMulti()} inp.addEventListener("blur",done); inp.addEventListener("keydown",function(k){if(k.key==="Enter"){k.preventDefault(); inp.blur()} if(k.key==="Escape"){inp.value=t.name; inp.blur()}})});
document.addEventListener("keydown",function(e){var tb=e.target.closest&&e.target.closest(".stab"); if(tb&&!e.target.closest("input")&&(e.key==="Enter"||e.key===" ")){e.preventDefault(); STABS.on=tb.dataset.stab; stSave(); renderMulti()}});

/* model chip (shared Ember.modelPicker): harness, then model and effort; updates the member in demo state */
function sModeName(s){var M=D.modes||{}, id=s.mode||((M.def||{})[s.agent])||"ask"; return ((M.list||{})[id]||{name:"명령마다 확인"}).name}
function sModeChip(s){return Ember.modePicker?Ember.modePicker.html({harness:s.agent,mode:s.mode,name:"md_"+s.id}):'<span class="pickchip">'+esc(sModeName(s))+'</span>'}
document.addEventListener("modechange",function(e){var w=e.target.closest&&e.target.closest("[data-msid]"); if(!w) return; var s=sess(w.dataset.msid), v=e.detail; if(!s||!v) return; s.mode=v.mode; save(); if(sessView==="single") renderRPanel()});
function sProfName(h){var p=Ember.profileOf?Ember.profileOf(h,sessP.id):null; return p?p.name:""}
function sPicker(s){return Ember.modelPicker?Ember.modelPicker.html({harness:s.agent,model:s.model,project:s.project,name:"m_"+s.id}):'<span class="chip">'+esc(s.agent+" · "+s.model)+'</span>'}
var EFN={low:"Low",medium:"Medium",high:"High"};
function sModelText(v){return v.modelName+(v.effort?" · "+EFN[v.effort]:"")}
document.addEventListener("modelchange",function(e){var nf=e.target.closest&&e.target.closest("[data-snew]"); if(nf&&e.detail){var pe=nf.querySelector(".snew-prof"); if(pe) pe.textContent="프로필 "+(sProfName(e.detail.harness)||"기본"); return}
 var w=e.target.closest&&e.target.closest("[data-msid]"); if(!w) return; var s=sess(w.dataset.msid), v=e.detail; if(!s||!v) return;
 var hv=s.agent!==v.harness; s.agent=v.harness; s.model=sModelText(v); save(); renderSTab();
 if(hv) document.querySelectorAll('.pane[data-sid="'+s.id+'"] .pane-h .bot, .detail .crumb-bot .bot').forEach(function(b){var t=document.createElement("span"); t.innerHTML=bot(s,b.offsetWidth||24); b.replaceWith(t.firstChild)})});
/* new member: name, harness and model, computer, account */
function snewOpen(){
 var p=sessP, accs=(D.accounts||[]).map(function(a){return a.id+" · "+a.tier+" ("+a.name+")"});
 sheet('<form class="snew" data-snew><div class="sheet-h"><b class="sheet-t">새 팀원</b><span class="muted ellipsis">'+esc(p.name)+'</span><div class="spacer"></div><button type="button" class="icon-btn sm" data-act="closemodal" title="닫기" aria-label="닫기">'+I("close")+'</button></div>'+
 '<div class="snew-b"><label class="snew-f"><span>역할 이름</span><input class="field" name="role" placeholder="예: 구현, 리뷰, 문서" required></label>'+
 '<label class="snew-f"><span>첫 지시 (선택)</span><textarea class="field" name="text" placeholder="이 팀원이 할 일을 적으세요"></textarea></label>'+
 '<div class="snew-f"><span>하네스, 모델, 권한 모드</span><div class="snew-picks">'+(Ember.modelPicker?Ember.modelPicker.html({harness:"Claude Code",project:p.id,name:"n_"}):"")+(Ember.modePicker?Ember.modePicker.html({harness:"Claude Code",name:"n_"}):"")+'</div><span class="small muted snew-prof">프로필 '+esc(sProfName("Claude Code")||"기본")+'</span></div>'+
 '<div class="snew-row"><label class="snew-f"><span>컴퓨터</span><select class="field" name="computer">'+p.computers.map(function(c){return '<option>'+c+'</option>'}).join("")+'</select></label>'+
 '</div></div>'+
 '<div class="snew-foot"><div class="spacer"></div><button type="button" class="btn sm" data-act="closemodal">취소</button><button class="btn primary sm">만들기</button></div></form>',"snewm");
 var f=$(".snewm [name=role]"); if(f) f.focus()}
document.addEventListener("submit",function(e){if(!e.target.matches("[data-snew]")) return; e.preventDefault(); var f=e.target, v=Ember.modelPicker?Ember.modelPicker.value(f):{harness:"Claude Code",modelName:"Opus",effort:"high"};
 var role=f.role.value.trim(); if(!role){toast("역할 이름을 입력하세요"); return} var text=f.text.value.trim();
 var s={id:"n"+Date.now(),title:text?text.slice(0,40):"새 세션",role:role,project:sessP.id,agent:v.harness,model:sModelText(v),computer:f.computer.value,account:(Ember.profileOf&&Ember.profileOf(v.harness,sessP.id)||{}).account||"ncmnd · Max",mode:(Ember.modePicker?Ember.modePicker.value(f).mode:"ask"),status:"done",t:maxT(),msgs:[]};
 if(text){s.msgs.push({k:"user",text:text},{k:"working",text:"작업 중"}); s.status="run"}
 D.sessions.push(s); save(); closeSheet("snewm"); toast(mname(s)+" 팀원을 만들었습니다"); location.hash=s.id});
/* version control popup: the vcs screen mounted in a large sheet for this project */
function vcsOpen(){
 var m=document.createElement("div"); m.className="modal svcs"; m.innerHTML='<div class="svcs-box" role="dialog" aria-modal="true" aria-label="버전 관리"><div class="svcs-b" id="svcsb"></div></div>';
 document.body.appendChild(m); var el=$("#svcsb");
 if(Ember.vcs&&Ember.vcs.mount) Ember.vcs.mount(el,sessP.id,{compact:true,onClose:function(){vcsClose(m)}});
 else el.innerHTML='<div class="empty">버전 관리 화면을 불러오지 못했습니다 (Ember.vcs 없음)<br><button class="btn sm" data-act="vcsclose">닫기</button></div>'}
function vcsClose(m){m=m||$(".modal.svcs"); if(!m) return; var el=m.querySelector("#svcsb"); if(el&&Ember.vcs&&Ember.vcs.unmount) Ember.vcs.unmount(el); m.remove()}
Ember.screens.conversation={render:conversation,onHash:conversation,onModalClose:function(m){if(m.classList.contains("svcs")){var el=m.querySelector("#svcsb"); if(el&&Ember.vcs&&Ember.vcs.unmount) Ember.vcs.unmount(el)}},click:function(e,t){var a;
 if(Date.now()-sDragEnd<250) return true;
 if(!t.closest(".sdrop-wrap")) sDropClose();
 var sidEl=t.closest&&t.closest("[data-sid]"); if(sidEl&&sess(sidEl.dataset.sid)) cur=sess(sidEl.dataset.sid);
 if((a=t.closest("button[data-view]"))){setView(a.dataset.view); return true}
 if((a=t.closest("[data-back]"))&&sessView==="single"){document.body.classList.remove("view-detail"); setView(sLastView()); return true}
 if((a=t.closest("[data-act=backsess]"))&&sessView==="single"){e.preventDefault(); setView(sLastView()); return true}
 if((a=t.closest("[data-stsec]"))){var pid=a.dataset.stsec; stClosed[pid]=!stClosed[pid]; lsSet("ember-mock-stclosed",JSON.stringify(stClosed)); renderSTab(); return true}
 if((a=t.closest("[data-open]"))){var s=sess(a.dataset.open); $(".sess").classList.remove("sdrawer-open"); if(!s) return true;
  if(s.project!==sessP.id){location.hash=s.id; return true}
  cur=s; if(cur.status==="unread") cur.status="done"; setView("single"); save(); return true}
 if((a=t.closest("[data-stabx]"))){var id=a.dataset.stabx; STABS.tabs=STABS.tabs.filter(function(x){return x.id!==id}); if(STABS.on===id) STABS.on=STABS.tabs[0].id; stSave(); renderMulti(); return true}
 if((a=t.closest("[data-stab]"))){STABS.on=a.dataset.stab; stSave(); renderMulti(); return true}
 if((a=t.closest("[data-padd]"))){curTab().rows=[[a.dataset.padd]]; curTab().sz=null; stSave(); renderMulti(); return true}
 if((a=t.closest("[data-pmode]"))){paneMode[cur.id]=a.dataset.pmode; updatePane(cur.id); return true}
 if((a=t.closest("[data-rpsec]"))){var k="ember-mock-rp:"+a.dataset.rpsec, off=lsGet(k,"1")!=="0"; lsSet(k,off?"0":"1"); a.parentNode.classList.toggle("off",off); a.setAttribute("aria-expanded",!off); return true}
 if((a=t.closest("[data-tl]"))){tlPlay(a.dataset.tl==="restart"); return true}
 if((a=t.closest("[data-tlx]"))){tlRun.x=+a.dataset.tlx; a.parentNode.querySelectorAll("button").forEach(function(b){b.classList.toggle("on",b===a)}); return true}
 if((a=t.closest("[data-web]"))){toast("새 탭에서 엽니다: "+a.dataset.web+" (데모)"); sDropClose(); return true}
 if((a=t.closest("[data-vsrt]"))){lsSet("ember-mock-vsrt",a.dataset.vsrt); a.closest(".sdrop").innerHTML=ideMenu(); return true}
 if((a=t.closest("[data-ide]"))){lsSet("ember-mock-ide",a.dataset.ide); var ide=sIde(), w=$(".idesplit"); w.querySelector(".ide-n").textContent=ide[1]; w.querySelector(".ide-main").title=ide[1]+"로 열기"; sDropClose(); toast(ide[1]+"로 엽니다: "+sTarget().computer+":~/"+proj(sTarget().project).name+" (데모)"); return true}
 if((a=t.closest("[data-approve]"))){var m=cur.msgs[+a.dataset.i], how=a.dataset.approve; m.state=how;
  if(how==="deny"){cur.msgs.push({k:"agent",text:"명령을 실행하지 않았습니다. 다른 방법이 필요하면 알려 주세요."}); cur.status="done"}
  else{var c={k:"cmds",running:true,items:[{cmd:m.cmd,out:"running 3 tests\ntest shim::health ... ok\ntest shim::routes ... ok\ntest shim::auth ... ok\n\ntest result: ok. 3 passed"}]}; cur.msgs.push(c); cur.status="run";
   var s2=cur; setTimeout(function(){c.running=false; s2.msgs.push({k:"agent",text:"통합 테스트 3개가 모두 통과했습니다. /shim/health가 200을 돌려줍니다."}); s2.status="done"; refreshFor(s2)},1500)}
  refreshConv(); toast(how==="deny"?"거부했습니다":"승인했습니다"); return true}
 if((a=t.closest("[data-act]"))){var act=a.dataset.act;
  if(act==="newin"){snewOpen(); return true}
  if(act==="focus"){setView("single"); return true}
  if(act==="vcsopen"){vcsOpen(); return true}
  if(act==="vcsclose"){vcsClose(); return true}
  if(act==="pclose"){var tb=curTab(); tb.rows=tb.rows.map(function(r){return r.filter(function(id){return id!==cur.id})}).filter(function(r){return r.length}); tb.sz=null; stSave(); renderMulti(); return true}
  if(act==="stabadd"){var n=STABS.n=(STABS.n||STABS.tabs.length)+1; STABS.tabs.push({id:"t"+Date.now(),name:"탭 "+n,rows:[]}); STABS.on=STABS.tabs[STABS.tabs.length-1].id; stSave(); renderMulti(); return true}
  if(act==="strail"){var sd=$(".sess"); if(matchMedia("(max-width:900px)").matches){sd.classList.toggle("sdrawer-open"); return true} var on=!sd.classList.contains("srail"); sd.classList.toggle("srail",on); lsSet("ember-mock-strail",on?"1":"0"); return true}
  if(act==="sdrawer"){var sd2=$(".sess"); if(matchMedia("(max-width:900px)").matches) sd2.classList.toggle("sdrawer-open"); else {sd2.classList.remove("srail"); lsSet("ember-mock-strail","0")} return true}
  if(act==="rptoggle"){var g=$(".sgl"); if(matchMedia("(min-width:1280px)").matches){var off2=!g.classList.contains("rp-off"); g.classList.toggle("rp-off",off2); lsSet("ember-mock-rpoff",off2?"1":"0")} else g.classList.toggle("rp-open"); return true}
  if(act==="mkpr"){var num=160+Math.floor(Math.random()*30); lsSet("ember-mock-pr:"+cur.id,String(num)); renderRPanel(); toast("PR #"+num+"을 develop으로 열었습니다 (데모)"); return true}
  if(act==="webmenu"){sDropOpen(a.closest(".sdrop-wrap"),webMenu()); return true}
  if(act==="idemenu"){sDropOpen(a.closest(".sdrop-wrap"),ideMenu()); return true}
  if(act==="ideopen"){var i2=sIde(); toast(i2[1]+"로 엽니다: "+sTarget().computer+":~/"+proj(sTarget().project).name+" (데모)"); return true}
  if(act==="psend"){var pi=a.closest(".pane").querySelector("[data-pin]"), pv=pi.value; pi.value=""; sendConv(pv); return true}
  if(act==="send"){sendConv(); return true}
  if(act==="stop"){cur.msgs=cur.msgs.filter(function(m){return m.k!=="working"}); cur.msgs.push({k:"sys",text:"사용자가 이번 턴을 중단했습니다."}); cur.status="done"; refreshConv(); return true}
 }
}};
