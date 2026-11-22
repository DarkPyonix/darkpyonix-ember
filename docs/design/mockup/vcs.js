"use strict";
/* ---------- 버전 관리 (vcs): GitHub Desktop level Git per project. Owned by the version-control agent. ----------
   Data: D.vcs[projectId] (data.js vcs block). URL: vcs.html#<project>/<tab>. Every name here starts with vcs. */
var VCS_TABS=[["changes","변경 사항","file"],["history","히스토리","clock"],["branches","브랜치","git"],["prs","풀 리퀘스트","pr"],["stash","스태시","stash"],["conflict","충돌","warn"],["team","팀 작업","team"]];
var vcsRoot=null, vcsOpts={}, vcsPage=null, vcsP="ember", vcsTab="changes", vcsFile=0, vcsCommit=0, vcsPR=null, vcsSub="a", vcsPop=false, vcsSummary="", vcsBody="", vcsAmend=false, vcsBrQ="", vcsFold={}, vcsCFile=0;
function vcsR(){return D.vcs[vcsP]}
function vcsWho(who,size){var s=sess(who); if(s&&s.lead===undefined){var ds=(window.EMBER_DEMO.sessions||[]).filter(function(x){return x.id===who})[0]; if(ds&&ds.lead) s.lead=true} return s?bot(s,size):'<span class="vcs-me" style="width:'+size+'px;height:'+size+'px">'+esc(who[0])+'</span>'}
function vcsWhoName(who){var s=sess(who); return s?mname(s):who}
function vcsSave(){save()}
function vcsHash(){try{history.replaceState(null,"","#"+vcsP+"/"+vcsTab)}catch(e){}}
function vcsReadHash(){var h=location.hash.slice(1).split("/"); if(h[0]&&(D.vcs[h[0]]||h[0]==="unsorted")) vcsP=h[0]; else vcsP=lsGet("ember-mock-vcs-proj",D.vcs[lastProj()]?lastProj():"ember");
 if(h[1]&&VCS_TABS.some(function(t){return t[0]===h[1]})) vcsTab=h[1]}

/* ---------- counts and helpers ---------- */
function vcsLines(c){var n=0,on=0; c.hunks.forEach(function(h,hi){h.lines.forEach(function(l,li){if(l[0]!==" "){n++; if(!(c.off&&c.off[hi+":"+li])) on++}})}); return {n:n,on:on}}
function vcsStaged(r){return r.changes.reduce(function(a,c){return a+(vcsLines(c).on>0?1:0)},0)}
function vcsOpenPRs(r){return r.prs.filter(function(p){return p.state==="open"}).length}
function vcsCount(t){var r=vcsR(); return {changes:r.changes.length,history:"",branches:r.branches.length,prs:vcsOpenPRs(r),stash:r.stashes.length,conflict:r.conflict?r.conflict.files.length:0,team:r.worktrees.length}[t]}
var VCS_ST={M:["수정","orange"],A:["추가","green"],D:["삭제","red"],R:["이름 변경","blue"]};
var VCS_CI={ok:["통과","green","check"],run:["실행 중","blue",""],wait:["대기","",""],fail:["실패","red","close"]};
var VCS_RV={approved:["승인","green"],changes:["변경 요청","orange"],pending:["리뷰 대기",""],none:["리뷰어 없음",""]};
function vcsCiPill(ci){var c=VCS_CI[ci]; return '<span class="pill '+c[1]+'">'+(ci==="run"?'<span class="spin"></span>':c[2]?I(c[2]):'<span class="dot"></span>')+'CI '+c[0]+'</span>'}
function vcsAB(a,b){return '<span class="vcs-ab" title="앞섬 '+a+' · 뒤처짐 '+b+'"><span class="'+(a?"on":"")+'">'+I("up")+a+'</span><span class="'+(b?"on":"")+'">'+I("down")+b+'</span></span>'}

/* ---------- toolbar: repository, branch, remote ---------- */
function vcsBar(){var r=vcsR(), p=proj(vcsP);
 var opts=D.projects.filter(function(x){return D.vcs[x.id]}).map(function(x){return '<option value="'+x.id+'"'+(x.id===vcsP?" selected":"")+'>'+esc(x.name)+'</option>'}).join("");
 var main=r.behind?["pull","down","Pull",r.behind]:r.ahead?["push","up","Push",r.ahead]:["fetch","fetch","Fetch",0];
 return '<div class="vcs-bar card">'+
 (vcsOpts.compact?'<div class="vcs-cell vcs-repo"><span class="vcs-k">저장소</span><span class="vcs-sel">'+I("folder")+'<b class="ellipsis">'+esc(p.name)+'</b></span>':'<label class="vcs-cell vcs-repo"><span class="vcs-k">저장소</span><span class="vcs-sel">'+I("folder")+'<select id="vcsProj" aria-label="프로젝트 저장소">'+opts+'</select><span class="vcs-chev">'+I("chev")+'</span></span>')+'<span class="vcs-sub ellipsis">'+esc(p.repo)+'</span>'+(vcsOpts.compact?'</div>':'</label>')+
 '<div class="vcs-cell vcs-brcell"><span class="vcs-k">현재 브랜치</span><button class="vcs-sel" data-vcs="pop" aria-haspopup="true" aria-expanded="'+vcsPop+'">'+I("git")+'<b class="ellipsis">'+esc(r.branch)+'</b><span class="vcs-chev">'+I("chev")+'</span></button><span class="vcs-sub ellipsis">'+esc(r.upstream)+' 추적</span>'+(vcsPop?vcsPopHtml():"")+'</div>'+
 '<div class="vcs-cell vcs-remote"><span class="vcs-k">원격 origin</span><div class="flex vcs-rbtns">'+
  '<button class="btn sm primary" data-vcs="'+main[0]+'">'+I(main[1])+main[2]+(main[3]?' <span class="vcs-n">'+main[3]+'</span>':"")+'</button>'+
  '<span class="vcs-group"><button class="icon-btn sm" data-vcs="fetch" title="Fetch" aria-label="Fetch">'+I("fetch")+'</button><button class="icon-btn sm" data-vcs="pull" title="Pull" aria-label="Pull">'+I("down")+'</button><button class="icon-btn sm" data-vcs="push" title="Push" aria-label="Push">'+I("up")+'</button></span></div>'+
  '<span class="vcs-sub">'+vcsAB(r.ahead,r.behind)+' · '+esc(r.fetched)+' fetch</span></div>'+
 '</div>'}
function vcsPopHtml(){var r=vcsR();
 return '<div class="vcs-pop card" role="dialog" aria-label="브랜치 전환"><div class="vcs-pophead"><label class="vcs-search">'+I("search")+'<input id="vcsBrQ" placeholder="브랜치 찾기" value="'+esc(vcsBrQ)+'"></label></div><div class="vcs-poplist" id="vcsPopList">'+vcsPopList()+'</div>'+
 '<div class="vcs-popfoot"><input class="field sm" id="vcsNewBr2" placeholder="feat/새-브랜치"><button class="btn sm" data-vcs="create2">'+I("plus")+'만들기</button></div></div>'}
