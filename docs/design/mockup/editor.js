/* Ember editor screen (D32 native editor), owned by the editor draft agent.
   Owner decision 2026-10-25: the plain Ember island style (rounded panels, tabs, tool windows, code area) with
   JetBrains-style 40px icon stripes on both sides. A keymap setting (VS Code or JetBrains, localStorage
   "ember-mock-keymap") drives both the shortcut hints and the key handling. #terminal maximizes the bottom tool window. */
(function(){
var MAC=/Mac|iPhone|iPad/.test(navigator.platform)&&!/[?&]os=other/.test(location.search);
/* [macOS, other OS] per action; "" = no default binding. Real defaults of each product. */
var KEYMAPS={
 vscode:{name:"VS Code",find:"파일로 이동",action:"명령 팔레트",k:{project:["⇧⌘E","Ctrl+Shift+E"],commit:["⌃⇧G","Ctrl+Shift+G"],git:["⌃⇧G","Ctrl+Shift+G"],run:["⇧⌘U","Ctrl+Shift+U"],problems:["⇧⌘M","Ctrl+Shift+M"],terminal:["⌃`","Ctrl+`"],
  close:["⌘W","Ctrl+W"],save:["⌘S","Ctrl+S"],find:["⌘P","Ctrl+P"],recent:["⌃R","Ctrl+R"],action:["⇧⌘P","Ctrl+Shift+P"],runcfg:["⌃F5","Ctrl+F5"],stop:["⇧F5","Shift+F5"],
  goto:["⌃G","Ctrl+G"],fmt:["⇧⌥F","Shift+Alt+F"],hide:["⌘J","Ctrl+J"],ask:["⌃⌘I","Ctrl+Alt+I"],newterm:["⌃⇧`","Ctrl+Shift+`"],max:["",""]}},
 jetbrains:{name:"JetBrains",find:"모든 곳에서 찾기",action:"동작 찾기",k:{project:["⌘1","Alt+1"],commit:["⌘0","Alt+0"],git:["⌘9","Alt+9"],run:["⌘4","Alt+4"],problems:["⌘6","Alt+6"],terminal:["⌥F12","Alt+F12"],
  close:["⌘W","Ctrl+F4"],save:["⌘S","Ctrl+S"],find:["⇧⇧","Shift Shift"],recent:["⌘E","Ctrl+E"],action:["⇧⌘A","Ctrl+Shift+A"],runcfg:["⌃R","Shift+F10"],stop:["⌘F2","Ctrl+F2"],
  goto:["⌘L","Ctrl+G"],fmt:["⌥⌘L","Ctrl+Alt+L"],hide:["⇧⎋","Shift+Esc"],ask:["",""],newterm:["⌘T","Ctrl+Shift+T"],max:["⇧⌘'","Ctrl+Shift+'"]}}};
var KM=lsGet("ember-mock-keymap","vscode"); if(!KEYMAPS[KM]) KM="vscode";
var SC={}; function buildSC(){SC={}; var t=KEYMAPS[KM].k; Object.keys(t).forEach(function(a){SC[a]=t[a][MAC?0:1]})} buildSC();
function setKeymap(v){KM=v; lsSet("ember-mock-keymap",v); buildSC()}
/* parse "⇧⌘P" or "Ctrl+Shift+P" into {meta,ctrl,alt,shift,key} */
function parseSC(x){if(!x||x==="⇧⇧"||x==="Shift Shift") return null; var m={meta:false,ctrl:false,alt:false,shift:false,key:""};
 if(MAC){var r=x; while(r&&"⌘⌃⌥⇧".indexOf(r[0])>=0){m[{"⌘":"meta","⌃":"ctrl","⌥":"alt","⇧":"shift"}[r[0]]]=true; r=r.slice(1)} m.key=r==="⎋"?"Escape":r}
 else {x.split("+").forEach(function(p,i,a){if(i<a.length-1) m[{Ctrl:"ctrl",Alt:"alt",Shift:"shift"}[p]]=true; else m.key=p==="Esc"?"Escape":p})} return m}
function keyOK(e,k){if(k==="`") return e.code==="Backquote"; if(k==="'") return e.code==="Quote"; if(/^\d$/.test(k)) return e.code==="Digit"+k; if(/^F\d+$/.test(k)||k==="Escape") return e.key===k; return e.code==="Key"+k.toUpperCase()}
function scMatch(e,x){var m=parseSC(x); return !!m&&e.metaKey===m.meta&&e.ctrlKey===m.ctrl&&e.altKey===m.alt&&e.shiftKey===m.shift&&keyOK(e,m.key)}
var ED_FILES={
 "ember/proxy/src/shim.rs":["use crate::router::Router;","use crate::session::{Session, SessionId};","","/// Attaches a session to the IDE router (#128).","pub struct SessionShim {","    router: Router,","    egress: Egress,","}","","impl SessionShim {","    pub fn attach(&mut self, session: &Session) -> Result<()> {","        let id = session.id().clone();","        // Clone before the IDE router takes ownership (#128).","        let handle = session.handle().clone();","        self.router.insert(id.clone(), handle);","        self.egress.watch(&id)?;","        Ok(())","    }","","    pub fn detach(&mut self, id: &SessionId) {","        if let Some(h) = self.router.remove(id) {","            h.close();","        }","        self.egress.stop(id);","        tracing::debug!(%id, \"shim detached\");","    }","}"],
 "ember/proxy/src/router.rs":["use std::collections::HashMap;","","pub struct Router {","    routes: HashMap<SessionId, Handle>,","}","","impl Router {","    pub fn new() -> Self {","        Self { routes: HashMap::new() }","    }","","    pub fn insert(&mut self, id: SessionId, h: Handle) {","        self.routes.insert(id, h);","    }","}"],
 "ember/proxy/Cargo.toml":["[package]","name = \"ember-proxy\"","version = \"0.4.0\"","edition = \"2021\"","","[dependencies]","tokio = { version = \"1\", features = [\"full\"] }","tracing = \"0.1\""],
 "docs/SPEC.md":["# Ember SPEC","","## FR-L28 Editor","","The native editor opens files from the project worktree.","Status: draft (#151)."]
};
var ED_TREE=[["darkpyonix-ember",0,"dir"],[".github",1,"dir",1],["docs",1,"dir"],["docs/SPEC.md",2,"file"],["ember",1,"dir"],["ember/proxy",2,"dir"],["ember/proxy/src",3,"dir"],["ember/proxy/src/router.rs",4,"file"],["ember/proxy/src/shim.rs",4,"file"],["ember/proxy/tests",3,"dir",1],["ember/proxy/Cargo.toml",3,"file"],["ember/client",2,"dir",1],["AGENTS.md",1,"file"],["README.md",1,"file"]];
var ED_MOD={"ember/proxy/src/shim.rs":"M","ember/proxy/src/router.rs":"M","docs/SPEC.md":"M"};
var ED_GUT={"ember/proxy/src/shim.rs":{12:"add",13:"add",14:"mod",15:"add",20:"mod",24:"add"},"ember/proxy/src/router.rs":{11:"mod"}};
var ED_DIAG={"ember/proxy/src/shim.rs":{16:["warn","unused Result from egress.watch, propagate it with ?"]},"ember/proxy/src/router.rs":{4:["err","cannot find type Handle in this scope"]}};
var st={tabs:["ember/proxy/src/shim.rs","ember/proxy/src/router.rs","ember/proxy/Cargo.toml"],cur:"ember/proxy/src/shim.rs",dirty:{"ember/proxy/src/shim.rs":1},
 left:true,bottom:true,btab:"terminal",ttab:0,max:false,closed:{},cursor:[15,22],term:null,agent:"run",menu:false};
var TERM0=[["p","~/darkpyonix-ember (feat/session-shim) $ ","cargo test -p ember-proxy --test shim"],["o","   Compiling ember-proxy v0.4.0 (ember/proxy)"],["o","    Finished test [unoptimized + debuginfo] target(s) in 4.21s"],["o","     Running tests/shim_attach.rs"],["o","running 3 tests"],["ok","test attach_routes_session ... ok"],["ok","test detach_stops_egress ... ok"],["err","test health_reports_shim ... FAILED"],["o",""],["err","test result: FAILED. 2 passed; 1 failed"]];
var RUN=[["o","▶ ember-proxy (cargo run -p ember-proxy)"],["o","2026-10-25T20:41:07Z INFO ember_proxy: listening on 0.0.0.0:8080"],["o","2026-10-25T20:41:09Z INFO shim: attach session=s1"],["warn","2026-10-25T20:41:12Z WARN egress: watch retry 1/3"]];
function terms(){if(!st.term) st.term=[{n:"로컬",l:TERM0.slice()},{n:"mac-mini · ember-리더",l:[["p","mac-mini:~/wt/ember-리더 $ ","git status -sb"],["o","## feat/session-shim...origin/feat/session-shim [ahead 2]"],["o"," M ember/proxy/src/shim.rs"]]}]; return st.term}
function Ic(n,c){return '<svg class="i '+c+'" viewBox="0 0 24 24">'+EMBER_ICONS[n]+'</svg>'}
function e$(s){return document.querySelector(s)}
function base(p){return p.split("/").pop()}
function tip(label,k){return ' data-tip="'+esc(label)+'"'+(k?' data-sc="'+esc(k)+'"':"")+' aria-label="'+esc(label)+(k?" ("+esc(k)+")":"")+'"'}
function kb(k){return '<kbd class="ed-sc">'+k+'</kbd>'}
var KW=/\b(use|pub|struct|impl|fn|let|mut|if|else|return|for|in|match|self|Self|crate|mod|Some|None|Ok|Err)\b/g;
function hl(line,p){var h=esc(line);
 if(/\.toml$/.test(p)) return h.replace(/^(\[.*\])$/,'<i class="t-kw">$1</i>').replace(/(&quot;.*?&quot;)/g,'<i class="t-str">$1</i>');
 if(/\.md$/.test(p)) return /^#/.test(line)?'<i class="t-kw">'+h+'</i>':h;
 var c=h.indexOf("//"); var code=c>=0?h.slice(0,c):h, com=c>=0?'<i class="t-com">'+h.slice(c)+'</i>':"";
 code=code.replace(/(&quot;.*?&quot;)/g,'<i class="t-str">$1</i>').replace(KW,'<i class="t-kw">$1</i>').replace(/\b([a-z_]+)(?=\()/g,'<i class="t-fn">$1</i>').replace(/\b([A-Z][A-Za-z]+)\b/g,'<i class="t-ty">$1</i>');
 return code+com}
function stripe(side){var L=side==="l"?[["project","folder","프로젝트",SC.project],["commit","commit","커밋",SC.commit],["git","git","Git",SC.git]]:side==="r"?[["agent","spark","에이전트",SC.ask],["notif","bell","알림",""]]:[["terminal","code","터미널",SC.terminal],["run","play","실행",SC.run],["problems","warn","문제",SC.problems]];
 return '<nav class="ed-stripe ed-stripe-'+side+'" aria-label="도구 창">'+L.map(function(x){var on=(x[0]==="project"&&st.left&&!st.max)||(side==="b"&&st.bottom&&st.btab===x[0]);return '<button class="ed-sb'+(on?" on":"")+'" data-ed="tw" data-w="'+x[0]+'"'+tip(x[2],x[3])+'>'+I(x[1])+'<span class="ed-sbl">'+x[2]+'</span></button>'}).join("")+'</nav>'}
function twHead(title,tabs,acts,hide){return '<div class="ed-twh"><b class="ed-twt">'+title+'</b>'+(tabs||"")+'<div class="spacer"></div>'+(acts||"")+'<button class="ed-ib" data-ed="'+hide+'"'+tip("숨기기",SC.hide)+'>'+I("close")+'</button></div>'}
function tree(){return '<section class="ed-tw ed-left" aria-label="프로젝트">'+twHead("프로젝트","",'<button class="ed-ib" data-ed="collapse"'+tip("모두 접기","")+'>'+Ic("chev","chev")+'</button><button class="ed-ib" data-ed="locate"'+tip("열린 파일 찾기","")+'>'+I("search")+'</button>',"hideleft")+
 '<div class="ed-tree" role="tree">'+ED_TREE.filter(function(n){var parts=n[0].split("/");for(var i=1;i<parts.length;i++){var pre=parts.slice(0,i).join("/");if(st.closed[pre])return false}return true}).map(function(n){var p=n[0],dir=n[2]==="dir",open=dir&&!st.closed[p]&&!n[3];if(dir&&n[3]&&st.closed[p]===undefined)st.closed[p]=1;open=dir&&!st.closed[p];
  return '<div class="ed-tn'+(p===st.cur?" sel":"")+(ED_MOD[p]?" mod":"")+'" role="treeitem" style="--d:'+n[1]+'" data-ed="'+(dir?"dir":"open")+'" data-p="'+p+'"'+(dir?' aria-expanded="'+open+'"':"")+'>'+(dir?'<svg class="i chev'+(open?" dn":"")+'" viewBox="0 0 24 24">'+EMBER_ICONS.chev+'</svg>'+I("folder"):'<span class="chev-sp"></span>'+I("file"))+'<span class="ellipsis">'+(n[1]===0?'<b>'+p+'</b> <span class="muted">~/darkpyonix-ember</span>':base(p))+'</span></div>'}).join("")+'</div></section>'}
function tabs(){return '<div class="ed-tabs" role="tablist">'+st.tabs.map(function(p){return '<div class="ed-tab'+(p===st.cur?" on":"")+(ED_MOD[p]?" vc-mod":"")+'" role="tab" aria-selected="'+(p===st.cur)+'" data-ed="tab" data-p="'+p+'" title="'+p+'">'+I("file")+'<span class="ellipsis">'+base(p)+'</span>'+(st.dirty[p]?'<i class="ed-dirty" title="저장 안 됨"></i>':"")+'<button class="ed-tx" data-ed="close" data-p="'+p+'"'+tip("탭 닫기",SC.close)+'>'+I("close")+'</button></div>'}).join("")+'<div class="spacer"></div><button class="ed-ib" data-ed="menu"'+tip("더보기","")+' aria-haspopup="menu">'+I("more")+'</button></div>'}
function crumbs(){var p=st.cur.split("/"),f=ED_FILES[st.cur]||[],fn="";for(var i=st.cursor[0]-1;i>=0;i--){var m=/fn (\w+)/.exec(f[i]||"");if(m){fn=m[1];break}}
 return '<div class="ed-crumbs">'+p.map(function(x,i){return '<span'+(i===p.length-1?' class="cur"':"")+'>'+x+'</span>'}).join(I("chev"))+(fn?I("chev")+'<span class="fn">impl SessionShim</span>'+I("chev")+'<span class="fn">fn '+fn+'</span>':"")+'</div>'}
function code(){var f=ED_FILES[st.cur]||["// 미리보기 데이터가 없습니다"],g=ED_GUT[st.cur]||{},d=ED_DIAG[st.cur]||{};
 return '<div class="ed-code" role="textbox" aria-label="'+st.cur+' 편집기" aria-readonly="true">'+f.map(function(l,i){var n=i+1,dg=d[n];return '<div class="ed-ln'+(n===st.cursor[0]?" cur":"")+'" data-ed="line" data-n="'+n+'"><span class="ed-no">'+n+'</span><span class="ed-gm '+(g[n]||"")+'"></span><span class="ed-tx2">'+(hl(l,st.cur)||" ")+(dg?'<span class="ed-dg '+dg[0]+'" title="'+esc(dg[1])+'">'+esc(dg[1])+'</span>':"")+'</span></div>'}).join("")+'</div>'}
function probs(){var out=[];Object.keys(ED_DIAG).forEach(function(p){Object.keys(ED_DIAG[p]).forEach(function(n){out.push([p,n,ED_DIAG[p][n]])})});return out}
function bottom(){var T=terms(),bt=st.btab,body;
 var btabs='<div class="ed-btabs" role="tablist">'+[["terminal","터미널",SC.terminal],["run","실행",SC.run],["problems","문제 "+probs().length,SC.problems]].map(function(x){return '<button role="tab" class="'+(bt===x[0]?"on":"")+'" aria-selected="'+(bt===x[0])+'" data-ed="btab" data-w="'+x[0]+'"'+tip(x[1],x[2])+'>'+x[1]+'</button>'}).join("")+'</div>';
 if(bt==="terminal"){var t=T[st.ttab]; body='<div class="ed-tsub">'+T.map(function(x,i){return '<button class="'+(i===st.ttab?"on":"")+'" data-ed="ttab" data-i="'+i+'">'+I("code")+esc(x.n)+'</button>'}).join("")+'<button class="ed-ib" data-ed="newterm"'+tip("새 터미널",SC.newterm)+'>'+I("plus")+'</button></div><div class="ed-term" id="edterm">'+t.l.map(line).join("")+'<form class="ed-tin" data-ed-form><span class="pr">'+esc(t.l[0][1]||"$ ")+'</span><input id="edcmd" autocomplete="off" spellcheck="false" aria-label="터미널 입력"></form></div>'}
 else if(bt==="run") body='<div class="ed-term">'+RUN.map(line).join("")+'</div>';
 else body='<div class="ed-plist">'+probs().map(function(x){return '<div class="ed-prow" data-ed="goto" data-p="'+x[0]+'" data-n="'+x[1]+'"><span class="ed-pdot '+x[2][0]+'"></span><span class="ellipsis spacer">'+esc(x[2][1])+'</span><span class="muted mono">'+base(x[0])+':'+x[1]+'</span></div>'}).join("")+'</div>';
 var acts=bt==="run"?'<button class="ed-ib" data-ed="rerun"'+tip("다시 실행",SC.runcfg)+'>'+I("refresh")+'</button><button class="ed-ib" data-ed="stop"'+tip("중지",SC.stop)+'>'+I("pause")+'</button>':"";
 return '<section class="ed-tw ed-bottom" aria-label="아래 도구 창">'+twHead("",btabs,acts+'<button class="ed-ib" data-ed="max"'+tip(st.max?"원래 크기":"최대화",SC.max)+'>'+I(st.max?"down":"up")+'</button>',"hidebottom")+body+'</section>'}
function line(l){return '<div class="tl '+l[0]+'">'+(l[0]==="p"?'<span class="pr">'+esc(l[1])+'</span>'+esc(l[2]):esc(l[1]))+'</div>'}
function statusbar(){var a={run:["run","ember-리더 실행 중"],wait:["wait","ember-리더 승인 대기"],done:["ok","ember-리더 쉬는 중"]}[st.agent];
 return '<footer class="ed-status"><span class="ed-sbi crumbs-s">darkpyonix-ember › '+st.cur.split("/").slice(0,-1).join(" › ")+'</span><div class="spacer"></div><button class="ed-sbi agent" data-ed="agent"'+tip("에이전트 상태","")+'><span class="dot '+a[0]+'"></span>'+a[1]+'</button><button class="ed-sbi" data-ed="goto-line"'+tip("줄로 이동",SC.goto)+'>'+st.cursor[0]+'행 '+st.cursor[1]+'열</button><span class="ed-sbi hide-md">LF</span><span class="ed-sbi hide-md">UTF-8</span><span class="ed-sbi hide-md">4칸</span><button class="ed-sbi" data-ed="tw" data-w="git"'+tip("Git 브랜치",SC.git)+'>'+I("git")+'feat/session-shim</button></footer>'}
function top(){return '<header class="ed-top">'+/* the full DarkPyonix logo: the editor's top left is one of its two places (D58, 2026-11-27) */((Ember.BRAND&&Ember.BRAND.full)||'<img class="logo logo-img" src="logo/phoenix-fold/mark-sm.svg" width="32" height="32" alt="DarkPyonix">')+'<button class="ed-pj" data-ed="tw" data-w="project">darkpyonix-ember'+Ic("chev","chev")+'</button><button class="ed-pj" data-ed="tw" data-w="git">'+I("git")+'feat/session-shim'+Ic("chev","chev")+'</button><div class="spacer"></div>'+
 '<button class="ed-find" data-ed="find"'+tip(KEYMAPS[KM].find,SC.find)+'>'+I("search")+'<span class="hide-md">파일, 동작, 기호 찾기</span>'+kb(SC.find)+'</button>'+
 '<div class="ed-runcfg"><button class="ed-pj" data-ed="runcfg">'+I("play")+'ember-proxy'+Ic("chev","chev")+'</button><button class="ed-ib run" data-ed="rerun"'+tip("실행",SC.runcfg)+'>'+I("play")+'</button><button class="ed-ib" data-ed="stop"'+tip("중지",SC.stop)+'>'+I("pause")+'</button></div>'+
 '<button class="ed-ib" data-ed="ask"'+tip("에이전트에게 묻기",SC.ask)+'>'+I("spark")+'</button><button class="ed-ib" data-ed="theme"'+tip("테마 전환","")+'>'+I(document.documentElement.dataset.theme==="dark"?"moon":"sun")+'</button></header>'}
function menu(){return '<div class="ed-menu" role="menu">'+[["save","저장",SC.save],["recent","최근 파일",SC.recent],["fmt","코드 정리",SC.fmt],["goto-line","줄로 이동",SC.goto],["closeall","다른 탭 모두 닫기",""],["action",KEYMAPS[KM].action,SC.action]].map(function(x){return '<button role="menuitem" data-ed="'+x[0]+'"><span class="spacer">'+x[1]+'</span>'+(x[2]?'<span class="ed-msc">'+x[2]+'</span>':"")+'</button>'}).join("")+
 '<div class="ed-msep"></div><div class="ed-mlab">단축키 체계</div>'+Object.keys(KEYMAPS).map(function(v){return '<button role="menuitemradio" aria-checked="'+(v===KM)+'" data-ed="keymap" data-v="'+v+'"><span class="ed-mchk">'+(v===KM?I("check"):"")+'</span><span class="spacer">'+KEYMAPS[v].name+'</span><span class="ed-msc">'+KEYMAPS[v].k.find[MAC?0:1]+'</span></button>'}).join("")+'</div>'}
function draw(){var y=(e$(".ed-code")||{}).scrollTop||0;
 render('<div class="ed'+(st.left?"":" no-left")+(st.bottom?"":" no-bottom")+(st.max?" max":"")+'">'+top()+'<div class="ed-rail">'+stripe("l")+stripe("b")+'</div>'+(st.left?tree():"")+
  '<section class="ed-main" aria-label="편집기">'+(st.tabs.length?tabs()+crumbs()+code():'<div class="ed-empty"><b>열린 파일이 없습니다</b><p class="muted">'+kb(SC.find)+' '+KEYMAPS[KM].find+'<br>'+kb(SC.project)+' 프로젝트 창</p></div>')+(st.menu?menu():"")+'</section>'+
  stripe("r")+(st.bottom?bottom():"")+statusbar()+'</div><div class="ed-tip" id="edtip" role="tooltip"></div>');
 var c=e$(".ed-code"); if(c) c.scrollTop=y; var t=e$("#edterm"); if(t) t.scrollTop=t.scrollHeight;
 var f=e$("[data-ed-form]"); if(f) f.onsubmit=function(ev){ev.preventDefault(); var v=e$("#edcmd").value.trim(), T=terms()[st.ttab]; T.l.push(["p",T.l[0][1],v]); if(v) T.l.push(v==="clear"?["o",""]:["o","(데모) "+v+": 실제 앱은 이 컴퓨터의 셸로 보냅니다"]); if(v==="clear") T.l=[T.l[0]]; draw(); var i=e$("#edcmd"); if(i) i.focus()}}
function openFile(p,n){if(!ED_FILES[p]){toast(base(p)+": 미리보기 데이터가 없습니다");return} if(st.tabs.indexOf(p)<0) st.tabs.push(p); st.cur=p; st.cursor=[+n||1,1]; draw()}
function tw(w){if(w==="project") st.left=!st.left; else if(w==="terminal"||w==="run"||w==="problems"){if(st.bottom&&st.btab===w) st.bottom=false; else {st.bottom=true; st.btab=w}} else if(w==="git"||w==="commit") {location.href="vcs.html#ember"; return} else toast(w==="agent"?"에이전트 창은 세션 화면과 같은 대화를 엽니다":"새 알림이 없습니다"); draw()}
function click(e,t){var a=t.closest("[data-ed]"); if(st.menu&&!(a&&a.dataset.ed==="menu")){st.menu=false; if(!a){draw(); return true}}
 if(!a) return false; var k=a.dataset.ed, p=a.dataset.p;
 if(k==="close"){e.stopPropagation(); var i=st.tabs.indexOf(p); st.tabs.splice(i,1); delete st.dirty[p]; if(st.cur===p) st.cur=st.tabs[Math.max(0,i-1)]||""; draw()}
 else if(k==="tab") {st.cur=p; draw()}
 else if(k==="open") openFile(p);
 else if(k==="goto") openFile(p,a.dataset.n);
 else if(k==="dir"){st.closed[p]=st.closed[p]?0:1; draw()}
 else if(k==="collapse"){ED_TREE.forEach(function(n){if(n[2]==="dir"&&n[1]>0) st.closed[n[0]]=1}); draw()}
 else if(k==="locate"){st.cur.split("/").forEach(function(_,i,arr){st.closed[arr.slice(0,i+1).join("/")]=0}); draw()}
 else if(k==="line"){st.cursor=[+a.dataset.n,1]; draw()}
 else if(k==="tw") tw(a.dataset.w);
 else if(k==="btab"){st.btab=a.dataset.w; draw()}
 else if(k==="ttab"){st.ttab=+a.dataset.i; draw()}
 else if(k==="newterm"){terms().push({n:"로컬 "+terms().length,l:[["p","~/darkpyonix-ember $ ",""]]}); st.ttab=terms().length-1; draw()}
 else if(k==="hideleft"){st.left=false; draw()} else if(k==="hidebottom"){st.bottom=false; st.max=false; draw()}
 else if(k==="max"){st.max=!st.max; draw()}
 else if(k==="menu"){st.menu=!st.menu; draw()}
 else if(k==="keymap"){setKeymap(a.dataset.v); st.menu=false; toast("단축키: "+KEYMAPS[KM].name); draw()}
 else if(k==="theme"){Ember.toggleTheme(); draw()}
 else if(k==="save"){delete st.dirty[st.cur]; toast("저장했습니다"); draw()}
 else if(k==="closeall"){st.tabs=[st.cur]; draw()}
 else if(k==="agent"){st.agent={run:"wait",wait:"done",done:"run"}[st.agent]; draw()}
 else if(k==="rerun"){st.bottom=true; st.btab="run"; RUN.push(["o","2026-10-25T20:42:00Z INFO ember_proxy: restarted"]); draw()}
 else if(k==="stop"){RUN.push(["err","프로세스가 종료되었습니다 (exit 130)"]); st.bottom=true; st.btab="run"; draw()}
 else toast(({find:"모든 곳에서 찾기",action:"동작 찾기",fmt:"코드 정리",ask:"선택한 코드로 에이전트에게 묻기","goto-line":"줄로 이동",runcfg:"실행 구성"}[k]||k)+" (데모)");
 return true}
function runAct(a){var lab={find:KEYMAPS[KM].find,action:KEYMAPS[KM].action,recent:"최근 파일",fmt:"코드 정리",goto:"줄로 이동",ask:"에이전트에게 묻기"};
 if(a==="project"||a==="run"||a==="problems"||a==="terminal") return tw(a);
 if(a==="commit"||a==="git") return tw("git");
 if(a==="close"&&st.cur){var i=st.tabs.indexOf(st.cur); st.tabs.splice(i,1); delete st.dirty[st.cur]; st.cur=st.tabs[Math.max(0,i-1)]||""}
 else if(a==="save"){delete st.dirty[st.cur]; toast("저장했습니다")}
 else if(a==="hide"){st.bottom=KM==="vscode"?!st.bottom:false; st.max=false}
 else if(a==="max"){st.bottom=true; st.max=!st.max}
 else if(a==="runcfg"){st.bottom=true; st.btab="run"; RUN.push(["o","2026-10-25T20:42:00Z INFO ember_proxy: restarted"])}
 else if(a==="stop"){st.bottom=true; st.btab="run"; RUN.push(["err","프로세스가 종료되었습니다 (exit 130)"])}
 else if(a==="newterm"){st.bottom=true; st.btab="terminal"; terms().push({n:"로컬 "+terms().length,l:[["p","~/darkpyonix-ember $ ",""]]}); st.ttab=terms().length-1}
 else if(lab[a]) {toast(lab[a]+" ("+SC[a]+", 데모)"); return}
 draw()}
var edLastShift=0;
document.addEventListener("keydown",function(e){if(document.body.dataset.screen!=="editor") return;
 if(e.key==="Shift"&&!e.repeat&&!e.metaKey&&!e.ctrlKey&&!e.altKey){var now=Date.now(); if(SC.find==="⇧⇧"||SC.find==="Shift Shift"){if(now-edLastShift<400){edLastShift=0; runAct("find"); return}} edLastShift=now; return}
 var inTerm=e.target&&e.target.id==="edcmd";
 for(var a in SC){if(scMatch(e,SC[a])){if(inTerm&&!/terminal|hide|max|newterm/.test(a)) continue; e.preventDefault(); window.__edLastAct=a; runAct(a); return}}});
document.addEventListener("mouseover",function(e){var el=e.target.closest&&e.target.closest("[data-tip]"), tp=e$("#edtip"); if(!tp) return; if(!el){tp.classList.remove("on"); return}
 tp.innerHTML='<span>'+esc(el.dataset.tip)+'</span>'+(el.dataset.sc?'<span class="ed-msc">'+esc(el.dataset.sc)+'</span>':""); var r=el.getBoundingClientRect(); tp.classList.add("on");
 var x=Math.min(innerWidth-tp.offsetWidth-8,Math.max(8,r.left+r.width/2-tp.offsetWidth/2)), y=r.bottom+6; if(y+tp.offsetHeight>innerHeight-4) y=r.top-tp.offsetHeight-6; tp.style.left=x+"px"; tp.style.top=y+"px"});
var edFirst=true;
function onHash(){if(edFirst){edFirst=false; if(innerWidth<=900) st.left=false}st.max=location.hash==="#terminal"; if(st.max){st.bottom=true; st.btab="terminal"} draw()}
Ember.screens.editor={render:onHash,onHash:onHash,click:click};
addEventListener("hashchange",onHash);
})();
