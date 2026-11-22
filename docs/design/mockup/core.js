/* Ember mockup core: state, storage, theme, icons, shell, shared helpers, click dispatcher, boot.
   Screen files register themselves in Ember.screens[name] = {render, click, onHash, onModalClose}.
   Top-level functions here are globals shared by all screen scripts and are also listed on window.Ember. */
"use strict";
var root=document.documentElement, THEME="ember-mock-theme", KEY="ember-mock-state-v7";
/* theme preference: system (default, follows prefers-color-scheme live), light or dark.
   data-theme always resolves to light or dark; data-theme-pref keeps the choice. Old stored light/dark stay explicit. */
var THEME_MQ=window.matchMedia?matchMedia("(prefers-color-scheme: dark)"):{matches:false};
function themePref(){try{var v=localStorage.getItem(THEME); return v==="light"||v==="dark"||v==="system"?v:"system"}catch(e){return "system"}}
function applyTheme(){var p=themePref(); root.dataset.themePref=p; root.dataset.theme=p==="system"?(THEME_MQ.matches?"dark":"light"):p}
applyTheme();
if(THEME_MQ.addEventListener) THEME_MQ.addEventListener("change",function(){if(themePref()==="system"){applyTheme(); updateThemeButtons()}});
var THEME_UI={system:["pc","시스템"],light:["sun","라이트"],dark:["moon","다크"]};
function updateThemeButtons(){var u=THEME_UI[themePref()], l="테마: "+u[1];
 document.querySelectorAll('[data-act="theme"]').forEach(function(b){var ic=I(u[0]);
  if(b.classList.contains("tool")) b.innerHTML=ic+'<span class="lbl">'+l+'</span>'; else if(b.classList.contains("icon-btn")) b.innerHTML=ic; else b.innerHTML=ic+l;
  b.title=l+" (눌러서 바꾸기: 시스템, 라이트, 다크)"; b.setAttribute("aria-label",l+", 눌러서 다음으로")})}
/* platform preview: iOS gets Liquid Glass on floating bars (owner exception), everything else stays unified */
var PLAT="ember-mock-platform";
function detectPlat(){var u=navigator.userAgent||""; return (/iPhone|iPad|iPod/.test(u)||(/Macintosh/.test(u)&&navigator.maxTouchPoints>1))?"ios":"other"}
try{root.dataset.platform=localStorage.getItem(PLAT)||detectPlat()}catch(e){root.dataset.platform=detectPlat()}
function platLabel(){return root.dataset.platform==="ios"?"iOS":"기타"}
function togglePlatform(){var n=root.dataset.platform==="ios"?"other":"ios"; root.dataset.platform=n; try{localStorage.setItem(PLAT,n)}catch(e){} document.querySelectorAll('[data-act="platform"]').forEach(function(b){var l=b.querySelector(".lbl"); if(l) l.textContent="플랫폼: "+platLabel(); else b.lastChild.textContent="플랫폼: "+platLabel(); b.setAttribute("aria-label","플랫폼 미리보기: "+platLabel())}); toast("플랫폼 미리보기: "+platLabel())}
function toggleTheme(){var o=["system","light","dark"], n=o[(o.indexOf(themePref())+1)%3]; try{localStorage.setItem(THEME,n)}catch(e){} applyTheme(); updateThemeButtons(); toast("테마: "+THEME_UI[n][1])}

var D; try{D=JSON.parse(localStorage.getItem(KEY)||"null")}catch(e){}
if(!D) D=JSON.parse(JSON.stringify(window.EMBER_DEMO));
/* v10: the default project (id kept as "unsorted") is named 기본 프로젝트 and has no leader */
(D.projects||[]).forEach(function(p){if(p.id==="unsorted"){if(p.name==="Unsorted") p.name="기본 프로젝트"; if(p.short==="unsorted") p.short="기본"; if(/Home에서/.test(p.repo||"")) p.repo="홈에서 시작한 대화"}});
(D.sessions||[]).forEach(function(s){if(s.project==="unsorted"&&s.lead) delete s.lead});
/* leader flags added after v7 data was saved */
(window.EMBER_DEMO.sessions||[]).forEach(function(ds){if(ds.lead){var s=(D.sessions||[]).filter(function(x){return x.id===ds.id})[0]; if(s&&s.lead===undefined) s.lead=true}});
/* data blocks appended later (e.g. EMBER_DEMO.vcs, EMBER_DEMO.integrations) reach saved state too */
Object.keys(window.EMBER_DEMO).forEach(function(k){if(!(k in D)) D[k]=JSON.parse(JSON.stringify(window.EMBER_DEMO[k]))});
/* team character style per project (v10 리더 결정): saved projects without one get 몽글 */
(D.projects||[]).forEach(function(p){if(p.characterStyle==="mallang") p.characterStyle="malang"; if(!p.characterStyle) p.characterStyle=p.charStyle||"mongle"});
function save(){try{localStorage.setItem(KEY,JSON.stringify(D))}catch(e){}}
/* demo reset clears saved state and per-project UI keys (session tabs and layouts, views, last project, IDE choice stays) */
function resetDemo(){try{localStorage.removeItem(KEY); var ks=[]; for(var i=0;i<localStorage.length;i++){var k=localStorage.key(i); if(/^ember-mock-(stabs|view|view-last|layout):/.test(k)||k==="ember-mock-lastproj") ks.push(k)} ks.forEach(function(k){localStorage.removeItem(k)})}catch(e){} location.reload()}

var IC=window.EMBER_ICONS;
function I(n){return '<svg class="i" viewBox="0 0 24 24">'+IC[n]+'</svg>'}
function esc(s){return String(s).replace(/[&<>"]/g,function(c){return {"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]})}
function $(s,el){return (el||document).querySelector(s)}
function nowTime(){var d=new Date(),h=d.getHours(),m=("0"+d.getMinutes()).slice(-2);return (h<12?"오전 ":"오후 ")+((h%12)||12)+":"+m}
function toast(t){var el=$(".toast"); if(!el){el=document.createElement("div");el.className="toast";document.body.appendChild(el)} el.textContent=t; el.classList.add("show"); clearTimeout(el._t); el._t=setTimeout(function(){el.classList.remove("show")},1800)}
function proj(id){return D.projects.filter(function(p){return p.id===id})[0]}
function sess(id){return D.sessions.filter(function(s){return s.id===id})[0]}
function maxT(){return Math.max.apply(null,D.sessions.map(function(s){return s.t}).concat(D.rooms.map(function(r){return r.t})))+1}
var ST={run:["실행 중","blue","run"],wait:["승인 대기","orange","wait"],unread:["새 결과","sky","run"],done:["완료","green","ok"],fail:["실패","red","fail"]};
function pill(st){var s=ST[st];return '<span class="pill '+s[1]+'">'+s[0]+'</span>'}
function sdot(st){return st==="run"?'<span class="spin"></span>':'<span class="dot '+ST[st][2]+'"></span>'}
function av(name,cls){var p=D.people[name]||{av:name[0],cls:"p2"}; if(p.agent) return bot(name,"done",/stack/.test(cls||"")?20:(/sm/.test(cls||"")?28:36),name);return '<div class="av '+(cls||"")+' '+p.cls+'">'+p.av+'</div>'}
var VC={claude:"var(--v-claude)",codex:"var(--v-codex)",gemini:"var(--v-gemini)"};
function vendorOf(agent){return agent.indexOf("Codex")>=0?"codex":agent.indexOf("Gemini")>=0?"gemini":"claude"}

/* ---------- agent characters: small animated beings, one shape per harness ---------- */
var BOTSHAPE={
 claude:'M20 6.5c8.2 0 13.8 5.6 13.8 14.2S28.4 34.5 20 34.5 6.2 29.3 6.2 20.7 11.8 6.5 20 6.5z',
 codex:'M13 8h14c3.9 0 6 2.1 6 6v13c0 3.9-2.1 6-6 6H13c-3.9 0-6-2.1-6-6V14c0-3.9 2.1-6 6-6z',
 gemini:'M20 5.5c2.2 6.4 6.3 10.6 13.5 13.6-7.2 3-11.3 7.4-13.5 14.4-2.2-7-6.3-11.4-13.5-14.4C13.7 16.1 17.8 11.9 20 5.5z'};