function vcsPopList(){var r=vcsR(), q=vcsBrQ.toLowerCase();
 var l=r.branches.filter(function(b){return !q||b.name.toLowerCase().indexOf(q)>=0});
 return l.map(function(b){return '<button class="vcs-poprow'+(b.name===r.branch?" on":"")+'" data-vcs="switch" data-b="'+esc(b.name)+'">'+(b.name===r.branch?I("check"):I("git"))+'<span class="ellipsis spacer">'+esc(b.name)+'</span>'+(b.by?vcsWho(b.by,20):"")+'<span class="small muted">'+esc(b.when)+'</span></button>'}).join("")||'<div class="empty small">일치하는 브랜치가 없습니다</div>'}

/* ---------- section nav ---------- */
function vcsNav(){return '<nav class="vcs-nav" aria-label="버전 관리 섹션">'+VCS_TABS.map(function(t){var n=vcsCount(t[0]);
 return '<button class="vcs-navi'+(t[0]===vcsTab?" on":"")+(t[0]==="conflict"&&n?" alert":"")+'" data-vcs="tab" data-t="'+t[0]+'">'+I(t[2])+'<span class="lbl">'+t[1]+'</span>'+(n!==""&&n?'<span class="vcs-cnt">'+n+'</span>':"")+'</button>'}).join("")+'</nav>'}
function vcsSubs(list){return '<div class="vcs-subs seg" role="tablist">'+list.map(function(x){return '<button class="'+(vcsSub===x[0]?"on":"")+'" data-vcs="sub" data-s="'+x[0]+'">'+x[1]+'</button>'}).join("")+'</div>'}

/* ---------- changes: tree, diff with line staging, commit ---------- */
function vcsTree(changes){var root={d:{},f:[]};
 changes.forEach(function(c,i){var parts=c.path.split("/"), n=root; parts.slice(0,-1).forEach(function(p){n.d[p]=n.d[p]||{d:{},f:[]}; n=n.d[p]}); n.f.push(i)});
 function walk(node,name,path,depth){var out="";
  Object.keys(node.d).sort().forEach(function(k){var ch=node.d[k], label=k, pth=path+k+"/";
   while(Object.keys(ch.d).length===1&&!ch.f.length){var k2=Object.keys(ch.d)[0]; label+="/"+k2; pth+=k2+"/"; ch=ch.d[k2]}
   var closed=vcsFold[pth];
   out+='<button class="vcs-dir" style="--d:'+depth+'" data-vcs="fold" data-p="'+esc(pth)+'" aria-expanded="'+!closed+'">'+'<span class="vcs-chev">'+I("chev")+'</span>'+I("tree")+'<span class="ellipsis">'+esc(label)+'</span></button>';
   if(!closed) out+=walk(ch,label,pth,depth+1)});
  node.f.forEach(function(i){var c=changes[i], L=vcsLines(c), st=VCS_ST[c.st], state=L.on===0?"":L.on===L.n?"on":"part";
   out+='<div class="vcs-file'+(i===vcsFile?" sel":"")+'" style="--d:'+depth+'" data-vcs="file" data-i="'+i+'"><button class="vcs-cb '+state+'" data-vcs="fcheck" data-i="'+i+'" role="checkbox" aria-checked="'+(state==="part"?"mixed":!!state)+'" aria-label="'+esc(c.path)+' 스테이징"></button><span class="ellipsis spacer">'+esc(c.path.split("/").pop())+'</span><span class="vcs-st '+st[1]+'" title="'+st[0]+'">'+c.st+'</span></div>'});
  return out}
 return walk(root,"","",0)}
function vcsDiff(c){if(!c) return '<div class="empty">변경 사항이 없습니다. 작업 트리가 깨끗합니다.</div>';
 var L=vcsLines(c);
 return '<div class="vcs-diffhead"><span class="vcs-st '+VCS_ST[c.st][1]+'">'+c.st+'</span><b class="mono ellipsis spacer">'+esc(c.path)+'</b><span class="small vcs-add">+'+c.add+'</span><span class="small vcs-del">-'+c.del+'</span><span class="pill">'+L.on+'/'+L.n+'줄 스테이징</span></div>'+
 '<div class="vcs-diff" id="vcsDiff" data-keep>'+c.hunks.map(function(h,hi){
  return '<div class="vcs-hunk"><div class="vcs-hh"><span class="mono">'+esc(h.h)+'</span><button class="chip" data-vcs="hunk" data-h="'+hi+'">이 덩어리 전체</button></div><div class="vcs-code">'+h.lines.map(function(l,li){var k=hi+":"+li, ch=l[0]!==" ", on=ch&&!(c.off&&c.off[k]);
   return '<div class="vcs-ln '+(l[0]==="+"?"add":l[0]==="-"?"del":"ctx")+(ch?(on?" on":" off"):"")+'"'+(ch?' data-vcs="line" data-k="'+k+'" role="checkbox" aria-checked="'+on+'" tabindex="0"':"")+'><span class="vcs-g">'+(ch?'<i></i>':"")+'</span><span class="vcs-no">'+(l[2]||"")+'</span><span class="vcs-no">'+(l[3]||"")+'</span><span class="vcs-sg">'+(l[0]===" "?"":l[0])+'</span><span class="vcs-tx">'+esc(l[1])+'</span></div>'}).join("")+'</div></div>'}).join("")+'</div>'}
function vcsCommitBox(){var r=vcsR(), n=vcsStaged(r);
 return '<div class="vcs-commit"><div class="flex"><span class="vcs-av">'+av("BREW","round sm")+'</span><input class="field sm spacer" id="vcsSum" placeholder="요약 (필수) 예: Feat: Wire shim" value="'+esc(vcsSummary)+'" maxlength="72"></div>'+
 '<textarea class="field" id="vcsBody" placeholder="설명 (선택)">'+esc(vcsBody)+'</textarea>'+
 '<label class="vcs-amend small"><input type="checkbox" id="vcsAmend"'+(vcsAmend?" checked":"")+'> 마지막 커밋 수정 (amend)</label>'+
 '<button class="btn primary vcs-cbtn" data-vcs="commit"'+(n?"":" disabled")+'>'+I("commit")+'<span><b>'+esc(r.branch)+'</b>에 커밋'+(n?' ('+n+'개 파일)':'')+'</span></button>'+
 '<div class="small muted">'+(r.ahead?'커밋 '+r.ahead+'개가 아직 push되지 않았습니다':'원격과 같습니다')+'</div></div>'}
