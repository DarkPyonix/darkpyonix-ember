"use strict";
/* ---------- computer strip ---------- */
var OSN={mac:"macOS",linux:"Linux",win:"Windows"};
var KIND_IC={monitor:"pc",laptop:"pc",server:"server",mini:"cpu"};
function compStatus(c){return {online:["온라인","green","ok"],offline:["오프라인","",'off'],unknown:["알 수 없음","",""]}[c.status]}
function compTile(c){var st=compStatus(c), run=D.sessions.filter(function(s){return s.computer===c.id&&s.status==="run"}).length;
 var ps=D.projects.filter(function(p){return p.computers.indexOf(c.id)>=0}).length;
 return '<button class="cchip st-'+c.status+'" data-comp="'+c.id+'" aria-label="'+c.id+' '+st[0]+'"><span class="cchip-ic">'+I(KIND_IC[c.kind]||"pc")+'<span class="dot '+st[2]+'"></span></span><span class="col" style="min-width:0;align-items:flex-start"><b class="ellipsis">'+c.id+'</b><span class="small muted ellipsis">'+(OSN[c.osk]||"")+' · '+(run?'실행 '+run:st[0])+(ps?' · 프로젝트 '+ps:"")+'</span></span></button>'}
function compStrip(list,title){
 return '<section class="cstrip-wrap" aria-label="'+title+'"><div class="cstrip">'+list.map(compTile).join("")+'<button class="cchip add" data-act="addcomp" aria-label="컴퓨터 추가"><span class="cchip-ic">'+I("plus")+'</span><b>컴퓨터 추가</b></button></div><button class="icon-btn sm carrow cl" data-scroll="-1" aria-label="왼쪽으로">'+I("back")+'</button><button class="icon-btn sm carrow cr" data-scroll="1" aria-label="오른쪽으로">'+I("chev")+'</button></section>'}
function compModal(id){var c=D.computers.filter(function(x){return x.id===id})[0], st=compStatus(c);
 var ss=D.sessions.filter(function(s){return s.computer===id}).sort(function(a,b){return ORDER[a.status]-ORDER[b.status]});
 var m=document.createElement("div"); m.className="modal"; m.innerHTML='<div class="sheet card" role="dialog" aria-label="'+id+'"><div class="sheet-h"><b class="sheet-t">'+id+'</b><span class="pill '+st[1]+'">'+st[0]+'</span><div class="sheet-acts"></div><button class="icon-btn sm" data-act="closemodal" aria-label="닫기">'+I("close")+'</button></div>'+
 '<div class="card-b"><div class="kv">'+[["OS",c.os],["에이전트",c.agents],["마지막 접속",c.seen||"지금"]].map(function(x){return '<div class="inset"><div class="l">'+x[0]+'</div><div class="v">'+x[1]+'</div></div>'}).join("")+'</div></div>'+
 '<div class="grp">팀원 '+ss.length+'</div>'+(ss.length?ss.map(member).join(""):'<div class="empty">세션이 없습니다</div>')+
 '<div class="card-b flex" style="border-top:1px solid var(--line)"><button class="btn sm" data-act="assign">프로젝트 할당</button><div class="spacer"></div><button class="btn sm" data-act="closemodal">닫기</button></div></div>';
 document.body.appendChild(m)}