var MOUTH={done:'M16.5 24.5q3.5 3.2 7 0',unread:'M16.5 24.5q3.5 3.2 7 0',run:'M17 25.5h6',wait:'M18 25a2 2 0 0 0 4 0',fail:'M16.5 27q3.5-3 7 0'};
/* bot(member, size) shows a team member (adds the leader badge when member.lead);
   bot(agentOrVendor, status, size, label, lead) draws a plain character. */
function bot(agent,status,size,label,lead){
 if(agent&&typeof agent==="object"){var m=agent;
  /* team member: draw in the project's team character style (chars.js); without it, fall back to the drawing below */
  if(window.EmberChar&&EmberChar.draw){var lead=!!m.lead&&m.project!=="unsorted", nm=mname(m);
   return EmberChar.draw({name:nm,agent:m.agent,role:m.role,status:m.status,lead:lead},{style:Ember.charStyle(m.project),size:typeof status==="number"?status:36,lead:lead,label:nm})}
  return bot(m.agent,m.status,typeof status==="number"?status:36,mname(m)+(m.lead?" (리더)":""),!!m.lead)}
 var v=VC[agent]?agent:vendorOf(agent); status=status||"done"; size=size||36;
 var c=VC[v];
 return '<span class="bot '+v+' st-'+status+'" style="width:'+size+'px;height:'+size+'px" role="img" aria-label="'+esc(label||(v+" "+(ST[status]?ST[status][0]:"")))+'"><svg viewBox="0 0 40 40" aria-hidden="true">'+
 '<ellipse class="shadow" cx="20" cy="37.6" rx="9" ry="1.5"/>'+
 '<g class="b"><path class="body" d="'+BOTSHAPE[v]+'" fill="'+c+'"/>'+
 '<path class="cheek" d="M11.5 23.5h2.5M26 23.5h2.5"/>'+
 '<g class="eyes"><ellipse cx="15.5" cy="19" rx="1.9" ry="2.5"/><ellipse cx="24.5" cy="19" rx="1.9" ry="2.5"/></g>'+
 '<path class="mouth" d="'+(MOUTH[status]||MOUTH.done)+'"/></g>'+
 (status==="wait"?'<g class="hand"><circle cx="35" cy="15" r="3" fill="'+c+'"/></g>':"")+
 (status==="run"?'<g class="busy"><circle cx="31" cy="7" r="1.5"/><circle cx="35" cy="5" r="1.2"/></g>':"")+
 (status==="fail"?'<path class="drop" d="M31 12c1.5 2.2 2.3 3.5 2.3 4.6a2.3 2.3 0 0 1-4.6 0c0-1.1.8-2.4 2.3-4.6z"/>':"")+
 '</svg>'+(lead?'<span class="leadmark" title="리더 (오케스트레이터)"><svg viewBox="0 0 12 12" aria-hidden="true"><path d="M1.5 4.2 4 6.2 6 2.5l2 3.7 2.5-2-0.9 5.3H2.4z"/></svg></span>':"")+'</span>'}
function mname(s){var p=proj(s.project);return (p?p.short:"")+"-"+(s.role||"팀원")}
function lastProj(){return lsGet("ember-mock-lastproj","ember")}

/* ---------- shell ---------- */
var SIDE="ember-mock-sidebar";
function sideCollapsed(){try{return localStorage.getItem(SIDE)==="collapsed"}catch(e){return false}}
function toggleSide(){var app=$(".app"); if(matchMedia("(max-width:1279px)").matches){app.classList.toggle("overlay");return}
 var c=!app.classList.contains("collapsed"); app.classList.toggle("collapsed",c); try{localStorage.setItem(SIDE,c?"collapsed":"expanded")}catch(e){}}
/* IDE choices (v11, INTENT D9): Ember editor (native editor core), VS Code (that computer's code serve-web wrapped in the Ember IDE window,
   runtime OSE by default or official VS Code), JetBrains Gateway (a command to the Gateway installed on that computer). Old cursor/zed fall back to ember. */
var IDEN={ember:["Ember 에디터","Ember 자체 에디터 코어, 네이티브 창"],vscode:["VS Code","그 컴퓨터의 code serve-web을 Ember IDE 창에서 웹으로 엽니다, 런타임 OSE"],jetbrains:["JetBrains Gateway","그 컴퓨터에 설치된 Gateway에 열기 명령을 보냅니다"]};
function ideChoice(){var k=lsGet("ember-mock-ide","ember"); if(!IDEN[k]){k="ember"; lsSet("ember-mock-ide",k)} return k}
/* brand: the one place for the sidebar mark and the favicon. The logo swap (REQUESTS logo:) changes only these two values. */
var BRAND={mark:'<img class="logo logo-img" src="logo/phoenix-fold/mark-sm.svg" width="32" height="32" alt="Ember">',
 favicon:"logo/phoenix-fold/favicon.svg"};
(function(){var l=document.querySelector('link[rel="icon"]'); if(!l){l=document.createElement("link"); l.rel="icon"; document.head.appendChild(l)} l.href=BRAND.favicon})();
function shell(active,inner){
 var unreadRooms=D.rooms.reduce(function(a,r){return a+r.unread},0);
 var waiting=D.sessions.filter(function(s){return s.status==="wait"}).length;
 var MORE=["routines","vcs","agents","files"];
 function nav(k,icon,label,href,extra){return '<a class="nav tab'+(MORE.indexOf(k)>=0?" more-item":"")+(k===active?" on":"")+'" href="'+href+'" title="'+label+'"'+(k===active?' aria-current="page"':"")+'>'+I(icon)+'<span class="lbl">'+label+'</span>'+(extra||"")+'</a>'}
 return '<div class="app'+(sideCollapsed()?" collapsed":"")+'"><aside class="side">'+
  '<div class="brand"><button class="logobtn" data-act="side" title="사이드바 접기/펼치기" aria-label="사이드바 접기/펼치기">'+BRAND.mark+'<span class="lg-ico i-close">'+I("sidebar-close")+'</span><span class="lg-ico i-open">'+I("sidebar-open")+'</span></button><div><b>Ember</b><small>darkpyonix</small></div></div>'+
  nav("home","home","홈","home.html")+
  nav("project","folder","프로젝트","project.html",'<span class="count">'+D.projects.length+'</span>')+
  nav("sessions","grid","세션","conversation.html#p:"+lastProj(),waiting?'<span class="redbadge" title="승인 대기">'+waiting+'</span>':"")+
  nav("channels","hash","채널","channels.html",unreadRooms?'<span class="redbadge">'+unreadRooms+'</span>':"")+
  nav("routines","repeat","루틴","routines.html",'<span class="count">'+(D.routines||[]).filter(function(r){return r.enabled}).length+'</span>')+
  nav("vcs","git","버전 관리","vcs.html")+
  nav("agents","cpu","에이전트","agents.html")+
  nav("files","arrows-right-left","파일","files.html",(function(){var n=((D.files||window.EMBER_DEMO.files||{}).transfers||[]).filter(function(x){return x.state==="run"}).length; return n?'<span class="count">'+n+'</span>':""})())+
  '<button class="nav tab morebtn'+(MORE.indexOf(active)>=0?" on":"")+'" data-act="moretabs" title="더보기" aria-label="더보기: 루틴, 버전 관리, 에이전트, 파일" aria-haspopup="dialog">'+I("more")+'<span class="lbl">더보기</span></button>'+
  '<div class="spacer"></div>'+
  '<div class="sidetools" role="group" aria-label="도구">'+
  '<button class="tool" data-act="theme" title="테마 전환" aria-label="테마: '+THEME_UI[themePref()][1]+', 눌러서 다음으로">'+I(THEME_UI[themePref()][0])+'<span class="lbl">테마: '+THEME_UI[themePref()][1]+'</span></button>'+
  '<button class="tool" data-act="settings" title="설정" aria-label="설정">'+I("gear")+'<span class="lbl">설정</span></button>'+
  '<button class="tool" data-act="platform" title="플랫폼 미리보기 전환" aria-label="플랫폼 미리보기: '+platLabel()+'">'+I("cpu")+'<span class="lbl">플랫폼: '+platLabel()+'</span></button>'+
  '<button class="tool" data-act="reset" title="데모 초기화" aria-label="데모 초기화">'+I("refresh")+'<span class="lbl">데모 초기화</span></button></div>'+
  '<div class="me">'+av("BREW","round sm")+'<div><b style="font-size:13px">BREW</b><div class="small muted">ncmnd</div></div></div>'+
  '</aside>'+inner+
  '<div class="phonemenu"><button class="icon-btn sm" data-act="pmenu" title="메뉴" aria-label="도구 메뉴" aria-haspopup="true">'+I("menu")+'</button><div class="pmenu-pop" role="menu"><button data-act="theme" role="menuitem">'+I("moon")+'테마 전환</button><button data-act="settings" role="menuitem">'+I("gear")+'설정</button><button data-act="platform" role="menuitem">'+I("cpu")+'플랫폼: '+platLabel()+'</button><button data-act="reset" role="menuitem">'+I("refresh")+'데모 초기화</button></div></div></div>'
}
/* head(title, sub, searchId, extra, opts): opts.primary === false hides the "새 팀원" button,
   a string replaces it with that HTML. */