function vcsChanges(){var r=vcsR(); if(vcsFile>=r.changes.length) vcsFile=0; var all=r.changes.length&&r.changes.every(function(c){var L=vcsLines(c);return L.on===L.n}), some=r.changes.some(function(c){return vcsLines(c).on});
 return vcsSubs([["a","파일 "+r.changes.length],["b","차이"],["c","커밋"]])+
 '<div class="vcs-chg sub-'+vcsSub+'"><section class="card vcs-files vcs-pa"><div class="vcs-ph"><button class="vcs-cb '+(all?"on":some?"part":"")+'" data-vcs="all" role="checkbox" aria-label="모두 스테이징"></button><b>변경 파일 '+r.changes.length+'</b><div class="spacer"></div><button class="icon-btn sm" data-vcs="stashnow" title="변경 사항 스태시" aria-label="변경 사항 스태시">'+I("stash")+'</button></div>'+
 '<div class="vcs-tree" id="vcsTree" data-keep>'+(r.changes.length?vcsTree(r.changes):'<div class="empty small">변경 없음</div>')+'</div>'+vcsCommitBox()+'</section>'+
 '<section class="card vcs-diffcard vcs-pb">'+vcsDiff(r.changes[vcsFile])+'</section>'+'</div>'}

/* ---------- history graph ---------- */
var VCS_ROW=56, VCS_LW=16;
function vcsGraph(cs){var idx={}; cs.forEach(function(c,i){idx[c.sha]=i});
 var lanes=Math.max.apply(null,cs.map(function(c){return c.lane}))+1, W=VCS_LW*lanes+14, H=cs.length*VCS_ROW, x=function(l){return 12+l*VCS_LW}, y=function(i){return i*VCS_ROW+VCS_ROW/2};
 var col=["var(--c-blue)","var(--c-orange)","var(--text-2)","var(--c-green)","var(--c-sky)","var(--c-red)"];
 var paths="", dots="";
 cs.forEach(function(c,i){c.parents.forEach(function(ps,pi){var j=idx[ps]; if(j==null) return; var x1=x(c.lane),y1=y(i),x2=x(cs[j].lane),y2=y(j),d,R=VCS_ROW*0.6, lc=col[(pi===0?c.lane:cs[j].lane)%col.length];
   if(x1===x2) d="M"+x1+" "+y1+"V"+y2;
   else if(pi===0) d="M"+x1+" "+y1+"V"+(y2-R)+"C"+x1+" "+(y2-R/3)+" "+x2+" "+(y2-R*0.6)+" "+x2+" "+y2;
   else d="M"+x1+" "+y1+"C"+x1+" "+(y1+R*0.6)+" "+x2+" "+(y1+R/3)+" "+x2+" "+(y1+R)+"V"+y2;
   paths+='<path d="'+d+'" stroke="'+lc+'"/>'});
  var merge=c.parents.length>1;
  dots+='<circle cx="'+x(c.lane)+'" cy="'+y(i)+'" r="'+(merge?4:5)+'" class="'+(merge?"m":"")+(c.refs&&c.refs.indexOf("HEAD")>=0?" head":"")+'" style="--lc:'+col[c.lane%col.length]+'"/>'});
 return {svg:'<svg class="vcs-graph" width="'+W+'" height="'+H+'" viewBox="0 0 '+W+' '+H+'" aria-hidden="true"><g class="ln">'+paths+'</g><g class="dt">'+dots+'</g></svg>',w:W}}
function vcsRef(n){var cls=n==="HEAD"?"head":/^origin\//.test(n)?"remote":/^(main|develop|release)$/.test(n)?"base":"";
 return '<span class="vcs-ref '+cls+'">'+(n==="HEAD"?"HEAD":esc(n))+'</span>'}
function vcsHistory(){var r=vcsR(), cs=r.commits, g=vcsGraph(cs); if(vcsCommit>=cs.length) vcsCommit=0; var c=cs[vcsCommit];
 var rows=cs.map(function(c,i){return '<div class="vcs-crow'+(i===vcsCommit?" sel":"")+'" data-vcs="commitsel" data-i="'+i+'" tabindex="0"><div class="col spacer" style="min-width:0"><span class="ellipsis vcs-cmsg">'+(c.local?'<span class="vcs-up" title="push 안 됨">'+I("up")+'</span>':"")+esc(c.msg)+'</span><span class="small muted flex vcs-cmeta">'+vcsWho(c.who,16)+'<span class="ellipsis">'+esc(vcsWhoName(c.who))+' · '+esc(c.time)+'</span><span class="mono">'+c.sha+'</span></span></div><div class="vcs-refs">'+(c.refs||[]).map(vcsRef).join("")+'</div></div>'}).join("");
 var files=(c.files||[["ember/proxy/src/shim.rs","M",14,3],["ember/proxy/tests/shim_attach.rs","A",22,0]]).map(function(f){return '<div class="vcs-hf"><span class="vcs-st '+VCS_ST[f[1]][1]+'">'+f[1]+'</span><span class="mono ellipsis spacer">'+esc(f[0])+'</span><span class="small vcs-add">+'+f[2]+'</span><span class="small vcs-del">-'+f[3]+'</span></div>'}).join("");
 return vcsSubs([["a","그래프"],["b","커밋 상세"]])+
 '<div class="vcs-two sub-'+vcsSub+'"><section class="card vcs-pa vcs-hist"><div class="vcs-ph"><b>히스토리</b><span class="small muted">모든 브랜치 · '+cs.length+'개</span><div class="spacer"></div><span class="vcs-legend small"><i class="lg-l"></i>로컬 전용</span></div><div class="vcs-hscroll" id="vcsHist" data-keep><div class="vcs-hwrap" style="--gw:'+g.w+'px">'+g.svg+'<div class="vcs-hrows">'+rows+'</div></div></div></section>'+
 '<section class="card vcs-pb vcs-cdet"><div class="vcs-ph"><span class="mono">'+c.sha+'</span><div class="spacer"></div>'+(c.local?'<span class="pill orange">push 안 됨</span>':'<span class="pill">원격에 있음</span>')+'</div><div class="card-b col" style="gap:12px">'+
 '<b class="vcs-ctitle">'+esc(c.msg)+'</b>'+(c.body?'<div class="muted">'+esc(c.body)+'</div>':"")+
 '<div class="flex">'+vcsWho(c.who,28)+'<div class="col"><b class="small">'+esc(vcsWhoName(c.who))+'</b><span class="small muted">'+esc(c.time)+(c.parents.length>1?' · 병합 커밋 (부모 '+c.parents.join(", ")+')':' · 부모 '+(c.parents[0]||"없음"))+'</span></div></div>'+
 '<div class="vcs-refs">'+(c.refs||[]).map(vcsRef).join("")+'</div>'+
 '<div class="inset vcs-hfs">'+files+'</div>'+
 '<div class="flex wrap"><button class="btn sm" data-vcs="toast" data-m="'+c.sha+'에서 브랜치 만들기 (데모)">'+I("git")+'여기서 브랜치</button><button class="btn sm" data-vcs="toast" data-m="'+c.sha+' 되돌리기 커밋을 만들었습니다 (데모)">되돌리기</button><button class="btn sm" data-vcs="toast" data-m="'+c.sha+' cherry-pick (데모)">Cherry-pick</button><button class="btn sm" data-vcs="toast" data-m="SHA '+c.sha+' 복사됨">SHA 복사</button></div>'+
 '</div></section></div>'}