/* ---------- project: boxes of members (sessions), full view per project ---------- */
var memFilter="all";
var expanded={};
function projBox(p){
 var l=projSessions(p.id), wt=l.filter(function(s){return s.status==="wait"}).length, rn=l.filter(function(s){return s.status==="run"}).length, MAX=4, open=expanded[p.id], rest=l.slice(MAX);
 var n=l.length||1, a=rn/n*100, b=a+wt/n*100, c=b+l.filter(function(s){return s.status==="unread"||s.status==="done"}).length/n*100;
 var deck=rest.length&&!open?'<button class="deck" data-expand="'+p.id+'" aria-label="나머지 '+rest.length+'명 펼치기"><span class="deck-top">'+rest.slice(0,4).map(function(s){return bot(s,26)}).join("")+'<span class="col" style="min-width:0;align-items:flex-start"><b>외 '+rest.length+'명</b><span class="small muted ellipsis">'+rest.slice(0,3).map(mname).join(", ")+'</span></span></span></button>':"";
 return '<article class="pbox"><header class="pbox-h"><button class="pbox-title" data-psettings="'+p.id+'" title="'+p.name+' 설정" aria-label="'+p.name+' 설정 열기">'+donut(teamSegs(l),60,l.length,"팀원")+'<span class="col" style="min-width:0"><b class="ellipsis pname">'+p.name+'</b><span class="s ellipsis">'+esc(p.repo)+(p.branch?" · "+p.branch:"")+'</span></span></button>'+
 '<div class="flex pbox-act"><a class="icon-btn sm" href="vcs.html#'+p.id+'" title="버전 관리" aria-label="'+p.name+' 버전 관리">'+I("git")+'</a><button class="icon-btn sm" data-psettings="'+p.id+'" title="프로젝트 설정" aria-label="프로젝트 설정">'+I("gear")+'</button><a class="icon-btn sm primary" href="conversation.html#p:'+p.id+'" title="세션 열기" aria-label="'+p.name+' 세션 열기">'+I("grid")+'</a></div>'+
 '<div class="pbox-stats">'+teamSegs(l).filter(function(s){return s.v>0}).map(function(s){return '<span class="pst"><i style="background:'+s.c+'"></i><b>'+s.v+'</b>'+s.l+'</span>'}).join("")+'<span class="pst comp">'+I("pc")+'<b>'+p.computers.length+'</b>컴퓨터</span></div></header>'+
 '<div class="pbox-team">'+(l.length?(open?l:l.slice(0,MAX)).map(mtile).join("")+deck+(open&&rest.length?'<button class="btn sm fold" data-expand="'+p.id+'">접기</button>':""):'<div class="empty">아직 팀원이 없습니다</div>')+'</div></article>'}
function projectGrid(){
 var main='<main class="main">'+head("프로젝트","프로젝트를 누르면 세션으로 들어가 팀원들의 대화를 함께 봅니다.","q",'<button class="btn" data-act="pnew" title="새 프로젝트" aria-label="새 프로젝트">'+I("folder")+'<span class="hide-md">새 프로젝트</span></button>')+compStrip(D.computers,"컴퓨터")+'<div class="pgrid" id="pgrid">'+D.projects.map(projBox).join("")+'</div></main>';
 render(shell("project",main));
 $("#q").addEventListener("input",function(){var v=this.value.toLowerCase(); $("#pgrid").innerHTML=D.projects.filter(function(p){return !v||(p.name+projSessions(p.id).map(function(s){return mname(s)+s.title}).join(" ")).toLowerCase().indexOf(v)>=0}).map(projBox).join("")||'<div class="empty">일치하는 프로젝트가 없습니다</div>'});
}
function memberList(pid,q){
 var l=projSessions(pid).filter(function(s){return (memFilter==="all"||s.status===memFilter)&&(!q||(s.title+s.agent+s.computer).toLowerCase().indexOf(q)>=0)});
 if(!l.length) return '<div class="empty">일치하는 멤버가 없습니다</div>';
 return ["wait","run","unread","fail","done"].map(function(st){var g=l.filter(function(s){return s.status===st}); return g.length?'<div class="grp">'+ST[st][0]+' · '+g.length+'</div>'+g.map(member).join(""):""}).join("")}