function head(h1,sub,searchId,extra,opts){ opts=opts||{};
 return '<div class="head"><div><h1>'+h1+'</h1><div class="sub">'+sub+'</div></div><div class="spacer"></div>'+
  (searchId?'<button class="icon-btn searchbtn" data-act="search" title="검색" aria-label="검색">'+I("search")+'</button><label class="searchbox">'+I("search")+'<input id="'+searchId+'" placeholder="팀원 검색" aria-label="팀원 검색"><span class="kbd">/</span></label>':"")+(extra||"")+
  '<button class="icon-btn" data-act="theme" title="테마 전환" aria-label="테마 전환">'+I("moon")+'</button>'+
  (opts.primary===false?"":(typeof opts.primary==="string"?opts.primary:'<button class="btn primary" data-act="new" title="새 팀원" aria-label="새 팀원">'+I("plus")+'<span class="lbl">새 팀원</span></button>'))+'</div>'
}
function card(icon,title,body,right,id){return '<div class="card"'+(id?' id="'+id+'"':"")+'><div class="card-h"><div class="ico">'+I(icon)+'</div>'+title+'<div class="spacer"></div>'+(right||"")+'</div>'+body+'</div>'}
function sessRows(list){
 if(!list.length) return '<div class="empty">일치하는 팀원이 없습니다</div>';
 return list.map(function(s){var p=proj(s.project);return '<div class="list-row" data-href="conversation.html#'+s.id+'"><div class="tile">'+sdot(s.status)+'</div><div class="spacer col"><span class="t ellipsis">'+esc(s.title)+'</span><span class="s ellipsis">'+esc(p.name)+' · '+s.agent+' · '+s.computer+'</span></div><div class="right">'+pill(s.status)+'</div></div>'}).join("")
}
function compRows(list){
 return list.map(function(c){var st={online:["온라인","green","ok"],offline:["오프라인","",'off'],unknown:["알 수 없음","",""]}[c.status];
  var ps=D.projects.filter(function(p){return p.computers.indexOf(c.id)>=0}).map(function(p){return p.name}).join(", ")||"할당 없음";
  return '<div class="list-row" data-act="computer" data-id="'+c.id+'"><div class="tile">'+I("pc")+'</div><div class="spacer col"><span class="t">'+c.id+'</span><span class="s ellipsis">'+c.os+' · '+c.agents+(c.seen?' · '+c.seen+' 접속':"")+'</span></div><div class="right"><span class="pill '+st[1]+'"><span class="dot '+st[2]+'"></span>'+st[0]+'</span><span class="small muted hide-md">'+ps+'</span></div></div>'}).join("")
}
function sorted(){return D.sessions.slice().sort(function(a,b){return b.t-a.t})}
function newSession(projectId,text,agent,comp){
 var s={id:"n"+Date.now(),title:text?text.slice(0,40):"새 세션",project:projectId,agent:agent||"Claude Code",model:"claude-opus · High",computer:comp||proj(projectId).computers[0]||"mac-mini",account:"ncmnd · Max",status:"done",t:maxT(),msgs:[]};
 D.sessions.push(s); if(text) s.msgs.push({k:"user",text:text},{k:"working",text:"작업 중"}),s.status="run"; save(); location.href="conversation.html#"+s.id;
}

/* ---------- shared team, channel and sheet helpers (used by more than one screen) ---------- */
function activity(s){var m=s.msgs.slice().reverse().filter(function(x){return x.k==="agent"||x.k==="approval"||x.k==="user"})[0]; if(!m) return "아직 활동 없음"; return m.k==="approval"?"승인 요청: "+m.cmd:m.text}
function member(s){
 return '<div class="member" data-href="conversation.html#'+s.id+'">'+bot(s,36)+'<div class="spacer col"><div class="flex"><span class="t ellipsis">'+esc(mname(s))+'</span>'+pill(s.status)+'</div><span class="s ellipsis">'+esc(s.title)+' · '+s.agent+' · '+s.computer+'</span><span class="act ellipsis">'+esc(activity(s))+'</span></div></div>'}
function mtile(s){return '<a class="mtile st-'+s.status+'" href="conversation.html#'+s.id+'" title="'+esc(s.title)+' · '+s.computer+'"><span class="mt-top">'+bot(s,34)+'<b class="mt-name">'+esc(mname(s))+'</b></span><span class="mt-task">'+esc(s.title)+'</span><span class="mt-foot">'+pill(s.status)+'</span></a>'}
var ORDER={wait:0,run:1,unread:2,fail:3,done:4};
function projSessions(pid){return D.sessions.filter(function(s){return s.project===pid}).sort(function(a,b){return ORDER[a.status]-ORDER[b.status]||b.t-a.t})}
function lsGet(k,d){try{var v=localStorage.getItem(k);return v==null?d:v}catch(e){return d}}
function lsSet(k,v){try{localStorage.setItem(k,v)}catch(e){}}
function lastMsg(r){return r.msgs.filter(function(m){return m.from}).slice(-1)[0]||{text:"",time:""}}
function stackAv(r){var ms=r.members.filter(function(m){return m!==D.me}).slice(0,4);
 if(ms.length===1) return av(ms[0],"round");
 return '<div class="stack">'+ms.map(function(m){return av(m,"stack")}).join("")+'</div>'}
function sheet(html,cls){var m=$(".modal."+cls); if(!m){m=document.createElement("div"); m.className="modal "+cls; document.body.appendChild(m)} m.innerHTML='<div class="sheet card wide-sheet" role="dialog" aria-modal="true">'+html+'</div>'}
function closeSheet(cls){var m=$(".modal."+cls); if(m) m.remove()}


/* ---------- continuous corners: Figma-style smoothing path with a fixed radius ----------
   The radius never changes with the box's aspect ratio; smoothing shrinks only when the corner
   would not fit (half of the shorter side), the same budget rule Figma and iOS use. */
var SQ_SELECTOR=".card,.rt,.pbox,.mtile,.cchip,.rtcard,.runtile,.tile-acc,.ptile,.inset,.kcol,.kcard,.pane,.bigcomposer,.composer,.approve,.agentcard,.deck-top,.deck-l,.tile,.cchip-ic,.logo,.rtrow,.roomicon,.quick,.strip";
var SQ_SMOOTH=0.6;
function sqCorner(r,s,budget){
 var p=(1+s)*r; var maxS=budget/r-1; s=Math.max(0,Math.min(s,maxS)); p=Math.min(p,budget);
 var arc=90*(1-s), rad=Math.PI/180, arcLen=Math.sin(arc/2*rad)*r*Math.SQRT2, alpha=(90-arc)/2, p34=r*Math.tan(alpha/2*rad), beta=45*s;
 var c=p34*Math.cos(beta*rad), d=c*Math.tan(beta*rad), b=(p-arcLen-c-d)/3, a=2*b;
 return {a:a,b:b,c:c,d:d,p:p,arc:arcLen,r:r}}
function squirclePath(w,h,r,s){
 if(s==null) s=SQ_SMOOTH; var budget=Math.min(w,h)/2; r=Math.max(0,Math.min(r,budget));
 if(r<0.5) return "M0 0H"+w+"V"+h+"H0Z";
 var k=sqCorner(r,s,budget), f=function(n){return Math.round(n*100)/100}, a=k.a,b=k.b,c=k.c,d=k.d,p=k.p,A=k.arc,R=k.r;
 return ["M",f(w-p),0,"c",f(a),0,f(a+b),0,f(a+b+c),f(d),"a",f(R),f(R),0,0,1,f(A),f(A),"c",f(d),f(c),f(d),f(b+c),f(d),f(a+b+c),
  "L",w,f(h-p),"c",0,f(a),0,f(a+b),f(-d),f(a+b+c),"a",f(R),f(R),0,0,1,f(-A),f(A),"c",f(-c),f(d),f(-(b+c)),f(d),f(-(a+b+c)),f(d),
  "L",f(p),h,"c",f(-a),0,f(-(a+b)),0,f(-(a+b+c)),f(-d),"a",f(R),f(R),0,0,1,f(-A),f(-A),"c",f(-d),f(-c),f(-d),f(-(b+c)),f(-d),f(-(a+b+c)),
  "L",0,f(p),"c",0,f(-a),0,f(-(a+b)),f(d),f(-(a+b+c)),"a",f(R),f(R),0,0,1,f(A),f(-A),"c",f(c),f(-d),f(b+c),f(-d),f(a+b+c),f(-d),"Z"].join(" ")}