/* ---------- branches ---------- */
function vcsBrRow(b,r){var cur=b.name===r.branch, wt=r.worktrees.filter(function(w){return w.branch===b.name})[0], tot=Math.max(1,b.ahead+b.behind);
 return '<div class="vcs-brow'+(cur?" cur":"")+'"><span class="vcs-bico">'+(cur?I("check"):I("git"))+'</span><div class="col spacer" style="min-width:0"><span class="flex" style="min-width:0"><b class="mono ellipsis">'+esc(b.name)+'</b>'+(b.protected?'<span class="pill">보호됨</span>':"")+(b.merged?'<span class="pill green">병합됨</span>':"")+(cur?'<span class="pill blue">현재</span>':"")+'</span>'+
 '<span class="small muted ellipsis">'+(b.base?b.base+' 기준 · ':"")+esc(b.when)+(wt?' · worktree '+esc(wt.computer):"")+'</span></div>'+
 (b.base?'<div class="vcs-abbar" title="'+b.base+' 대비 뒤처짐 '+b.behind+' · 앞섬 '+b.ahead+'"><span class="bh"><i style="width:'+(b.behind/tot*100)+'%"></i></span><span class="ah"><i style="width:'+(b.ahead/tot*100)+'%"></i></span><span class="small muted">'+b.behind+' | '+b.ahead+'</span></div>':'<div class="vcs-abbar"></div>')+
 '<span class="vcs-bwho">'+(b.by?vcsWho(b.by,26):"")+'</span>'+
 (cur?'<span class="vcs-bact small muted">체크아웃됨</span>':'<button class="btn sm vcs-bact" data-vcs="switch" data-b="'+esc(b.name)+'">전환</button>')+'</div>'}
function vcsBranches(){var r=vcsR(), base=r.branches.filter(function(b){return b.protected}), mine=r.branches.filter(function(b){return !b.protected});
 return '<div class="vcs-stack"><section class="card"><div class="vcs-ph"><b>새 브랜치</b></div><div class="card-b vcs-newbr"><input class="field sm mono" id="vcsNewBr" placeholder="feat/session-search"><label class="small muted">기준<select class="field sm" id="vcsBase">'+r.branches.map(function(b){return '<option'+(b.name===r.branch?" selected":"")+'>'+esc(b.name)+'</option>'}).join("")+'</select></label><button class="btn primary sm" data-vcs="create">'+I("plus")+'만들고 전환</button></div></section>'+
 '<section class="card"><div class="vcs-ph"><b>기본 브랜치</b><span class="small muted">'+base.length+'</span></div>'+base.map(function(b){return vcsBrRow(b,r)}).join("")+'</section>'+
 '<section class="card"><div class="vcs-ph"><b>작업 브랜치</b><span class="small muted">'+mine.length+' · 막대는 기준 대비 뒤처짐 | 앞섬</span></div>'+mine.map(function(b){return vcsBrRow(b,r)}).join("")+'</section>'+
 '<section class="card"><div class="vcs-ph"><b>원격 브랜치</b><span class="small muted">origin</span></div><div class="card-b flex wrap">'+r.remotes.map(function(x){return vcsRef(x)}).join("")+'</div></section></div>'}

/* ---------- pull requests ---------- */
function vcsPRs(){var r=vcsR(); if(!r.prs.length) return '<div class="card card-b empty">풀 리퀘스트가 없습니다</div>';
 if(vcsPR==null||!r.prs.some(function(p){return p.n===vcsPR})) vcsPR=r.prs[0].n; var p=r.prs.filter(function(x){return x.n===vcsPR})[0];
 var rows=r.prs.map(function(x){return '<div class="vcs-prrow'+(x.n===vcsPR?" sel":"")+'" data-vcs="prsel" data-n="'+x.n+'" tabindex="0">'+vcsWho(x.by,30)+'<div class="col spacer" style="min-width:0"><span class="ellipsis"><b>'+esc(x.title)+'</b></span><span class="small muted ellipsis">#'+x.n+' · '+esc(x.branch)+' → '+esc(x.base)+'</span><span class="flex wrap vcs-prp">'+(x.state==="merged"?'<span class="pill sky">병합됨</span>':x.draft?'<span class="pill">초안</span>':'<span class="pill green">열림</span>')+(x.state==="open"?vcsCiPill(x.ci)+'<span class="pill '+VCS_RV[x.review][1]+'">'+VCS_RV[x.review][0]+'</span>':"")+'</span></div></div>'}).join("");
 var canMerge=p.state==="open"&&p.ci==="ok"&&p.review==="approved";
 var det='<div class="vcs-ph"><span class="mono">#'+p.n+'</span><b class="ellipsis spacer">'+esc(p.title)+'</b></div><div class="card-b col" style="gap:14px">'+
 '<div class="flex wrap small muted">'+vcsWho(p.by,22)+esc(vcsWhoName(p.by))+' · <span class="mono">'+esc(p.branch)+'</span> → <span class="mono">'+esc(p.base)+'</span> · 파일 '+p.files+' <span class="vcs-add">+'+p.add+'</span> <span class="vcs-del">-'+p.del+'</span></div>'+
 '<div class="vcs-prgrid"><div class="inset"><div class="flex"><b class="small">검사</b><div class="spacer"></div>'+vcsCiPill(p.ci)+'</div>'+p.checks.map(function(k){var c=VCS_CI[k[1]];return '<div class="vcs-check">'+(k[1]==="run"?'<span class="spin"></span>':'<span class="dot '+({ok:"ok",fail:"fail",wait:"",run:"run"}[k[1]])+'"></span>')+'<span class="ellipsis spacer small">'+esc(k[0])+'</span><span class="small muted">'+c[0]+'</span></div>'}).join("")+'</div>'+
 '<div class="inset"><div class="flex"><b class="small">리뷰</b><div class="spacer"></div><span class="pill '+VCS_RV[p.review][1]+'">'+VCS_RV[p.review][0]+'</span></div>'+(p.reviewers.length?p.reviewers.map(function(v){return '<div class="vcs-check">'+vcsWho(v[0],20)+'<span class="ellipsis spacer small">'+esc(vcsWhoName(v[0]))+'</span><span class="small '+(v[1]==="approved"?"vcs-add":v[1]==="changes"?"vcs-warn":"muted")+'">'+({approved:"승인",changes:"변경 요청",pending:"대기"}[v[1]])+'</span></div>'}).join(""):'<div class="small muted vcs-check">리뷰어 없음</div>')+'</div></div>'+
 (p.comments.length?'<div class="col" style="gap:8px"><b class="small">리뷰 코멘트</b>'+p.comments.map(function(m){return '<div class="vcs-cmt">'+vcsWho(m.who,26)+'<div class="col spacer" style="min-width:0"><span class="small muted ellipsis"><b>'+esc(vcsWhoName(m.who))+'</b> · <span class="mono">'+esc(m.file||"")+(m.line?":"+m.line:"")+'</span></span><span>'+esc(m.text)+'</span></div></div>'}).join("")+'</div>':"")+
 (p.state==="open"?'<div class="vcs-review"><textarea class="field" id="vcsRv" placeholder="리뷰 남기기 (에이전트에게 바로 전달됩니다)"></textarea><div class="flex wrap"><button class="btn sm" data-vcs="rv" data-k="comment">코멘트</button><button class="btn sm" data-vcs="rv" data-k="changes">변경 요청</button><button class="btn sm" data-vcs="rv" data-k="approved">'+I("check")+'승인</button><div class="spacer"></div><button class="btn sm primary" data-vcs="merge"'+(canMerge?"":" disabled")+' title="'+(canMerge?"병합":"CI 통과와 승인이 있어야 병합할 수 있습니다")+'">'+I("pr")+'병합</button></div>'+(canMerge?"":'<span class="small muted">병합 조건: 모든 검사 통과와 승인. 지금 '+(p.ci!=="ok"?"CI가 끝나지 않았거나 실패했습니다":"승인이 없습니다")+'.</span>')+'</div>':'<div class="small muted">'+esc(p.base)+'에 병합되었습니다. 브랜치는 정리 대상입니다.</div>')+
 '</div>';
 return vcsSubs([["a","목록 "+r.prs.length],["b","#"+p.n]])+'<div class="vcs-two sub-'+vcsSub+'"><section class="card vcs-pa"><div class="vcs-ph"><b>풀 리퀘스트</b><span class="small muted">열림 '+vcsOpenPRs(r)+'</span><div class="spacer"></div><button class="btn sm" data-vcs="newpr">'+I("plus")+'새 PR</button></div><div class="vcs-prlist">'+rows+'</div></section><section class="card vcs-pb">'+det+'</section></div>'}