function projectFull(p){
 var l=projSessions(p.id), rts=(D.routines||[]).filter(function(r){return r.project===p.id}), rooms=D.rooms.filter(function(r){return r.project===p.id}), comps=D.computers.filter(function(c){return p.computers.indexOf(c.id)>=0});
 var cnt=function(st){return l.filter(function(s){return s.status===st}).length};
 var pins=l.filter(function(s){return s.pinned});
 var seg='<div class="seg">'+[["all","전체"],["wait","승인 대기"],["run","실행 중"],["unread","새 결과"],["done","완료"],["fail","실패"]].map(function(x){return '<button data-memf="'+x[0]+'" class="'+(x[0]===memFilter?"on":"")+'">'+x[1]+'</button>'}).join("")+'</div>';
 var main='<main class="main"><div class="flex" style="margin-bottom:var(--sp4)"><a class="btn sm" href="project.html" aria-label="프로젝트 목록으로">'+I("back")+'프로젝트</a><div class="spacer"></div><a class="btn sm" href="vcs.html#'+p.id+'">'+I("git")+'버전 관리</a><button class="btn sm" data-psettings="'+p.id+'">'+I("gear")+'설정</button><a class="btn primary sm" href="conversation.html#p:'+p.id+'">'+I("grid")+'세션 열기</a></div>'+
 head(p.name,esc(p.repo)+(p.branch?" · "+p.branch:""),null,'<button class="btn" data-act="ide" title="IDE 열기" aria-label="IDE 열기">'+I("code")+'<span class="hide-md">IDE 열기</span></button>').replace('data-act="new"','data-act="newin" data-pid="'+p.id+'"')+
 compStrip(comps,"이 프로젝트의 컴퓨터")+'<div class="strip"><div class="kpi"><div class="l">멤버</div><div class="v">'+l.length+'</div></div><div class="kpi"><div class="l">승인 대기</div><div class="v">'+cnt("wait")+'</div></div><div class="kpi"><div class="l">실행 중</div><div class="v">'+cnt("run")+'</div></div><div class="kpi"><div class="l">이번 주 토큰</div><div class="v">8.4M</div></div>'+
 (pins.length?'<div class="pins"><div class="l">고정</div><div class="flex wrap">'+pins.map(function(s){return '<a class="chip" href="conversation.html#'+s.id+'">'+sdot(s.status)+esc(s.title)+'</a>'}).join("")+'</div></div>':"")+'</div>'+
 '<div class="grid g21 collapse">'+
 card("user","멤버",'<div class="card-b col" style="gap:var(--sp3);padding-bottom:var(--sp2)"><label class="searchbox" style="width:100%">'+I("search")+'<input id="mq" placeholder="멤버 검색" aria-label="멤버 검색"></label>'+seg+'</div><div id="mlist">'+memberList(p.id,"")+'</div>','<span class="pill">'+l.length+'명</span>')+
 '<div class="col" style="gap:var(--sp5)">'+
 card("repeat","루틴",(rts.length?rts.map(function(r){var x=r.runs[0];return '<div class="list-row" data-href="routines.html#'+r.id+'"><div class="tile">'+I(r.trigger==="time"?"clock":"zap")+'</div><div class="spacer col"><span class="t ellipsis">'+esc(r.name)+'</span><span class="s ellipsis">'+esc(r.schedule)+'</span></div><div class="right">'+(r.enabled?(x?pill(x.status):""):'<span class="pill">꺼짐</span>')+'</div></div>'}).join(""):'<div class="empty">루틴이 없습니다</div>'),'<a class="btn sm" href="routines.html#p:'+p.id+'">모두 보기</a>')+
 card("hash","채널",(rooms.length?rooms.map(function(r){var lm=lastMsg(r);return '<div class="list-row" data-href="channels.html#'+r.id+'">'+stackAv(r)+'<div class="spacer col"><span class="t ellipsis">'+esc(r.name)+'</span><span class="s ellipsis">'+esc(lm.text)+'</span></div>'+(r.unread?'<span class="redbadge">'+r.unread+'</span>':"")+'</div>'}).join(""):'<div class="empty">채널이 없습니다</div>'))+
 '</div></div></main>';
 render(shell("project",main));
 $("#mq").addEventListener("input",function(){$("#mlist").innerHTML=memberList(p.id,this.value.toLowerCase())});
}
function project(){var p=proj(location.hash.slice(1)); if(p) projectFull(p); else projectGrid(); window.scrollTo(0,0); var m=$(".main"); if(m) m.scrollTop=0}