function sqShadow(bs){ if(!bs||bs==="none") return ""; var parts=bs.split(/,(?![^(]*\))/), last=parts[parts.length-1].trim();
 var col=(last.match(/rgba?\([^)]*\)/)||["rgba(0,0,0,.08)"])[0], nums=last.replace(col,"").trim().split(/\s+/).map(parseFloat);
 return "drop-shadow("+(nums[0]||0)+"px "+(nums[1]||0)+"px "+((nums[2]||0)/2)+"px "+col+")"}
var sqRO=window.ResizeObserver?new ResizeObserver(function(es){es.forEach(function(e){var b=e.borderBoxSize&&e.borderBoxSize[0]; if(b) e.target._sqSize=[b.inlineSize,b.blockSize]; sqPaint(e.target)})}):null;
function sqSnapshot(el){
 el.classList.remove("sq-on"); var cs=getComputedStyle(el);
 var st={fill:cs.backgroundColor,stroke:cs.borderTopColor,sw:parseFloat(cs.borderTopWidth)||0,bw:parseFloat(cs.borderTopWidth)||0,r:parseFloat(cs.borderTopLeftRadius)||0,shadow:sqShadow(cs.boxShadow),img:cs.backgroundImage,ov:cs.overflowY,pos:cs.position,dashed:cs.borderTopStyle==="dashed"};
 el.classList.add("sq-on"); return st}
function sqPaint(el){
 var st=el._sq; if(!st) return;
 /* exact border-box size (fractional, unaffected by press-scale transforms); the SVG is placed over the
    border box (absolute children start inside the border, so shift out by the border width) */
 var sz=el._sqSize||[el.offsetWidth,el.offsetHeight], w=sz[0], h=sz[1]; if(!w||!h) return;
 var svg=el._sqSvg; var inset=st.sw/2, bw=st.bw||0;
 svg.style.left=svg.style.top=(-bw)+"px"; svg.style.width=w+"px"; svg.style.height=h+"px";
 svg.setAttribute("viewBox","0 0 "+w+" "+h); svg.setAttribute("preserveAspectRatio","none");
 var path=svg.firstChild; path.setAttribute("d",squirclePath(w-st.sw,h-st.sw,Math.max(0,st.r-inset)));
 path.setAttribute("transform","translate("+inset+" "+inset+")");
 path.setAttribute("fill",st.fill); path.setAttribute("stroke",st.sw?st.stroke:"none"); path.setAttribute("stroke-width",st.sw);
 if(st.dashed) path.setAttribute("stroke-dasharray","4 3"); else path.removeAttribute("stroke-dasharray");
 svg.style.filter=st.shadow||""}
function sqRefresh(el){ if(!el._sq) return; el._sq=sqSnapshot(el); sqPaint(el)}
function squircle(el){
 if(el._sq||el.closest(".sq-off")) return;
 var cs=getComputedStyle(el);
 if(cs.overflowY==="auto"||cs.overflowY==="scroll"||cs.display==="contents"||cs.display==="none"||cs.backgroundImage!=="none") return; /* scroll boxes and gradients keep CSS corners */
 if(parseFloat(cs.borderTopLeftRadius)!==parseFloat(cs.borderBottomRightRadius)) return; /* asymmetric bubbles keep CSS corners */
 el._sq={}; el._sq=sqSnapshot(el);
 if(cs.position==="static") el.classList.add("sq-static");
 var svg=document.createElementNS("http://www.w3.org/2000/svg","svg"); svg.setAttribute("class","sq-bg"); svg.setAttribute("aria-hidden","true");
 svg.appendChild(document.createElementNS("http://www.w3.org/2000/svg","path")); el.insertBefore(svg,el.firstChild); el._sqSvg=svg;
 sqPaint(el); if(sqRO) sqRO.observe(el);
 el.addEventListener("pointerenter",function(){requestAnimationFrame(function(){sqRefresh(el)})});
 el.addEventListener("pointerleave",function(){requestAnimationFrame(function(){sqRefresh(el)})});
 el.addEventListener("focusin",function(){sqRefresh(el)}); el.addEventListener("focusout",function(){sqRefresh(el)})}
function squircleAll(root){ if(!sqEnabled) return; (root||document).querySelectorAll(SQ_SELECTOR).forEach(squircle); if(root&&root.matches&&root.matches(SQ_SELECTOR)) squircle(root)}
var sqEnabled=true;
document.documentElement.classList.add("sq-js");
new MutationObserver(function(ms){ms.forEach(function(m){m.addedNodes.forEach(function(n){if(n.nodeType===1&&!(n.classList&&n.classList.contains("sq-bg"))) squircleAll(n)})})}).observe(document.documentElement,{childList:true,subtree:true});
new MutationObserver(function(){document.querySelectorAll(".sq-on").forEach(sqRefresh)}).observe(document.documentElement,{attributes:true,attributeFilter:["data-theme"]});

/* ---------- segmented donut for team status (project boxes and anywhere else) ---------- */
function donut(segs,size,center,sub){ size=size||64; var sw=Math.max(5,Math.round(size/10)), r=(size-sw)/2, C=2*Math.PI*r, tot=segs.reduce(function(a,s){return a+s.v},0)||1, gap=segs.filter(function(s){return s.v>0}).length>1?Math.min(6,C*0.03):0, off=0;
 var arcs=segs.filter(function(s){return s.v>0}).map(function(s){var len=Math.max(0,s.v/tot*C-gap); var el='<circle cx="'+size/2+'" cy="'+size/2+'" r="'+r+'" fill="none" stroke="'+s.c+'" stroke-width="'+sw+'" stroke-linecap="round" stroke-dasharray="'+Math.max(0.01,len-sw*0.0)+' '+C+'" stroke-dashoffset="'+(-off)+'"><title>'+esc(s.l+" "+s.v)+'</title></circle>'; off+=s.v/tot*C; return el}).join("");
 return '<span class="donut" style="width:'+size+'px;height:'+size+'px"><svg viewBox="0 0 '+size+' '+size+'" aria-hidden="true"><circle cx="'+size/2+'" cy="'+size/2+'" r="'+r+'" fill="none" stroke="var(--track)" stroke-width="'+sw+'"/><g transform="rotate(-90 '+size/2+' '+size/2+')">'+arcs+'</g></svg><span class="donut-c"><b>'+center+'</b>'+(sub?'<small>'+sub+'</small>':"")+'</span></span>'}
function teamSegs(l){var n=function(st){return l.filter(function(s){return st.indexOf(s.status)>=0}).length};
 return [{l:"실행",v:n(["run"]),c:"var(--s-run)"},{l:"승인 대기",v:n(["wait"]),c:"var(--s-wait)"},{l:"새 결과",v:n(["unread"]),c:"var(--c-sky)"},{l:"완료",v:n(["done"]),c:"var(--c-grey)"},{l:"실패",v:n(["fail"]),c:"var(--c-red)"}]}


/* ---------- press feedback: ripple from the press point and a short press-scale, one delegated handler ----------
   Applies to buttons, tabs, chips, links and clickable cards. Opt out with .no-press on the element or an ancestor.
   Skips text inputs and drag handles. Reduced motion: faint ripple only. The ripple lives in a fixed overlay,
   so the pressed element's DOM, layout, squircle layer and drag handling are untouched. */
var PRESS_SEL='button,a[href],[role=button],[role=tab],[role=switch],[data-href],[data-open],[data-act],[data-comp],[data-expand],[data-suggest],.nav,.chip,.mtile,.rtrow,.rtcard,.cchip,.ptile,.room-item,.thread,.kcard,.tool';
var reduceMotion=window.matchMedia?matchMedia("(prefers-reduced-motion: reduce)"):{matches:false};
var pressEl=null, pressAnim=null;
function pressTarget(t){ if(!t||!t.closest) return null; if(t.closest("input,textarea,select,[contenteditable=true],.no-press,.gut,[data-pdrag],[data-sdrag]")) return null;
 var el=t.closest(PRESS_SEL); if(!el||el.disabled||el.classList.contains("modal")||el.classList.contains("app")) return null; return el}