/* ---------- stash ---------- */
function vcsStash(){var r=vcsR();
 return '<div class="vcs-stack"><section class="card"><div class="vcs-ph"><b>스태시</b><span class="small muted">'+r.stashes.length+'</span><div class="spacer"></div><button class="btn sm primary" data-vcs="stashnow"'+(r.changes.length?"":" disabled")+'>'+I("stash")+'변경 사항 '+r.changes.length+'개 스태시</button></div>'+
 (r.stashes.length?r.stashes.map(function(s,i){return '<div class="vcs-brow"><span class="vcs-bico">'+I("stash")+'</span><div class="col spacer" style="min-width:0"><b class="ellipsis">'+esc(s.msg)+'</b><span class="small muted ellipsis"><span class="mono">'+esc(s.id)+'</span> · '+esc(s.branch)+' · 파일 '+s.files+' · '+esc(s.when)+'</span></div><div class="flex vcs-sact"><button class="btn sm" data-vcs="sapply" data-i="'+i+'">적용</button><button class="btn sm" data-vcs="spop" data-i="'+i+'">꺼내기</button><button class="icon-btn sm" data-vcs="sdrop" data-i="'+i+'" title="버리기" aria-label="버리기">'+I("close")+'</button></div></div>'}).join(""):'<div class="empty">스태시가 없습니다</div>')+
 '</section><div class="small muted vcs-note">적용은 스태시를 남기고, 꺼내기는 적용한 뒤 지웁니다. 브랜치를 바꿀 때 변경 사항이 있으면 여기로 옮길지 묻습니다.</div></div>'}

/* ---------- conflicts ---------- */
function vcsConflict(){var r=vcsR(), cf=r.conflict;
 if(!cf) return '<div class="card card-b empty">'+I("check")+' 충돌이 없습니다</div>';
 if(vcsCFile>=cf.files.length) vcsCFile=0; var f=cf.files[vcsCFile];
 var done=function(x){return x.blocks.every(function(b){return b.pick})}, left=cf.files.filter(function(x){return !done(x)}).length;
 var wt=r.worktrees.filter(function(w){return w.branch===cf.into})[0];
 return '<div class="vcs-banner card"><span class="vcs-wico">'+I("warn")+'</span><div class="col spacer" style="min-width:0"><b class="ellipsis"><span class="mono">'+esc(cf.from)+'</span> → <span class="mono">'+esc(cf.into)+'</span> 병합 중</b><span class="small muted">'+(wt?esc(vcsWhoName(wt.who))+'의 worktree · '+esc(wt.computer)+' · ':"")+'남은 충돌 파일 '+left+'개</span></div>'+(wt?vcsWho(wt.who,32):"")+'<button class="btn sm" data-vcs="abort">병합 취소</button><button class="btn sm primary" data-vcs="resolve"'+(left?" disabled":"")+'>병합 커밋</button></div>'+
 '<div class="vcs-cf"><nav class="card vcs-cflist">'+cf.files.map(function(x,i){return '<button class="vcs-file'+(i===vcsCFile?" sel":"")+'" data-vcs="cfile" data-i="'+i+'">'+(done(x)?'<span class="vcs-ok">'+I("check")+'</span>':'<span class="vcs-wico sm">'+I("warn")+'</span>')+'<span class="mono ellipsis spacer">'+esc(x.path)+'</span></button>'}).join("")+'</nav>'+
 '<section class="card vcs-cfmain"><div class="vcs-ph"><b class="mono ellipsis spacer">'+esc(f.path)+'</b><button class="chip" data-vcs="pickall" data-k="ours">모두 현재 것</button><button class="chip" data-vcs="pickall" data-k="theirs">모두 들어오는 것</button></div>'+
 f.blocks.map(function(b,bi){var code=function(ls,cls){return '<div class="vcs-code">'+ls.map(function(t){return '<div class="vcs-ln '+cls+'"><span class="vcs-tx">'+esc(t)+'</span></div>'}).join("")+'</div>'}, ctx=code(b.ctx,"ctx");
  if(b.pick==="none") return '<div class="vcs-cblock">'+ctx+'</div>';
  var res=b.pick==="ours"?b.ours:b.pick==="theirs"?b.theirs:b.pick==="both"?b.ours.concat(b.theirs):null;
  return '<div class="vcs-cblock">'+ctx+'<div class="vcs-sides"><div class="vcs-side ours'+(b.pick==="ours"||b.pick==="both"?" picked":"")+'"><div class="vcs-sh"><b class="small">현재 ('+esc(cf.into)+')</b><button class="chip" data-vcs="pick" data-b="'+bi+'" data-k="ours">이것 사용</button></div>'+code(b.ours,"del")+'</div>'+
  '<div class="vcs-side theirs'+(b.pick==="theirs"||b.pick==="both"?" picked":"")+'"><div class="vcs-sh"><b class="small">들어오는 것 ('+esc(cf.from)+')</b><button class="chip" data-vcs="pick" data-b="'+bi+'" data-k="theirs">이것 사용</button></div>'+code(b.theirs,"add")+'</div></div>'+
  '<div class="vcs-result"><div class="vcs-sh"><b class="small">결과</b><button class="chip" data-vcs="pick" data-b="'+bi+'" data-k="both">둘 다</button>'+(b.pick?'<button class="chip" data-vcs="pick" data-b="'+bi+'" data-k="">다시 고르기</button>':"")+'</div>'+(res?code(res,"ctx"):'<div class="small muted vcs-pending">아직 고르지 않았습니다</div>')+'</div></div>'}).join("")+
 '</section></div>'}