/* ---------- project settings sheet ---------- */

function psOpt(list,v){return list.map(function(x){var k=x[0]!=null&&typeof x!=="string"?x[0]:x, l=typeof x==="string"?x:x[1];return '<option value="'+esc(k)+'"'+(k===v?" selected":"")+'>'+esc(l)+'</option>'}).join("")}
function psSection(title,hint,body,cls){return '<section class="ps-sec'+(cls?" "+cls:"")+'"><header class="ps-sec-h"><b>'+title+'</b><span>'+hint+'</span></header>'+body+'</section>'}
function projSettings(pid){var p=proj(pid); if(!p) return; var l=projSessions(pid), lead=l.filter(function(s){return s.lead})[0];
 var rooms=D.rooms.filter(function(r){return r.project===pid}), rts=(D.routines||[]).filter(function(r){return r.project===pid});
 var d=p.defaults||{agent:"Claude Code",model:"claude-opus · High",mode:"edits"};
 var fx=function(lbl,inner,full){return '<label class="ps-field'+(full?" full":"")+'"><span>'+lbl+'</span>'+inner+'</label>'};
 sheet('<div class="sheet-h ps-top"><b class="sheet-t ellipsis">'+esc(p.name)+' 설정</b><div class="sheet-acts"><button class="btn sm" data-act="closemodal">취소</button><button class="btn primary sm" data-act="pssave" data-pid="'+pid+'">저장</button></div><button class="icon-btn sm" data-act="closemodal" aria-label="닫기">'+I("close")+'</button></div>'+
 '<form class="ps-form" id="psf" onsubmit="return false">'+
 psSection("기본","프로젝트 이름과 저장소",'<div class="ps-grid">'+fx("이름",'<input class="field" name="name" value="'+esc(p.name)+'"'+(pid==="unsorted"?" disabled":"")+'>',true)+fx("저장소",'<input class="field" name="repo" value="'+esc(p.repo)+'">')+fx("기본 브랜치",'<input class="field mono" name="branch" value="'+esc(p.branch||"")+'">')+'</div>')+
 psSection("컴퓨터","이 프로젝트의 팀원이 일할 수 있는 컴퓨터",'<div class="ps-chips">'+D.computers.map(function(c){return '<label class="ps-check"><input type="checkbox" name="comp" value="'+c.id+'"'+(p.computers.indexOf(c.id)>=0?" checked":"")+'><span class="ps-ic">'+I(c.kind==="server"?"server":"pc")+'</span><span>'+c.id+'</span><span class="dot '+({online:"ok",offline:"off",unknown:""}[c.status])+'"></span></label>'}).join("")+'</div>')+
 psSection("새 팀원 기본값","새 팀원을 만들 때 처음 고르는 값",'<div class="ps-grid">'+'<div class="ps-field full"><span>하네스 · 모델 · 추론 강도</span>'+Ember.modelPicker.html({harness:d.agent,model:d.model,project:pid})+'<span class="small muted">계정은 하네스의 프로필을 따릅니다.</span>'+'</div><div class="ps-field full"><span>권한 모드</span>'+Ember.modePicker.html({harness:d.agent,mode:d.mode||({safe:"edits",auto:"bypass"}[d.policy])||"ask"})+'</div></div>')+

 psSection("프로필 덮어쓰기","선택 사항입니다. 이 프로젝트에서만 하네스에 다른 프로필을 씁니다",'<div class="ps-grid">'+Object.keys(Ember.modelPicker.models()).map(function(h){var base=typeof Ember.profileFor==="function"&&Ember.profileFor(h)||Ember.profileList(h)[0], cur=(p.profileOverride||{})[h]||"";
  return fx(h,'<select class="field" data-povr="'+esc(h)+'"><option value="">기본 ('+esc(base?base.name:"프로필 없음")+')</option>'+Ember.profileList(h).filter(function(x){return !base||x.id!==base.id}).map(function(x){return '<option value="'+esc(x.id)+'"'+(x.id===cur?" selected":"")+'>'+esc(x.name)+'</option>'}).join("")+'</select>')}).join("")+'</div>')+
 psSection("팀 캐릭터","고르면 이 프로젝트의 모든 팀원이 모든 화면에서 이 모양으로 보입니다",Ember.charStylePicker("charStyle",p.characterStyle||"mongle",lead||l[0]))+
 (pid==="unsorted"?psSection("리더 (오케스트레이터)","기본 프로젝트는 리더를 두지 않습니다",'<p class="ps-note">리더가 필요하면 프로젝트를 새로 만들어 옮기세요.</p>'):psSection("리더 (오케스트레이터)","다른 팀원에게 일을 나누고 모으는 팀원 한 명",(l.length?'<div class="ps-leads">'+l.map(function(s){return '<label class="ps-lead"><input type="radio" name="lead" value="'+s.id+'"'+(lead&&lead.id===s.id?" checked":"")+'>'+bot(s,30)+'<span class="col"><b>'+esc(mname(s))+'</b><span class="small muted">'+esc(s.title)+'</span></span></label>'}).join("")+'</div>':'<div class="empty">팀원이 없습니다</div>')))+
 psSection("연결","이 프로젝트에 묶인 채널, 루틴, 버전 관리",'<div class="ps-chips">'+rooms.map(function(r){return '<a class="chip outline" href="channels.html#'+r.id+'">'+I("hash")+esc(r.name)+'</a>'}).join("")+rts.map(function(r){return '<a class="chip outline" href="routines.html#'+r.id+'">'+I("repeat")+esc(r.name)+'</a>'}).join("")+'<a class="chip outline" href="vcs.html#'+pid+'">'+I("git")+'버전 관리</a></div>')+
 psSection("위험 영역","되돌리기 어려운 작업",'<div class="ps-danger"><div class="col" style="min-width:0"><b>프로젝트 보관</b><span class="small muted">세션과 기록은 남고 목록에서 숨겨집니다.</span></div><button class="btn sm danger" data-act="psarchive" data-pid="'+pid+'"'+(pid==="unsorted"?" disabled":"")+'>보관</button></div>'+
  '<div class="ps-danger"><div class="col" style="min-width:0"><b>프로젝트 삭제</b><span class="small muted">'+(pid==="unsorted"?"기본 프로젝트는 지울 수 없습니다.":"팀원 "+l.length+"명의 대화와 루틴 "+rts.length+"개가 함께 지워집니다.")+'</span></div><button class="btn sm primary danger" data-act="psdelete" data-pid="'+pid+'"'+(pid==="unsorted"?" disabled":"")+'>삭제</button></div>',"danger")+
 '</form>',"psmodal")}