/* calm press, like Toss and ordinary web pages: a soft darkening tint over the element (no expanding circle)
   and a slow press-in to 0.975 (180 ms ease-out). Release waits until the press has been visible for at least
   120 ms, then returns to 1 over 200 ms ease-out with no overshoot. Reduced motion: tint only. */
var PRESS_MIN=120;
function ripple(el,x,y){var r=el.getBoundingClientRect(); if(!r.width||!r.height) return null;
 var cs=getComputedStyle(el), o=document.createElement("span"); o.className="ripple-clip"; o.setAttribute("aria-hidden","true");
 o.style.cssText="left:"+r.left+"px;top:"+r.top+"px;width:"+r.width+"px;height:"+r.height+"px;border-radius:"+cs.borderRadius+";color:"+cs.color;
 document.body.appendChild(o); requestAnimationFrame(function(){o.classList.add("in")}); return o}
function tintOut(o){ if(!o) return; o.classList.remove("in"); o.classList.add("out"); setTimeout(function(){o.remove()},260)}
var pressAt=0, pressTint=null;
function pressRelease(){ if(!pressEl&&!pressTint) return; var el=pressEl, an=pressAnim, tint=pressTint; pressEl=null; pressAnim=null; pressTint=null;
 var wait=Math.max(0,PRESS_MIN-(Date.now()-pressAt));
 setTimeout(function(){ tintOut(tint);
  if(!el) return; var from=1; try{from=parseFloat(getComputedStyle(el).scale)||0.975}catch(e){}
  if(an) an.cancel();
  if(!reduceMotion.matches&&el.animate) el.animate([{scale:String(from)},{scale:"1"}],{duration:200,easing:"cubic-bezier(.2,0,0,1)"})},wait)}
var pressLast=0;
function pressStart(e){ if(typeof e.button==="number"&&e.button>0) return; if(e.type==="mousedown"&&Date.now()-pressLast<500) return;
 if(e.type==="pointerdown") pressLast=Date.now();
 var t=e.target; if(t&&t.nodeType===3) t=t.parentElement; var el=pressTarget(t); if(!el) return;
 if(pressEl||pressTint) pressRelease();
 pressAt=Date.now();
 try{pressTint=ripple(el,e.clientX,e.clientY)}catch(err){pressTint=null}
 if(reduceMotion.matches||!el.animate) return;
 try{pressEl=el; pressAnim=el.animate([{scale:"1"},{scale:"0.975"}],{duration:180,easing:"cubic-bezier(.2,0,0,1)",fill:"forwards"})}catch(err){pressEl=null}}
window.addEventListener("pointerdown",pressStart,true);
window.addEventListener("mousedown",pressStart,true);
["pointerup","pointercancel","mouseup","dragstart"].forEach(function(ev){window.addEventListener(ev,pressRelease,true)});
document.addEventListener("pointerout",function(e){if(pressEl&&e.target===pressEl&&!pressEl.contains(e.relatedTarget)) pressRelease()},true);
document.addEventListener("keydown",function(e){if((e.key==="Enter"||e.key===" ")&&!e.repeat){var el=pressTarget(e.target); if(el&&e.target===el&&!/^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)){var o=ripple(el,0,0); setTimeout(function(){tintOut(o)},PRESS_MIN)}}});

/* ---------- render, dispatcher, boot ---------- */

function render(html){document.body.innerHTML=html; updateThemeButtons()}
var Ember=window.Ember=window.Ember||{};
Ember.screens=Ember.screens||{};
Ember.data=D;
Ember.__press={pressTarget:pressTarget,ripple:ripple,pressStart:pressStart,selector:PRESS_SEL};
function screenName(){return document.body.dataset.screen}
function curScreen(){return Ember.screens[screenName()]||{}}
document.addEventListener("click",function(e){
 var t=e.target, a;
 if(!t.closest(".phonemenu")){var pm=$(".phonemenu.open"); if(pm) pm.classList.remove("open")}
 if(t.classList&&t.classList.contains("app")&&t.classList.contains("overlay")){t.classList.remove("overlay");return}
 var sc=curScreen(); if(sc.click&&sc.click(e,t)===true) return;
 if((a=t.closest("[data-back]"))){document.body.classList.remove("view-detail");return}
 if(t.classList&&t.classList.contains("modal")){if(sc.onModalClose) sc.onModalClose(t); t.remove(); return}
 if((a=t.closest("[data-act]"))){var act=a.dataset.act;
  if(act==="theme") toggleTheme();
  else if(act==="reset") resetDemo();
  else if(act==="platform") togglePlatform();
  else if(act==="side") toggleSide();
  else if(act==="pmenu"){a.parentNode.classList.toggle("open")}
  else if(act==="settings") toast("설정 (데모)");
  else if(act==="closemodal"){var md=$(".modal"); if(md){if(sc.onModalClose) sc.onModalClose(md); md.remove()}}
  else if(act==="attach") toast("파일 첨부 (데모)");
  else if(act==="newin") newSession(a.dataset.pid,"");
  else if(act==="new"){var pid=screenName()==="project"?(location.hash.slice(1)||"ember"):"unsorted"; newSession(pid,"")}
  else if(act==="search"){var sb=$(".searchbox"); sb.classList.toggle("open"); if(sb.classList.contains("open")) sb.querySelector("input").focus()}
  else if(act==="ide"){var ideK=ideChoice(); toast("IDE 열기: "+IDEN[ideK][0]+" ("+IDEN[ideK][1]+")")}
  else if(act==="assign") toast("컴퓨터 할당 창 (데모)");
  else if(act==="moretabs"){var cur=screenName(); sheet('<div class="sheet-h"><b class="sheet-t">더보기</b><div class="sheet-acts"></div><button class="icon-btn sm" data-act="closemodal" aria-label="닫기">'+I("close")+'</button></div><nav class="more-list" aria-label="더보기">'+[["routines","repeat","루틴","routines.html"],["vcs","git","버전 관리","vcs.html"],["agents","cpu","에이전트","agents.html"],["files","arrows-right-left","파일","files.html"]].map(function(x){return '<a class="list-row'+(x[0]===cur?" on":"")+'" href="'+x[3]+'"'+(x[0]===cur?' aria-current="page"':"")+'><div class="tile">'+I(x[1])+'</div><span class="t spacer">'+x[2]+'</span>'+I("chev")+'</a>'}).join("")+'</nav>',"moremodal")}
  else if(act==="computer") toast(a.dataset.id+": 세부 화면 (데모)");
  return}
 if((a=t.closest("[data-href]"))) location.href=a.dataset.href;
});
window.addEventListener("hashchange",function(){var sc=curScreen(); if(sc.onHash) sc.onHash()});
document.addEventListener("keydown",function(e){if(e.key==="Escape"){var m=$(".modal"); if(m){var sc=curScreen(); if(sc.onModalClose) sc.onModalClose(m); m.remove()}}});
function boot(){var sc=Ember.screens[screenName()]; if(sc&&sc.render) sc.render()}
document.addEventListener("DOMContentLoaded",boot);
/* everything a screen needs, also reachable as globals */
["D","save","resetDemo","toggleTheme","I","esc","$","nowTime","toast","proj","sess","maxT","ST","pill","sdot","av","VC","vendorOf","bot","mname","lastProj","lsGet","lsSet","shell","head","card","sessRows","compRows","sorted","newSession","activity","member","mtile","ORDER","projSessions","lastMsg","stackAv","sheet","closeSheet","render","boot","toggleSide","squircle","squircleAll","squirclePath","sqRefresh","donut","teamSegs","togglePlatform","ripple","themePref","updateThemeButtons","BRAND","IDEN","ideChoice"].forEach(function(k){try{Ember[k]=eval(k)}catch(err){}});
/* ---------- model picker (v10 addition 4, v11 ref10 layout) and permission mode picker (v11) ----------
   Ember.modelPicker.html(opts) / mount(el, opts) / value(el): chip "harness · model · effort".
       opts: {harness, model, effort, account, project, name, onChange(v)}; v = {harness, model, modelName, effort, account, project, profile, label}.
     Without account, the plan check uses the harness profile's account (Ember.profileOf(harness, project)); the profile name shows on rows and in the chip title.
     Popover: left rail (favourites star + one icon per harness), search on top, one row per "model (effort)",
     Ctrl+1..9 picks the n-th row, star toggles a favourite (saved in D.modelFavs). Picking a row closes it.
     Hidden inputs <name>agent and <name>model; emits a bubbling "modelchange" event.
   Ember.modePicker.html(opts) / mount(el, opts) / value(el): chip with the permission mode.
     opts: {harness, mode, name, onChange(v)}; v = {harness, mode, modeName, warn}. Hidden input <name>mode; emits "modechange".
     A modelchange in the same form (or composer / [data-pick-anchor]) moves the mode chip to the new harness. */