/* ---------- team worktrees ---------- */
function vcsTeam(){var r=vcsR(), ws=r.worktrees.slice().sort(function(a,b){var A=sess(a.who),B=sess(b.who); return ((B&&B.lead)?1:0)-((A&&A.lead)?1:0)});
 return '<div class="vcs-team">'+ws.map(function(w){var s=sess(w.who), pr=r.prs.filter(function(p){return p.branch===w.branch})[0];
  return '<article class="card vcs-wt'+(w.conflict?" warn":"")+'"><div class="flex">'+vcsWho(w.who,40)+'<div class="col spacer" style="min-width:0"><b class="ellipsis">'+esc(vcsWhoName(w.who))+'</b><span class="small muted ellipsis">'+(s?esc(s.title)+' · '+s.agent:"")+'</span></div>'+(s?pill(s.status):"")+'</div>'+
  '<div class="inset vcs-wtk"><div class="flex"><span class="muted small">브랜치</span><b class="mono ellipsis spacer">'+esc(w.branch)+'</b>'+vcsAB(w.ahead,w.behind)+'</div><div class="flex"><span class="muted small">worktree</span><span class="mono ellipsis spacer small">'+esc(w.path)+'</span></div><div class="flex"><span class="muted small">컴퓨터</span><span class="spacer small">'+esc(w.computer)+'</span><span class="small '+(w.dirty?"vcs-warn":"muted")+'">'+(w.dirty?"변경 "+w.dirty+"개":"깨끗함")+'</span></div></div>'+
  '<div class="small ellipsis">'+I("commit")+' '+esc(w.last)+'</div>'+
  '<div class="flex wrap">'+(pr?'<button class="chip" data-vcs="goto-pr" data-n="'+pr.n+'">'+I("pr")+'#'+pr.n+' '+(pr.state==="merged"?"병합됨":VCS_CI[pr.ci][0])+'</button>':'<span class="chip muted">PR 없음</span>')+(w.conflict?'<button class="chip vcs-warn" data-vcs="tab" data-t="conflict">'+I("warn")+'충돌 해결</button>':"")+'<div class="spacer"></div><button class="btn sm" data-vcs="switch" data-b="'+esc(w.branch)+'">브랜치 보기</button>'+(s?'<a class="btn sm" href="conversation.html#'+s.id+'">세션</a>':"")+'</div></article>'}).join("")+'</div>'}

/* ---------- screen ---------- */
function vcsContent(){return {changes:vcsChanges,history:vcsHistory,branches:vcsBranches,prs:vcsPRs,stash:vcsStash,conflict:vcsConflict,team:vcsTeam}[vcsTab]()}
function vcsQ(sel){return (vcsRoot||document).querySelector(sel)}
function vcsQA(sel){return (vcsRoot||document).querySelectorAll(sel)}
function vcsInner(){var r=vcsR();
 return vcsBar()+'<div class="vcs-body">'+vcsNav()+'<div class="vcs-content" id="vcsContent">'+vcsContent()+'</div></div>'}
function vcs(){
 if(vcsP==="unsorted"||!D.vcs[vcsP]){vcsP="ember"}
 var r=vcsR(), p=proj(vcsP);
 if(vcsRoot){var w=vcsRoot.querySelector(".vcs-mounted");
  var html=(vcsOpts.compact?'<div class="vcs-mhead sheet-h"><span class="vcs-mico">'+I("git")+'</span><div class="col spacer" style="min-width:0"><b class="ellipsis">버전 관리 · '+esc(p.name)+'</b><span class="small muted ellipsis">'+esc(r.branch)+' · 변경 '+r.changes.length+' · PR '+vcsOpenPRs(r)+'</span></div><div class="vcs-macts sheet-acts"><a class="btn sm" href="vcs.html#'+vcsP+'/'+vcsTab+'">전체 화면</a><button class="icon-btn sm" data-vcs="close" title="닫기" aria-label="닫기">'+I("close")+'</button></div></div>':"")+vcsInner();
  if(!w){vcsRoot.innerHTML='<div class="vcs vcs-mounted'+(vcsOpts.compact?" vcs-compact":"")+'" id="vcsMain"></div>'; w=vcsRoot.querySelector(".vcs-mounted")}
  w.innerHTML=html; vcsBind(); return}
 render(shell("vcs",'<main class="main vcs" id="vcsMain">'+head("버전 관리",esc(p.name)+' · '+esc(r.branch)+' · 변경 '+r.changes.length+' · PR '+vcsOpenPRs(r),"","",{primary:false})+vcsInner()+'</main>'));
 vcsBind(); vcsHash()}
function vcsPhoneTop(){if(matchMedia("(max-width:560px)").matches){var c=vcsQ("#vcsContent"),m=vcsQ("#vcsMain"); if(c&&m) m.scrollTop=Math.max(0,c.offsetTop-12)}}
function vcsDraw(){var keep={}; vcsQA("[data-keep]").forEach(function(el){keep[el.id]=[el.scrollTop,el.scrollLeft]}); var mt=vcsQ("#vcsMain"), ms=mt?mt.scrollTop:0;
 vcs(); Object.keys(keep).forEach(function(id){var el=vcsQ("#"+id); if(el){el.scrollTop=keep[id][0]; el.scrollLeft=keep[id][1]}}); var m2=vcsQ("#vcsMain"); if(m2) m2.scrollTop=ms}
function vcsBind(){var s=vcsQ("#vcsProj"); if(s) s.onchange=function(){vcsP=s.value; lsSet("ember-mock-vcs-proj",vcsP); vcsFile=0; vcsCommit=0; vcsPR=null; vcsCFile=0; vcsPop=false; vcsSub="a"; vcsDraw()};
 vcsQA("#vcsSum,#vcsBody").forEach(function(el){el.oninput=function(){if(el.id==="vcsSum") vcsSummary=el.value; else vcsBody=el.value; vcsQA("#"+el.id).forEach(function(x){if(x!==el) x.value=el.value})}});
 var am=vcsQ("#vcsAmend"); if(am) am.onchange=function(){vcsAmend=am.checked};
 var q=vcsQ("#vcsBrQ"); if(q){q.oninput=function(){vcsBrQ=q.value; vcsQ("#vcsPopList").innerHTML=vcsPopList()}; q.focus()}
 vcsQA(".vcs-ln[data-vcs]").forEach(function(el){el.onkeydown=function(e){if(e.key===" "||e.key==="Enter"){e.preventDefault(); el.click()}}})}