function psSave(pid){var p=proj(pid), f=$("#psf"); if(!p||!f) return; var E=function(k){return f.elements.namedItem(k)};
 if(pid!=="unsorted"&&E("name").value.trim()) p.name=E("name").value.trim();
 p.repo=E("repo").value.trim(); p.branch=E("branch").value.trim();
 p.computers=[].filter.call(f.querySelectorAll('input[name=comp]'),function(x){return x.checked}).map(function(x){return x.value});
 p.defaults={agent:E("agent").value,model:E("model").value.trim(),mode:E("mode").value};
 var ov={}; [].forEach.call(f.querySelectorAll('select[data-povr]'),function(x){if(x.value) ov[x.dataset.povr]=x.value}); p.profileOverride=ov;
 var cs=f.querySelector('input[name=charStyle]:checked'); if(cs) p.characterStyle=cs.value;
 var ld=pid==="unsorted"?null:f.querySelector('input[name=lead]:checked'); if(ld) projSessions(pid).forEach(function(s){s.lead=(s.id===ld.value)});
 save(); closeSheet("psmodal"); toast("저장했습니다"); project()}

/* ---------- new project (minimal): name, repo, branch, team character ---------- */
function projNew(){
 sheet('<div class="sheet-h"><b class="sheet-t">새 프로젝트</b><div class="sheet-acts"><button class="btn sm" data-act="closemodal">취소</button><button class="btn primary sm" data-act="pncreate">만들기</button></div><button class="icon-btn sm" data-act="closemodal" aria-label="닫기">'+I("close")+'</button></div>'+
 '<form class="ps-form" id="pnf" onsubmit="return false">'+
 psSection("기본","프로젝트 이름과 저장소",'<div class="ps-grid"><label class="ps-field full"><span>이름</span><input class="field" name="name" required placeholder="예: darkpyonix-docs"></label><label class="ps-field"><span>저장소</span><input class="field" name="repo" placeholder="github.com/..."></label><label class="ps-field"><span>기본 브랜치</span><input class="field mono" name="branch" value="develop"></label></div>')+
 psSection("팀 캐릭터","이 프로젝트의 팀원이 모두 이 모양으로 보입니다. 나중에 설정에서 바꿀 수 있습니다",Ember.charStylePicker("charStyle","mongle"))+
 '</form>',"pnmodal"); var n=$("#pnf input[name=name]"); if(n) n.focus()}