/* profiles (v11): account set + routing, assigned per harness. agents.js may provide Ember.profileFor(harness) and Ember.profiles();
   until then these demo fallbacks answer. A project may override one harness's profile (project.profileOverride[harness] = profile id). */
Ember.PROFILE_FALLBACK=[{id:"claude-personal",name:"Claude 개인",harness:"Claude Code",account:"ncmnd · Max"},{id:"claude-proxy",name:"Claude via opencodex",harness:"Claude Code",account:"work · Max"},{id:"codex-team",name:"Codex 팀",harness:"Codex",account:"ncmnd · Pro"},{id:"gemini-personal",name:"Gemini 개인",harness:"Gemini CLI",account:"ncmnd · Pro"}];
Ember.profileList=function(harness){var l=typeof Ember.profiles==="function"?Ember.profiles():Ember.PROFILE_FALLBACK; return l.filter(function(p){return !harness||p.harness===harness})};
Ember.profileOf=function(harness,projectId){var pr=(D.projects||[]).filter(function(x){return x.id===projectId})[0], ov=pr&&pr.profileOverride&&pr.profileOverride[harness];
 if(ov){var o=Ember.profileList(harness).filter(function(p){return p.id===ov})[0]; if(o) return o}
 if(typeof Ember.profileFor==="function"){var f=Ember.profileFor(harness); if(f) return f}
 return Ember.profileList(harness)[0]||null};