/* ---------- actions ---------- */
function vcsHeadCommit(r){return r.commits.filter(function(c){return c.refs&&c.refs.indexOf("HEAD")>=0})[0]||r.commits[0]}
function vcsSha(){return Math.random().toString(16).slice(2,9)}
function vcsDoCommit(){var r=vcsR(), n=vcsStaged(r); if(!n) return toast("스테이징한 줄이 없습니다"); if(!vcsSummary.trim()) {toast("요약을 입력하세요"); var s=vcsQ("#vcsSum"); if(s) s.focus(); return}
 var h=vcsHeadCommit(r), files=[];
 r.changes=r.changes.filter(function(c){var L=vcsLines(c); if(!L.on) return true; files.push([c.path,c.st,c.add,c.del]);
  if(L.on===L.n) return false;
  c.hunks.forEach(function(hk,hi){hk.lines=hk.lines.filter(function(l,li){var on=l[0]!==" "&&!(c.off&&c.off[hi+":"+li]); if(on&&l[0]==="-") return false; if(on&&l[0]==="+") l[0]=" "; return true})});
  var keep={}; c.hunks.forEach(function(hk,hi){hk.lines.forEach(function(l,li){if(l[0]!==" ") keep[hi+":"+li]=1})}); c.off={}; Object.keys(keep).forEach(function(k){c.off[k]=1}); return true});
 if(vcsAmend){h.msg=vcsSummary; h.files=(h.files||[]).concat(files); toast("마지막 커밋을 수정했습니다")}
 else{var refs=(h.refs||[]).filter(function(x){return x==="HEAD"||x===r.branch}); h.refs=(h.refs||[]).filter(function(x){return refs.indexOf(x)<0});
  r.commits.unshift({sha:vcsSha(),lane:h.lane,parents:[h.sha],msg:vcsSummary,body:vcsBody,who:"BREW",time:"방금",refs:["HEAD",r.branch],local:true,files:files}); r.ahead++; toast(r.branch+"에 커밋했습니다 · 파일 "+files.length+"개")}
 vcsSummary=""; vcsBody=""; vcsAmend=false; vcsFile=0; vcsSave(); vcsDraw()}
function vcsSwitch(name){var r=vcsR(), b=r.branches.filter(function(x){return x.name===name})[0]; if(!b) return;
 var n=r.changes.length; r.branch=name; r.upstream="origin/"+name; r.ahead=b.ahead; r.behind=b.behind;
 r.commits.forEach(function(c){if(c.refs) c.refs=c.refs.filter(function(x){return x!=="HEAD"})});
 var tip=r.commits.filter(function(c){return c.refs&&c.refs.indexOf(name)>=0})[0]; if(tip) tip.refs.unshift("HEAD");
 vcsPop=false; vcsSave(); toast(name+"로 전환했습니다"+(n?" · 변경 "+n+"개를 함께 가져갔습니다":"")); vcsDraw()}
function vcsCreate(id){var el=vcsQ("#"+id), name=(el&&el.value||"").trim().replace(/\s+/g,"-"), r=vcsR();
 if(!name) {toast("브랜치 이름을 입력하세요"); if(el) el.focus(); return}
 if(r.branches.some(function(b){return b.name===name})) return toast("이미 있는 이름입니다");
 var bs=vcsQ("#vcsBase"), base=bs?bs.value:r.branch;
 r.branches.splice(3,0,{name:name,base:base,ahead:0,behind:0,by:"",when:"방금"});
 var h=vcsHeadCommit(r); h.refs=(h.refs||[]).concat([name]); vcsSwitch(name)}
function vcsRemote(k){var r=vcsR();
 if(k==="fetch"){r.fetched="방금"; toast("origin에서 가져왔습니다 · 뒤처짐 "+r.behind)}
 else if(k==="pull"){if(!r.behind) toast("받을 커밋이 없습니다"); else {toast("커밋 "+r.behind+"개를 받아 병합했습니다"); r.behind=0; r.fetched="방금"}}
 else if(k==="push"){if(r.behind) return toast("원격이 앞서 있어 거부되었습니다. 먼저 Pull 하세요"); if(!r.ahead) return toast("보낼 커밋이 없습니다");
  toast("커밋 "+r.ahead+"개를 "+r.upstream+"에 보냈습니다"); r.ahead=0;
  r.commits.forEach(function(c){c.local=false; if(c.refs) c.refs=c.refs.filter(function(x){return x!==r.upstream})}); var h=vcsHeadCommit(r); h.refs.push(r.upstream)}
 var b=r.branches.filter(function(x){return x.name===r.branch})[0]; if(b){b.ahead=r.ahead; b.behind=r.behind}
 vcsSave(); vcsDraw()}
function vcsStashNow(){var r=vcsR(); if(!r.changes.length) return toast("스태시할 변경이 없습니다");
 r.stashes.unshift({id:"stash@{0}",msg:"WIP on "+r.branch+": "+(vcsSummary||r.changes[0].path.split("/").pop()),branch:r.branch,files:r.changes.length,when:"방금",data:r.changes});
 r.stashes.forEach(function(s,i){s.id="stash@{"+i+"}"}); r.changes=[]; vcsSave(); toast("변경 사항을 스태시했습니다"); vcsDraw()}
function vcsStashUse(i,pop){var r=vcsR(), s=r.stashes[i];
 if(s.data) r.changes=r.changes.concat(JSON.parse(JSON.stringify(s.data)));
 if(pop){r.stashes.splice(i,1); r.stashes.forEach(function(s,i){s.id="stash@{"+i+"}"})}
 vcsSave(); toast((pop?"꺼냈습니다: ":"적용했습니다: ")+s.msg); vcsDraw()}
function vcsReview(k){var r=vcsR(), p=r.prs.filter(function(x){return x.n===vcsPR})[0], t=(vcsQ("#vcsRv")||{}).value||"";
 if(k==="comment"&&!t.trim()) return toast("코멘트를 입력하세요");
 if(t.trim()) p.comments.push({who:"BREW",text:t.trim()});
 if(k!=="comment"){var me=p.reviewers.filter(function(v){return v[0]==="BREW"})[0]; if(me) me[1]=k; else p.reviewers.push(["BREW",k]);
  p.review=p.reviewers.some(function(v){return v[1]==="changes"})?"changes":p.reviewers.some(function(v){return v[1]==="approved"})?"approved":"pending"}
 vcsSave(); toast(k==="approved"?"승인했습니다":k==="changes"?"변경을 요청했습니다. 작성 에이전트에게 전달됩니다":"코멘트를 남겼습니다"); vcsDraw()}