function projCreate(){var f=$("#pnf"); if(!f) return; var E=function(k){return f.elements.namedItem(k)}, nm=E("name").value.trim();
 if(!nm){E("name").focus(); toast("이름을 입력하세요"); return}
 var id="p"+Date.now(), cs=f.querySelector('input[name=charStyle]:checked');
 D.projects.splice(Math.max(0,D.projects.length-1),0,{id:id, characterStyle:cs?cs.value:"mongle", short:nm.replace(/^darkpyonix-/,""), name:nm, repo:E("repo").value.trim()||"저장소 없음", branch:E("branch").value.trim(), computers:["mac-mini"]});
 save(); closeSheet("pnmodal"); toast("프로젝트를 만들었습니다"); project()}

Ember.screens.project={render:project,onHash:project,click:function(e,t){var a;
 if((a=t.closest("[data-comp]"))){if(Ember.monitor&&Ember.monitor.open) Ember.monitor.open(a.dataset.comp); else compModal(a.dataset.comp); return true}
 if((a=t.closest("[data-psettings]"))){projSettings(a.dataset.psettings); return true}
 if((a=t.closest("[data-act=pssave]"))){psSave(a.dataset.pid); return true}
 if((a=t.closest("[data-act=pnew]"))){projNew(); return true}
 if((a=t.closest("[data-act=pncreate]"))){projCreate(); return true}
 if((a=t.closest("[data-act=psarchive]"))){toast("보관했습니다 (데모)"); return true}
 if((a=t.closest("[data-act=psdelete]"))){toast("삭제는 데모에서 막아 두었습니다"); return true}
 if((a=t.closest("[data-scroll]"))){var sc=a.closest(".cstrip-wrap").querySelector(".cstrip"); sc.scrollBy({left:+a.dataset.scroll*sc.clientWidth*0.8,behavior:"smooth"}); return true}
 if((a=t.closest("[data-expand]"))){expanded[a.dataset.expand]=!expanded[a.dataset.expand]; var pb=a.closest(".pbox"); var tmp=document.createElement("div"); tmp.innerHTML=projBox(proj(a.dataset.expand)); pb.replaceWith(tmp.firstChild); return true}
 if((a=t.closest("[data-proj]"))){location.hash=a.dataset.proj; return true}
 if((a=t.closest("[data-memf]"))){memFilter=a.dataset.memf; a.parentNode.querySelectorAll("button").forEach(function(b){b.classList.toggle("on",b===a)}); $("#mlist").innerHTML=memberList(location.hash.slice(1),$("#mq").value.toLowerCase()); return true}
 if((a=t.closest("[data-act=addcomp]"))){toast("컴퓨터 추가: 페어링 코드 표시 (데모)"); return true}
}};