(function(){
 var EFF={low:"낮음",medium:"보통",high:"높음"}, CB={}, seq=0, pop=null, cur=null, st=null;
 var GLYPH={claude:'<svg viewBox="0 0 24 24" class="hg" aria-hidden="true"><path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6 5.6 18.4" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" fill="none"/></svg>',
  codex:'<svg viewBox="0 0 24 24" class="hg" aria-hidden="true"><path d="M12 2.8 20 7.4v9.2l-8 4.6-8-4.6V7.4z" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" fill="none"/><path d="M8.5 10l2.5 2-2.5 2M12.5 14.5h3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/></svg>',
  gemini:'<svg viewBox="0 0 24 24" class="hg" aria-hidden="true"><path d="M12 2.5c.6 5.2 4.3 8.9 9.5 9.5-5.2.6-8.9 4.3-9.5 9.5-.6-5.2-4.3-8.9-9.5-9.5 5.2-.6 8.9-4.3 9.5-9.5z" fill="currentColor"/></svg>'};
 function H(){return (window.EMBER_DEMO&&window.EMBER_DEMO.models)||{}}
 function MD(){return (window.EMBER_DEMO&&window.EMBER_DEMO.modes)||{list:{},byHarness:{},def:{}}}
 function hIcon(h){var c=H()[h]; return '<span class="hic hic-'+(c?c.vendor:"claude")+'">'+GLYPH[c?c.vendor:"claude"]+'</span>'}
 function favs(){if(!D.modelFavs) D.modelFavs=[]; return D.modelFavs}
 function tierOf(acct){var m=/·\s*(\S+)/.exec(acct||""); return m?m[1]:""}
 function findModel(h,m){var c=H()[h]; if(!c) return null; return c.list.filter(function(x){return x.id===m||x.name===m||(m&&m.toLowerCase().split(" ")[0].indexOf(x.id.split("-")[0])>=0)})[0]||null}
 function blocked(x,acct){return x&&x.block&&x.block[tierOf(acct)]||""}
 function norm(o){var h=H()[o.harness]?o.harness:"Claude Code", c=H()[h], mm=findModel(h,o.model);
  if(!o.account){var pf0=Ember.profileOf(h,o.project); if(pf0&&pf0.account) o={harness:o.harness,model:o.model,effort:o.effort,project:o.project,account:pf0.account}}
  if(!mm||blocked(mm,o.account)) mm=c.list.filter(function(x){return x.id===c.def&&!blocked(x,o.account)})[0]||c.list.filter(function(x){return !blocked(x,o.account)})[0]||c.list[0];
  var e=c.effort?(c.effort.indexOf((o.effort||"").toLowerCase())>=0?o.effort.toLowerCase():(/·\s*(\w+)/.exec(o.model||"")||[])[1]):null;
  if(c.effort&&c.effort.indexOf((e||"").toLowerCase())<0) e=c.defEffort; if(e) e=e.toLowerCase();
  var pf=Ember.profileOf(h,o.project);
  return {harness:h, model:mm.id, modelName:mm.name, effort:e, account:o.account||"", project:o.project||"", profile:pf?pf.name:"", label:h+" · "+mm.name+(e?" · "+EFF[e]:"")}}
 function inputs(v,name){var p=name||""; return '<input type="hidden" name="'+p+'agent" value="'+esc(v.harness)+'"><input type="hidden" name="'+p+'model" value="'+esc(v.modelName+(v.effort?" · "+v.effort[0].toUpperCase()+v.effort.slice(1):""))+'">'}
 function chev(){return I("chev").replace('<svg class="i"','<svg class="i mp-chev"')}
 function chipInner(v){return hIcon(v.harness)+'<span class="mp-l ellipsis">'+esc(v.label)+'</span>'+chev()}
 function html(o){o=o||{}; var v=norm(o), id="mp"+(++seq); if(o.onChange) CB[id]=o.onChange;
  return '<span class="mpick-wrap" data-mpwrap="'+id+'"><button type="button" class="pickchip mpick" data-mpick="'+id+'" data-h="'+esc(v.harness)+'" data-m="'+esc(v.model)+'" data-e="'+(v.effort||"")+'" data-acct="'+esc(o.account||"")+'" data-proj="'+esc(o.project||"")+'" data-name="'+esc(o.name||"")+'" title="'+esc(v.label+(v.profile?" · 프로필 "+v.profile:""))+'" aria-haspopup="dialog" aria-expanded="false" aria-label="하네스와 모델: '+esc(v.label)+(v.profile?", 프로필 "+esc(v.profile):"")+'">'+chipInner(v)+'</button>'+inputs(v,o.name)+'</span>'}
 function mount(el,o){if(!el) return; el.innerHTML=html(o); return el.querySelector(".mpick")}
 function read(b){return norm({harness:b.dataset.h,model:b.dataset.m,effort:b.dataset.e,account:b.dataset.acct,project:b.dataset.proj})}
 function scopeOf(b){return b.closest("form, .composer, .bigcomposer, [data-pick-anchor]")||b.parentNode.parentNode}
 function apply(b,v){b.dataset.h=v.harness; b.dataset.m=v.model; b.dataset.e=v.effort||""; b.innerHTML=chipInner(v); b.title=v.label+(v.profile?" · 프로필 "+v.profile:""); b.setAttribute("aria-label","하네스와 모델: "+v.label+(v.profile?", 프로필 "+v.profile:""));
  var w=b.parentNode; [].slice.call(w.querySelectorAll("input[type=hidden]")).forEach(function(x){x.remove()}); w.insertAdjacentHTML("beforeend",inputs(v,b.dataset.name));
  if(CB[b.dataset.mpick]) CB[b.dataset.mpick](v); b.dispatchEvent(new CustomEvent("modelchange",{bubbles:true,detail:v}));
  var sc=scopeOf(b); if(sc) [].forEach.call(sc.querySelectorAll(".mdpick"),function(m){if(m.dataset.h!==v.harness) mdApply(m,mdNorm({harness:v.harness,mode:m.dataset.v}))})}
 /* rows: every model x effort of the current view (a harness, favourites, or a search across all) */
 function rowsFor(view,q,acct,proj){var M=H(), out=[], PF={};
  function push(h,x,e){var nm=x.name+(e?" ("+EFF[e]+")":""); if(q&&(nm+" "+h).toLowerCase().indexOf(q)<0) return;
   if(!(h in PF)) PF[h]=Ember.profileOf(h,proj); var pf=PF[h], a=acct||(pf&&pf.account)||"";
   out.push({h:h,m:x.id,e:e,name:nm,why:blocked(x,a),prof:pf?pf.name:"",key:h+"|"+x.id+"|"+(e||"")})}
  if(view==="fav"&&!q){favs().forEach(function(k){var p=k.split("|"), c=M[p[0]], x=c&&c.list.filter(function(y){return y.id===p[1]})[0]; if(x) push(p[0],x,p[2]||null)}); return out}
  Object.keys(M).forEach(function(h){if(!q&&h!==view) return; var c=M[h]; c.list.forEach(function(x){(c.effort||[null]).slice().reverse().forEach(function(e){push(h,x,e)})})}); return out}
 function popHTML(v){var M=H(), q=st.q.toLowerCase().trim(), rows=rowsFor(st.view,q,cur.dataset.acct,v.project), sel=v.harness+"|"+v.model+"|"+(v.effort||""), F=favs();
  st.rows=rows; if(st.act>=rows.length) st.act=rows.length-1; if(st.act<0) st.act=Math.max(0,rows.map(function(r){return r.key}).indexOf(sel));
  var rail='<button type="button" class="mp-rail-b'+(st.view==="fav"?" on":"")+'" data-mpv="fav" title="즐겨찾기" aria-label="즐겨찾기" aria-pressed="'+(st.view==="fav")+'">'+I("star")+'</button><span class="mp-rail-sep"></span>'+
   Object.keys(M).map(function(h){return '<button type="button" class="mp-rail-b'+(st.view===h?" on":"")+'" data-mpv="'+esc(h)+'" title="'+esc(h)+'" aria-label="'+esc(h)+'" aria-pressed="'+(st.view===h)+'">'+hIcon(h)+'</button>'}).join("");
  var list=rows.length?rows.map(function(r,i){var on=r.key===sel, fv=F.indexOf(r.key)>=0;
    return '<div class="mp-row'+(on?" on":"")+(i===st.act?" act":"")+(r.why?" off":"")+'" role="option" id="mprow'+i+'" aria-selected="'+on+'"'+(r.why?' aria-disabled="true"':"")+' data-mpr="'+i+'"><span class="mp-rt"><b>'+esc(r.name)+'</b><span class="mp-h">'+hIcon(r.h)+'<span>'+esc(r.h)+(r.prof?' · <span class="mp-prof">'+esc(r.prof)+'</span>':"")+(r.why?' · <em>'+esc(r.why)+'</em>':"")+'</span></span></span>'+
     (i<9?'<kbd class="mp-k">Ctrl+'+(i+1)+'</kbd>':'<span class="mp-k"></span>')+'<button type="button" class="mp-star'+(fv?" on":"")+'" data-mpfav="'+i+'" aria-pressed="'+fv+'" aria-label="'+esc(r.name)+(fv?" 즐겨찾기 해제":" 즐겨찾기")+'">'+I("star")+'</button></div>'}).join("")
   :'<div class="mp-empty">'+(st.view==="fav"&&!q?"즐겨찾기가 없습니다. 행의 별을 누르면 여기에 모입니다.":"일치하는 모델이 없습니다.")+'</div>';
  return '<nav class="mp-rail" aria-label="하네스">'+rail+'</nav><div class="mp-main"><div class="mp-top"><label class="mp-search">'+I("search")+'<input type="text" data-mpq placeholder="모델 검색" aria-label="모델 검색" aria-controls="mplist" aria-activedescendant="mprow'+st.act+'" value="'+esc(st.q)+'" autocomplete="off"></label></div><div class="mp-body" id="mplist" role="listbox" aria-label="모델">'+list+'</div></div>'}
 /* ---- popover shell shared by both pickers: anchoring and height rules from v10 ---- */
 function place(){if(!pop||!cur) return; if(matchMedia("(max-width:560px)").matches){pop.style.left=pop.style.top=pop.style.maxHeight=""; return}
  /* open below when it fits, otherwise on the side with more room; it sits 8px above or below the enclosing composer (.composer, .bigcomposer or [data-pick-anchor]), not just the chip; height capped to that side only when needed */
  pop.style.maxHeight=""; var r=cur.getBoundingClientRect(), anc=cur.closest(".composer, .bigcomposer, [data-pick-anchor]"), ar=anc?anc.getBoundingClientRect():r, w=pop.offsetWidth,
   top=pop.querySelector(".mp-top"), body=pop.querySelector(".mp-body"), full=(top?top.offsetHeight:0)+(body?body.scrollHeight:pop.scrollHeight)+2, below=innerHeight-ar.bottom-8-8, above=ar.top-8-8;
  var up=full>below&&above>below, h=Math.min(full,up?above:below); pop.style.maxHeight=Math.max(160,h)+"px"; h=pop.offsetHeight;
  var x=Math.min(Math.max(8,r.left),innerWidth-w-8), y=up?Math.max(8,ar.top-8-h):ar.bottom+8; pop.style.left=x+"px"; pop.style.top=y+"px"; pop.classList.toggle("up",up)}
 function shell(b,cls,label){close(); cur=b; b.setAttribute("aria-expanded","true");
  pop=document.createElement("div"); pop.className="mp-pop "+cls; pop.setAttribute("role","dialog"); pop.setAttribute("aria-label",label);
  var bd=document.createElement("div"); bd.className="mp-backdrop"; bd.addEventListener("click",function(){close(true)}); document.body.appendChild(bd); document.body.appendChild(pop)}
 function close(refocus){if(!pop) return; pop.remove(); var bd=document.querySelector(".mp-backdrop"); if(bd) bd.remove(); pop=null; st=null; if(cur){cur.setAttribute("aria-expanded","false"); if(refocus) cur.focus()} cur=null}
 /* ---- model popover ---- */
 function draw(keepFocus){var v=read(cur), q=pop.querySelector("[data-mpq]"), pos=q?q.selectionStart:null; pop.innerHTML=popHTML(v); place();
  var nq=pop.querySelector("[data-mpq]"); if(keepFocus!==false&&nq){nq.focus(); if(pos!=null) nq.setSelectionRange(pos,pos)} var a=pop.querySelector(".mp-row.act"); if(a&&a.scrollIntoView) a.scrollIntoView({block:"nearest"})}
 function open(b){shell(b,"mp-model","하네스와 모델 선택"); var v=read(b), sel=v.harness+"|"+v.model+"|"+(v.effort||"");
  st={view:v.harness,q:"",act:-1,rows:[]}; pop.addEventListener("click",onPop); pop.addEventListener("keydown",onKey); pop.addEventListener("input",function(e){if(e.target.matches("[data-mpq]")){st.q=e.target.value; st.act=0; draw()}});
  draw()}
 function pick(i){var r=st.rows[i]; if(!r) return; if(r.why){toast(r.why); return} apply(cur,norm({harness:r.h,model:r.m,effort:r.e,account:cur.dataset.acct,project:cur.dataset.proj})); close(true)}
 function onPop(e){e.stopPropagation(); var a;
  if((a=e.target.closest("[data-mpfav]"))){var r=st.rows[+a.dataset.mpfav], F=favs(), k=F.indexOf(r.key); if(k<0) F.push(r.key); else F.splice(k,1); save(); draw(); return}
  if((a=e.target.closest("[data-mpv]"))){st.view=a.dataset.mpv; st.q=""; st.act=-1; draw(); return}
  if((a=e.target.closest("[data-mpr]"))){pick(+a.dataset.mpr)}}
 function onKey(e){var k=e.key;
  if(k==="Escape"){e.stopPropagation(); e.preventDefault(); close(true); return}
  if(e.ctrlKey&&/^[1-9]$/.test(k)){e.preventDefault(); e.stopPropagation(); pick(+k-1); return}
  if(e.target.closest(".mp-rail")&&(k==="ArrowDown"||k==="ArrowUp")){var rb=[].slice.call(pop.querySelectorAll(".mp-rail-b")), j=rb.indexOf(e.target.closest(".mp-rail-b"))+(k==="ArrowDown"?1:-1); if(rb[j]) rb[j].focus(); e.preventDefault(); return}
  if(!e.target.matches("[data-mpq]")) return;
  if(k==="ArrowDown"||k==="ArrowUp"){st.act=Math.max(0,Math.min(st.rows.length-1,st.act+(k==="ArrowDown"?1:-1))); draw(); e.preventDefault()}
  else if(k==="Home"&&!st.q||k==="End"&&!st.q){st.act=k==="Home"?0:st.rows.length-1; draw(); e.preventDefault()}
  else if(k==="Enter"){e.preventDefault(); pick(st.act)}}
 /* ---- permission mode ---- */
 function mdNorm(o){var M=MD(), h=M.byHarness[o.harness]?o.harness:"Claude Code", l=M.byHarness[h], m=l.indexOf(o.mode)>=0?o.mode:(M.def[h]||l[0]), x=M.list[m]||{};
  return {harness:h, mode:m, modeName:x.name||m, warn:!!x.warn}}
 function mdInner(v){return '<span class="md-ic">'+I(v.warn?"warn":"shield")+'</span><span class="mp-l ellipsis">'+esc(v.modeName)+'</span>'+chev()}
 function mdHTML(o){o=o||{}; var v=mdNorm(o), id="md"+(++seq); if(o.onChange) CB[id]=o.onChange;
  return '<span class="mpick-wrap"><button type="button" class="pickchip mdpick'+(v.warn?" warn":"")+'" data-mdpick="'+id+'" data-h="'+esc(v.harness)+'" data-v="'+v.mode+'" data-name="'+esc(o.name||"")+'" aria-haspopup="dialog" aria-expanded="false" aria-label="권한 모드: '+esc(v.modeName)+'">'+mdInner(v)+'</button><input type="hidden" name="'+esc(o.name||"")+'mode" value="'+v.mode+'"></span>'}
 function mdMount(el,o){if(!el) return; el.innerHTML=mdHTML(o); return el.querySelector(".mdpick")}
 function mdRead(b){return mdNorm({harness:b.dataset.h,mode:b.dataset.v})}
 function mdApply(b,v){b.dataset.h=v.harness; b.dataset.v=v.mode; b.classList.toggle("warn",v.warn); b.innerHTML=mdInner(v); b.setAttribute("aria-label","권한 모드: "+v.modeName);
  var inp=b.parentNode.querySelector("input[type=hidden]"); if(inp) inp.value=v.mode;
  if(CB[b.dataset.mdpick]) CB[b.dataset.mdpick](v); b.dispatchEvent(new CustomEvent("modechange",{bubbles:true,detail:v}))}
 function mdDraw(){var v=mdRead(cur), M=MD();
  pop.innerHTML='<div class="mp-main"><div class="md-head">권한 모드 · '+esc(v.harness)+'</div><div class="mp-body" role="listbox" aria-label="권한 모드">'+M.byHarness[v.harness].map(function(m){var x=M.list[m], on=m===v.mode;
   return '<button type="button" role="option" class="md-row'+(on?" on":"")+(x.warn?" warn":"")+'" aria-selected="'+on+'" data-mdv="'+m+'"><span class="md-ic">'+I(x.warn?"warn":"shield")+'</span><span class="mp-rt"><b>'+esc(x.name)+'</b><span class="mp-s">'+esc(x.desc)+'</span></span>'+(on?I("check"):"")+'</button>'}).join("")+'</div></div>';
  place(); var f=pop.querySelector(".md-row.on")||pop.querySelector(".md-row"); if(f) f.focus()}
 function mdOpen(b){shell(b,"mp-mode","권한 모드 선택");
  pop.addEventListener("click",function(e){e.stopPropagation(); var a=e.target.closest("[data-mdv]"); if(!a) return; var v=mdRead(cur); mdApply(cur,mdNorm({harness:v.harness,mode:a.dataset.mdv})); close(true)});
  pop.addEventListener("keydown",function(e){var k=e.key; if(k==="Escape"){e.stopPropagation(); e.preventDefault(); close(true); return}
   var o=e.target.closest(".md-row"); if(!o) return; if(k==="ArrowDown"||k==="ArrowUp"){var s=[].slice.call(pop.querySelectorAll(".md-row")), i=s.indexOf(o)+(k==="ArrowDown"?1:-1); if(s[i]) s[i].focus(); e.preventDefault()}});
  mdDraw()}
 /* ---- chips: click and keyboard open; account change re-checks the plan ---- */
 document.addEventListener("click",function(e){var b=e.target.closest&&e.target.closest("[data-mpick],[data-mdpick]"); if(!b) return; e.stopPropagation(); e.preventDefault(); if(cur===b){close(true); return} if(b.dataset.mpick) open(b); else mdOpen(b)},true);
 document.addEventListener("keydown",function(e){var b=e.target.closest&&e.target.closest("[data-mpick],[data-mdpick]"); if(b&&(e.key==="ArrowDown"||e.key==="Enter"||e.key===" ")){e.preventDefault(); e.stopPropagation(); if(b.dataset.mpick) open(b); else mdOpen(b)}},true);
 /* a project select in the same form moves the model chip to that project (profile override, plan check, profile name) */
 document.addEventListener("change",function(e){var t=e.target; if(t.name!=="project"||!t.form) return; [].forEach.call(t.form.querySelectorAll(".mpick"),function(b){var v=read(b); b.dataset.proj=t.value; var n=norm({harness:v.harness,model:v.model,effort:v.effort,account:b.dataset.acct,project:t.value}); if(n.model!==v.model) toast(v.modelName+"은(는) 이 프로젝트의 프로필 플랜에서 쓸 수 없어 "+n.modelName+"(으)로 바꿨습니다"); apply(b,n)})});
 document.addEventListener("change",function(e){var t=e.target; if(t.name!=="account"||!t.form) return; [].forEach.call(t.form.querySelectorAll(".mpick"),function(b){var v=read(b); b.dataset.acct=t.value; var n=norm({harness:v.harness,model:v.model,effort:v.effort,account:t.value,project:b.dataset.proj}); if(n.model!==v.model) toast(v.modelName+"은(는) 이 계정 플랜에서 쓸 수 없어 "+n.modelName+"(으)로 바꿨습니다"); apply(b,n)})});
 addEventListener("resize",place); addEventListener("scroll",function(e){if(pop&&!pop.contains(e.target)) place()},true);
 Ember.modelPicker={html:html, mount:mount, value:function(b){return read(b.matches(".mpick")?b:b.querySelector(".mpick"))}, close:close, models:H, normalize:norm, favourites:favs};
 Ember.modePicker={html:mdHTML, mount:mdMount, value:function(b){return mdRead(b.matches(".mdpick")?b:b.querySelector(".mdpick"))}, close:close, modes:MD, normalize:mdNorm};
})();
/* ---------- team character styles (v10 리더 결정): one style per project, drawn by chars.js (EmberChar) when loaded ---------- */
Ember.CHAR_STYLES=[["mongle","몽글","눈, 입, 볼이 있는 기본 캐릭터"],["malang","말랑","이름으로 정해지는 말랑한 블롭"],["crew","크루","역할 소품을 단 캡슐 몸"],["pocket","포켓","떠 있는 단순 도형 마스코트"],["cream","크림","크림색 몸에 하네스 색 옷"],["keyring","키링","고리에 매달려 흔들리는 참"]];
Ember.charStyle=function(pid){var p=(D.projects||[]).filter(function(x){return x.id===pid})[0]; return (p&&p.characterStyle)||"mongle"};
/* charStylePicker(name, value, sample): six preview cards as radio inputs; sample is a member object drawn in each style */
Ember.charStylePicker=function(name,value,sample){sample=sample||{agent:"Claude Code",status:"done",title:"미리보기",project:""};
 var list=window.EmberChar&&EmberChar.styles?EmberChar.styles.map(function(s){return [s.id,s.name,s.desc]}):Ember.CHAR_STYLES, sm={name:sample.title?mname(sample):"미리보기",agent:sample.agent,role:sample.role,status:"done",lead:false};
 return '<div class="cs-grid" role="radiogroup" aria-label="팀 캐릭터">'+list.map(function(x){var art=window.EmberChar&&EmberChar.draw?EmberChar.draw(sm,{style:x[0],size:48,label:x[1]}):bot(sample.agent,"done",48,x[1]);
  return '<label class="cs-card"><input type="radio" name="'+name+'" value="'+x[0]+'"'+(x[0]===value?" checked":"")+'><span class="cs-art" aria-hidden="true">'+art+'</span><b>'+x[1]+'</b><span class="cs-s">'+x[2]+'</span></label>'}).join("")+'</div>'};