function vcsMerge(){var r=vcsR(), p=r.prs.filter(function(x){return x.n===vcsPR})[0]; p.state="merged";
 var b=r.branches.filter(function(x){return x.name===p.branch})[0]; if(b) b.merged=true;
 var bc=r.commits.filter(function(c){return c.refs&&c.refs.indexOf(p.base)>=0})[0];
 if(bc){var tip=r.commits.filter(function(c){return c.refs&&c.refs.indexOf(p.branch)>=0})[0]; bc.refs=bc.refs.filter(function(x){return x!==p.base});
  r.commits.unshift({sha:vcsSha(),lane:bc.lane,parents:[bc.sha].concat(tip?[tip.sha]:[]),msg:"Merge pull request #"+p.n+" from "+p.branch,who:"BREW",time:"방금",refs:[p.base]})}
 vcsSave(); toast("#"+p.n+"을 "+p.base+"에 병합했습니다"); vcsDraw()}

function vcsClick(e,t){var a=t.closest("[data-vcs]");
 if(!a){if(vcsPop&&!t.closest(".vcs-pop")){vcsPop=false; vcsDraw(); return true} return false}
 var k=a.dataset.vcs, r=vcsR(), i=+a.dataset.i;
 if(k==="pop"){vcsPop=!vcsPop; vcsBrQ=""; vcsDraw()}
 else if(k==="tab"){vcsTab=a.dataset.t; vcsSub="a"; vcsPop=false; vcsDraw(); vcsPhoneTop()}
 else if(k==="sub"){vcsSub=a.dataset.s; vcsDraw(); vcsPhoneTop()}
 else if(k==="file"){vcsFile=i; vcsSub="b"; vcsDraw(); vcsPhoneTop()}
 else if(k==="fcheck"){var c=r.changes[i], L=vcsLines(c); c.off={}; if(L.on===L.n) c.hunks.forEach(function(h,hi){h.lines.forEach(function(l,li){if(l[0]!==" ") c.off[hi+":"+li]=1})}); vcsSave(); vcsDraw()}
 else if(k==="all"){var all=r.changes.every(function(c){var L=vcsLines(c);return L.on===L.n}); r.changes.forEach(function(c){c.off={}; if(all) c.hunks.forEach(function(h,hi){h.lines.forEach(function(l,li){if(l[0]!==" ") c.off[hi+":"+li]=1})})}); vcsSave(); vcsDraw()}
 else if(k==="line"){var c2=r.changes[vcsFile]; c2.off=c2.off||{}; if(c2.off[a.dataset.k]) delete c2.off[a.dataset.k]; else c2.off[a.dataset.k]=1; vcsSave(); vcsDraw()}
 else if(k==="hunk"){var c3=r.changes[vcsFile], hi=+a.dataset.h, keys=[]; c3.off=c3.off||{}; c3.hunks[hi].lines.forEach(function(l,li){if(l[0]!==" ") keys.push(hi+":"+li)}); var anyOff=keys.some(function(x){return c3.off[x]}); keys.forEach(function(x){if(anyOff) delete c3.off[x]; else c3.off[x]=1}); vcsSave(); vcsDraw()}
 else if(k==="fold"){vcsFold[a.dataset.p]=!vcsFold[a.dataset.p]; vcsDraw()}
 else if(k==="commit") vcsDoCommit();
 else if(k==="commitsel"){vcsCommit=i; vcsSub="b"; vcsDraw(); vcsPhoneTop()}
 else if(k==="switch") vcsSwitch(a.dataset.b);
 else if(k==="create") vcsCreate("vcsNewBr");
 else if(k==="create2") vcsCreate("vcsNewBr2");
 else if(k==="fetch"||k==="pull"||k==="push") vcsRemote(k);
 else if(k==="prsel"){vcsPR=+a.dataset.n; vcsSub="b"; vcsDraw(); vcsPhoneTop()}
 else if(k==="goto-pr"){vcsPR=+a.dataset.n; vcsTab="prs"; vcsSub="b"; vcsDraw()}
 else if(k==="rv") vcsReview(a.dataset.k);
 else if(k==="merge") vcsMerge();
 else if(k==="newpr") toast(r.branch+" → develop PR 작성 (데모)");
 else if(k==="stashnow") vcsStashNow();
 else if(k==="sapply") vcsStashUse(i,false);
 else if(k==="spop") vcsStashUse(i,true);
 else if(k==="sdrop"){var sd=r.stashes.splice(i,1)[0]; r.stashes.forEach(function(s,j){s.id="stash@{"+j+"}"}); vcsSave(); toast("버렸습니다: "+sd.msg); vcsDraw()}
 else if(k==="cfile"){vcsCFile=i; vcsDraw()}
 else if(k==="pick"){r.conflict.files[vcsCFile].blocks[+a.dataset.b].pick=a.dataset.k||null; vcsSave(); vcsDraw()}
 else if(k==="pickall"){r.conflict.files[vcsCFile].blocks.forEach(function(b){if(b.pick!=="none") b.pick=a.dataset.k}); vcsSave(); vcsDraw()}
 else if(k==="abort"||k==="resolve"){var cf=r.conflict; r.worktrees.forEach(function(w){if(w.branch===cf.into) w.conflict=false}); r.conflict=null; vcsSave(); toast(k==="abort"?"병합을 취소했습니다":cf.from+"을 "+cf.into+"에 병합했습니다. 담당 에이전트에게 알렸습니다"); vcsDraw()}
 else if(k==="toast") toast(a.dataset.m);
 else if(k==="close"){if(vcsOpts.onClose) vcsOpts.onClose(); else {var md=vcsRoot&&vcsRoot.closest(".modal"); vcsUnmount(vcsRoot); if(md) md.remove()}}
 return true}
/* ---------- embedding: Ember.vcs.mount(el, projectId, opts) / unmount(el). One mount at a time; see FILES.md. ---------- */
function vcsMount(el,pid,opts){if(vcsRoot&&vcsRoot!==el) vcsUnmount(vcsRoot);
 vcsPage={p:vcsP,tab:vcsTab}; vcsRoot=el; vcsOpts=opts||{}; vcsP=D.vcs[pid]?pid:"ember"; vcsTab=vcsOpts.tab||"changes"; vcsSub="a"; vcsPop=false; vcsFile=0; vcsCommit=0; vcsPR=null; vcsCFile=0;
 el._vcsClick=function(e){if(vcsClick(e,e.target)===true) e.stopPropagation()}; el.addEventListener("click",el._vcsClick); vcs()}
function vcsUnmount(el){el=el||vcsRoot; if(!el) return; if(el._vcsClick) el.removeEventListener("click",el._vcsClick); el._vcsClick=null; el.innerHTML="";
 if(el===vcsRoot){vcsRoot=null; vcsOpts={}; if(vcsPage){vcsP=vcsPage.p; vcsTab=vcsPage.tab; vcsPage=null}}}
Ember.vcs={mount:vcsMount,unmount:vcsUnmount};
Ember.screens.vcs={render:function(){vcsReadHash(); vcs()},click:vcsClick,onHash:function(){vcsReadHash(); vcsDraw()}};
