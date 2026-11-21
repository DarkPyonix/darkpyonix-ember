/* Ember.monitor: 컴퓨터 상태 모니터 시트 (Mole Status 구성, Ember 팔레트).
   Ember.monitor.open(computerId), Ember.monitor.close(). 데모 값은 컴퓨터마다 시드가 고정된 랜덤 워크로 1초마다 바뀐다. */
(function(){
var E=window.Ember=window.Ember||{};
var CLS="monmodal", HIST=40, st=null;
var RM=window.matchMedia?matchMedia("(prefers-reduced-motion: reduce)"):{matches:false};

function data(){var d=window.D||window.EMBER_DEMO||{}; return d}
function monSpec(id){var d=data(); return (d.monitor||(window.EMBER_DEMO&&window.EMBER_DEMO.monitor)||{})[id]||null}
function monComp(id){return (data().computers||[]).filter(function(c){return c.id===id})[0]}
function monEsc(s){return E.esc?E.esc(s):String(s).replace(/[&<>"]/g,function(c){return {"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]})}
function monIc(n){return E.I?E.I(n):""}

/* 시드 랜덤 (mulberry32) */
function monSeed(s){var h=2166136261; for(var i=0;i<s.length;i++){h^=s.charCodeAt(i); h=Math.imul(h,16777619)} return h>>>0}
function monRng(seed){var a=seed; return function(){a|=0; a=a+0x6D2B79F5|0; var t=Math.imul(a^a>>>15,1|a); t=t+Math.imul(t^t>>>7,61|t)^t; return ((t^t>>>14)>>>0)/4294967296}}
function clamp(v,lo,hi){return Math.max(lo,Math.min(hi,v))}
/* 평균으로 돌아가는 랜덤 워크: 튀지 않고 기준값 주위를 맴돈다 */
function walk(r,v,base,step,lo,hi){return clamp(v+(r()-0.5)*2*step+(base-v)*0.08,lo,hi)}

function monMembers(id){return (data().sessions||[]).filter(function(s){return s.computer===id&&(s.status==="run"||s.status==="wait")})}
var PROC_NAME={"Claude Code":"claude","Codex":"codex","Gemini CLI":"gemini"};

function monInit(id){
 var sp=monSpec(id)||{chip:"",cores:8,ram:16,disk:500,used:200,cpu:10,gpu:5,mem:40,temp:45,fan:0,procs:[]};
 var r=monRng(monSeed(id)), s={id:id, sp:sp, r:r, sort:{k:"cpu",d:-1}, fanMode:"auto", base:{}};
 s.cores=[]; for(var i=0;i<sp.cores;i++) s.cores.push(clamp(sp.cpu+(r()-0.5)*40,1,99));
 s.gpu=sp.gpu; s.mem=sp.mem; s.temp=sp.temp; s.gtemp=Math.max(0,sp.temp-8); s.fan=sp.fan;
 s.up=0.4+r()*0.6; s.down=1.5+r()*2; s.gpuH=[]; s.memH=[]; s.upH=[]; s.downH=[];
 var members=monMembers(id);
 s.procs=sp.procs.map(function(p){return {name:p[0],pid:p[1],cpu:p[2],pwr:p[3],mem:p[4],b:p[2]}});
 members.forEach(function(m,i){var base=m.status==="run"?18+r()*20:3+r()*4;
  s.procs.push({name:PROC_NAME[m.agent]||"agent",pid:7000+i*37+Math.floor(r()*30),cpu:base,b:base,pwr:+(base/6).toFixed(1),mem:300+Math.floor(r()*900),m:m})});
 for(var k=0;k<HIST;k++) monStep(s,true);
 return s}

function monStep(s,warm){
 var r=s.r, sp=s.sp, sum=0;
 s.cores=s.cores.map(function(v,i){var b=sp.cpu*(i<s.cores.length/2?1.4:0.6); v=walk(r,v,b,9,0,100); sum+=v; return v});
 s.cpu=sum/s.cores.length;
 s.gpu=walk(r,s.gpu,sp.gpu,4,0,100); s.mem=walk(r,s.mem,sp.mem,1.2,5,98);
 s.temp=walk(r,s.temp,sp.temp+s.cpu*0.15-(s.fanMode==="max"?6:0),1.2,30,98);
 s.gtemp=walk(r,s.gtemp,sp.temp-8+s.gpu*0.2,1,28,95);
 var target={auto:sp.fan,cool:sp.fan*1.4+300,max:sp.fan?Math.max(3800,sp.fan*2.6):0}[s.fanMode];
 s.fan=sp.fan||s.fanMode!=="auto"?s.fan+(target-s.fan)*0.35+(r()-0.5)*40:0; if(s.fan<0) s.fan=0;
 s.up=walk(r,s.up,0.6,0.25,0,8); s.down=walk(r,s.down,2.6,0.8,0,30);
 [["gpuH","gpu"],["memH","mem"],["upH","up"],["downH","down"]].forEach(function(p){s[p[0]].push(s[p[1]]); if(s[p[0]].length>HIST) s[p[0]].shift()});
 if(!warm) s.procs.forEach(function(p){p.cpu=walk(r,p.cpu,p.b,Math.max(0.3,p.b*0.15),0,100); p.pwr=p.pwr?+walk(r,p.pwr,p.cpu/6,0.4,0,99).toFixed(1):0; p.mem=Math.round(walk(r,p.mem,p.mem,p.mem*0.01,1,99999))})
}

/* ---------- 그리기 도구 ---------- */
function big(v,unit,sub){return '<div class="mon-big"><b>'+v+'</b>'+(unit?'<span class="u">'+unit+'</span>':"")+(sub?'<span class="s">'+sub+'</span>':"")+'</div>'}
function badge(t,tone){return '<span class="pill mon-badge'+(tone?" "+tone:"")+'">'+monEsc(t)+'</span>'}
function head(ic,t,right){return '<div class="mon-h"><span class="mon-l">'+monIc(ic)+t+'</span><span class="mon-hr">'+(right||"")+'</span></div>'}
function foot(t){return '<div class="mon-f">'+t+'</div>'}
function bar(p,tone){return '<div class="mon-bar"><i class="'+(tone||"")+'" style="width:'+clamp(p,0,100).toFixed(1)+'%"></i></div>'}
function line(arr,max,cls,area){
 var w=200,h=44,n=arr.length-1||1;
 var pts=arr.map(function(v,i){return (i*w/n).toFixed(1)+","+(h-3-(v/max)*(h-6)).toFixed(1)}).join(" ");
 var fill=area?'<polygon class="mon-area '+cls+'" points="0,'+h+" "+pts+" "+w+","+h+'"/>':"";
 return fill+'<polyline class="mon-line '+cls+'" points="'+pts+'"/>'}
/* 네트워크: 가운데 선 위로 보내기, 아래로 받기 */
function netLines(s){var w=200,n=HIST-1,mu=Math.max(2,Math.max.apply(null,s.upH)),md=Math.max(6,Math.max.apply(null,s.downH));
 function pts(a,mx,dir){return a.map(function(v,i){return (i*w/n).toFixed(1)+","+(22+dir*(v/mx)*19).toFixed(1)}).join(" ")}
 var u=pts(s.upH,mu,-1), d=pts(s.downH,md,1);
 return '<line x1="0" y1="22" x2="200" y2="22" class="mon-mid"/><polygon class="mon-area green" points="0,22 '+u+' 200,22"/><polyline class="mon-line green" points="'+u+'"/><polygon class="mon-area blue" points="0,22 '+d+' 200,22"/><polyline class="mon-line blue" points="'+d+'"/>'}
function graph(inner){return '<svg class="mon-g" viewBox="0 0 200 44" preserveAspectRatio="none" aria-hidden="true">'+inner+'</svg>'}
function tempTone(t){return t>=85?"red":t>=70?"orange":"green"}
function f1(v){return (Math.round(v*10)/10).toFixed(1)}
function gb(mb){return mb>=1000?f1(mb/1000)+"\u00a0GB":Math.round(mb)+"\u00a0MB"}

function score(s){var sp=s.sp, free=1-sp.used/sp.disk; return Math.round(clamp(100-s.cpu*0.25-Math.max(0,s.mem-60)*0.6-Math.max(0,s.temp-70)*0.8-(free<0.15?12:0),0,100))}
function scoreWord(n){return n>=85?"아주 좋습니다":n>=70?"좋습니다":n>=50?"바쁩니다":"점검이 필요합니다"}

function cards(s){
 var sp=s.sp, c=monComp(s.id)||{}, sc=score(s), free=sp.disk-sp.used, freeP=free/sp.disk;
 var load=(s.cpu/100*sp.cores).toFixed(1), gpuName=sp.gpuName||(sp.gpuCores?"GPU 코어 "+sp.gpuCores+"개":"내장 GPU");
 var memUsed=sp.ram*s.mem/100, swap=s.mem>80?f1((s.mem-80)*0.2)+"\u00a0GB":"0\u00a0GB";
 var press=s.mem>85?["압력 높음","red"]:s.mem>70?["압력 보통","orange"]:["압력 "+Math.round(s.mem/4)+"%","green"];
 var cpuState=s.cpu<15?"여유롭습니다":s.cpu<60?"작업 중입니다":"바쁩니다";
 var fanTxt={auto:"자동으로 조절됩니다",cool:"조용하게 식히도록 돕니다",max:"최대 속도로 돕니다"}[s.fanMode];
 var ring=2*Math.PI*30;
 return [
 ["score",head("spark","상태",badge(sp.chip)+badge(sp.ram+"\u00a0GB")+badge(c.os||""))+
  '<div class="mon-score"><div>'+big(sc,"",scoreWord(sc))+'<div class="mon-meta">'+(freeP<0.15?"저장 공간이 부족합니다":"저장 공간이 넉넉합니다")+' · 여유 '+free+'\u00a0GB</div></div>'+
  '<svg class="mon-ring" viewBox="0 0 72 72" aria-hidden="true"><circle cx="36" cy="36" r="30" class="trk"/><circle cx="36" cy="36" r="30" class="val '+(sc>=70?"green":sc>=50?"orange":"red")+'" stroke-dasharray="'+(ring*sc/100).toFixed(1)+' '+ring.toFixed(1)+'"/></svg></div>'+
  foot("가동 시간 "+sp.uptime+" · "+sp.since+"부터")],
 ["cpu",head("cpu","CPU",badge(Math.round(s.temp)+"°C",tempTone(s.temp)))+big(Math.round(s.cpu),"%")+
  '<div class="mon-cores">'+s.cores.map(function(v){return '<i style="height:'+Math.max(4,v).toFixed(0)+'%"></i>'}).join("")+'</div>'+
  foot("부하 "+load+" / 코어 "+sp.cores+"개 · "+cpuState)],
 ["gpu",head("chart","GPU",badge(Math.round(s.gtemp)+"°C",tempTone(s.gtemp)))+big(Math.round(s.gpu),"%")+graph(line(s.gpuH,100,"orange"))+
  foot((s.gpu<10?"쉬고 있습니다":"사용 중입니다")+" · "+gpuName)],
 ["mem",head("server","메모리",badge(press[0],press[1]))+big(Math.round(s.mem),"%")+graph(line(s.memH,100,"blue",true))+
  foot(f1(memUsed)+"\u00a0GB 사용 · 스왑 "+swap)],
 ["temp",head("zap","온도",badge(s.temp>=85?"뜨겁습니다":s.temp>=70?"따뜻합니다":"정상입니다",tempTone(s.temp)))+
  '<div class="mon-temps"><div><span class="k">CPU</span><b>'+Math.round(s.temp)+'</b><span class="u">°C</span></div><div><span class="k">GPU</span><b>'+Math.round(s.gtemp)+'</b><span class="u">°C</span></div></div>'+
  '<div class="mon-tbars">'+bar(s.temp,tempTone(s.temp))+bar(s.gtemp,tempTone(s.gtemp))+'</div>'+foot(sp.chip)],
 ["disk",head("folder","디스크",badge(sp.disk>=1000?(sp.disk/1000)+"\u00a0TB":sp.disk+"\u00a0GB","blue"))+big(free,"GB","여유")+bar(100-freeP*100,"blue")+
  foot((sp.used>=1000?f1(sp.used/1000)+"\u00a0TB":sp.used+"\u00a0GB")+" 사용 · "+Math.round(100-freeP*100)+"%")],
 ["net",head("globe","네트워크",badge(sp.iface,"blue"))+big(f1(s.down),"MB/s","받기")+
  graph(netLines(s))+
  foot("보내기 "+f1(s.up)+"\u00a0MB/s · "+sp.iface)],
 ["fan",head("refresh","팬",badge("부하 "+Math.round(s.cpu)+"%",s.cpu>60?"orange":""))+big(Math.round(s.fan).toLocaleString(),"RPM")+
  '<div class="seg mon-seg" role="radiogroup" aria-label="팬 모드">'+[["auto","자동"],["cool","냉각"],["max","최대"]].map(function(m){return '<button role="radio" aria-checked="'+(s.fanMode===m[0])+'" class="'+(s.fanMode===m[0]?"on":"")+'" data-mon-fan="'+m[0]+'">'+m[1]+'</button>'}).join("")+'</div>'+
  foot(sp.fan||s.fanMode!=="auto"?fanTxt:"팬 없이 조용하게 돕니다")]
 ]}

var COLS=[["name","이름"],["pid","PID"],["cpu","CPU"],["pwr","전력"],["mem","메모리"]];
function tableHead(s){return '<tr>'+COLS.map(function(c){var on=s.sort.k===c[0];
 return '<th class="c-'+c[0]+'" data-mon-sort="'+c[0]+'" aria-sort="'+(on?(s.sort.d<0?"descending":"ascending"):"none")+'"><button data-mon-sort="'+c[0]+'">'+c[1]+(c[0]==="name"?" ("+s.procs.length+")":"")+'<span class="ar'+(on?" on":"")+'">'+(on?(s.sort.d<0?"↓":"↑"):"↕")+'</span></button></th>'}).join("")+'</tr>'}
function tableBody(s){
 var maxc=Math.max.apply(null,s.procs.map(function(p){return p.cpu}).concat([1]));
 return sortProcs(s).map(function(p){
  var m=p.m, who=m?'<span class="mon-who">'+(E.bot?E.bot(m,22):"")+'<span class="mon-mn">'+monEsc(E.mname?E.mname(m):m.role)+'</span></span>':'<span class="mon-pi">'+monIc("cmd")+'</span>';
  return '<tr data-pid="'+p.pid+'"'+(m?' class="mine" data-mon-sess="'+m.id+'" tabindex="0" aria-label="'+monEsc((E.mname?E.mname(m):m.role)+" 세션으로 이동합니다")+'"':"")+'><td class="c-name"><span class="mon-nm">'+who+'<b>'+monEsc(p.name)+'</b></span></td>'+
   '<td class="c-pid num">'+p.pid+'</td><td class="c-cpu"><span class="mon-cpu"><span class="mon-bar sm"><i class="'+(m?"blue":"")+'" style="width:'+(p.cpu/maxc*100).toFixed(0)+'%"></i></span><span class="num">'+f1(p.cpu)+'</span></span></td>'+
   '<td class="c-pwr num">'+(p.pwr?f1(p.pwr):"-")+'</td><td class="c-mem num">'+gb(p.mem)+'</td></tr>'}).join("")}

/* ---------- 시트 ---------- */
function shell(s){
 var c=monComp(s.id)||{id:s.id,os:"",status:"unknown"}, on=c.status==="online";
 var stTxt={online:"온라인",offline:"오프라인",unknown:"알 수 없음"}[c.status]||"";
 return '<div class="sheet-h"><span class="sheet-t ellipsis">'+monEsc(c.id)+'</span><span class="mon-os muted">'+monEsc(c.os||"")+'</span>'+
  '<span class="sheet-acts"><span class="mon-state '+(on?"on":"")+'"><span class="dot '+(on?"ok":"off")+'"></span>'+stTxt+'</span>'+
  '<button class="icon-btn sm" data-mon-close aria-label="닫기">'+monIc("close")+'</button></span></div>'+
  '<div class="mon-scroll">'+(on?"":'<div class="mon-off" role="status">'+monIc("warn")+'<div><b>연결이 끊겼습니다</b><span>마지막 연결: '+monEsc(s.sp.seenAt||c.seen||"기록이 없습니다")+' · 아래는 마지막으로 받은 값입니다.</span></div></div>')+
  '<div class="mon-wrap'+(on?"":" stale")+'"><div class="mon-grid">'+cards(s).map(function(k){return '<section class="card mon-card" data-mon-card="'+k[0]+'"><div class="mon-in">'+k[1]+'</div></section>'}).join("")+'</div>'+
  '<section class="card mon-tablebox"><div class="mon-tscroll"><table class="mon-table"><thead>'+tableHead(s)+'</thead><tbody>'+tableBody(s)+'</tbody></table></div></section>'+
  '<p class="mon-note muted">팀원 행을 누르면 그 팀원의 세션으로 이동합니다.</p></div></div>'}

/* 표는 DOM을 다시 만들지 않고 제자리에서 고친다. 누르는 사이에 행이 바뀌면 click이 사라지기 때문이다. */
function sortProcs(s){var k=s.sort.k, d=s.sort.d; return s.procs.slice().sort(function(a,b){var x=a[k],y=b[k]; if(k==="name"){x=x.toLowerCase(); y=y.toLowerCase()} return x<y?-d:x>y?d:(a.pid-b.pid)})}
function updateHead(){
 st.root.querySelectorAll(".mon-table th").forEach(function(th,i){var k=COLS[i][0], on=st.sort.k===k, ar=th.querySelector(".ar");
  th.setAttribute("aria-sort",on?(st.sort.d<0?"descending":"ascending"):"none");
  ar.textContent=on?(st.sort.d<0?"↓":"↑"):"↕"; ar.classList.toggle("on",on)})}
function updateBody(){
 var tb=st.root.querySelector(".mon-table tbody"), rows={}, maxc=Math.max.apply(null,st.procs.map(function(p){return p.cpu}).concat([1]));
 [].forEach.call(tb.rows,function(r){rows[r.dataset.pid]=r});
 sortProcs(st).forEach(function(p){var r=rows[p.pid]; if(!r) return;
  r.querySelector(".mon-bar i").style.width=(p.cpu/maxc*100).toFixed(0)+"%";
  r.querySelector(".c-cpu .num").textContent=f1(p.cpu);
  r.querySelector(".c-pwr").textContent=p.pwr?f1(p.pwr):"-";
  r.querySelector(".c-mem").textContent=gb(p.mem);
  tb.appendChild(r)})}
function update(){
 if(!st||!st.root) return;
 cards(st).forEach(function(k){var el=st.root.querySelector('[data-mon-card="'+k[0]+'"] .mon-in'); if(el) el.innerHTML=k[1]});
 updateHead(); updateBody()}

function live(){return st&&st.root&&st.root.isConnected&&(monComp(st.id)||{}).status==="online"}
function tick(){
 if(!st) return;
 if(!st.root.isConnected){stopTimer(); st=null; return}
 if(document.hidden||!live()||st.down) return;
 monStep(st,false); update()}
function startTimer(){stopTimer(); if(live()) st.timer=setInterval(tick,1000)}
function stopTimer(){if(st&&st.timer){clearInterval(st.timer); st.timer=null}}

/* 같은 열을 다시 누르면 방향이 바뀐다(↓와 ↑). 다른 열은 이름만 오름차순, 나머지는 내림차순으로 시작한다. */
function doSort(k){if(!st) return; st.sort=st.sort.k===k?{k:k,d:-st.sort.d}:{k:k,d:k==="name"?1:-1}; updateHead(); updateBody(); return st.sort}
function go(id){close(); window.location.assign("conversation.html#"+encodeURIComponent(id))}
/* 캡처 단계에서 먼저 받아 core 디스패처와 다른 위임 처리기보다 앞선다 */
function onClick(e){
 var t=e.target, a;
 if(!st) return;
 if(t===st.modal){e.stopPropagation(); close(); return}
 if(!t.closest) return;
 if((a=t.closest("[data-mon-sess]"))){e.preventDefault(); e.stopPropagation(); go(a.dataset.monSess); return}
 if(t.closest("[data-mon-close]")){e.stopPropagation(); close(); return}
 if((a=t.closest("[data-mon-sort]"))){e.stopPropagation(); doSort(a.dataset.monSort); return}
 if((a=t.closest("[data-mon-fan]"))){e.stopPropagation(); st.fanMode=a.dataset.monFan; update(); return}
 e.stopPropagation()}
function onKey(e){var a=e.target.closest&&e.target.closest("[data-mon-sess]"); if(a&&(e.key==="Enter"||e.key===" ")){e.preventDefault(); e.stopPropagation(); go(a.dataset.monSess)}}

function open(id){
 close();
 st=monInit(id);
 var m=document.createElement("div"); m.className="modal "+CLS;
 m.innerHTML='<div class="sheet card mon-sheet" role="dialog" aria-modal="true" aria-label="'+monEsc(id)+' 상태"></div>';
 document.body.appendChild(m);
 st.modal=m; st.root=m.firstChild; st.root.innerHTML=shell(st);
 m.addEventListener("click",onClick,true); m.addEventListener("keydown",onKey,true);
 m.addEventListener("pointerdown",function(){if(st) st.down=true},true);
 if(E.sqRefresh) try{E.sqRefresh(st.root)}catch(_){}
 var cb=st.root.querySelector("[data-mon-close]"); if(cb) cb.focus();
 startTimer()}
function close(){if(!st) return; stopTimer(); if(st.modal&&st.modal.isConnected) st.modal.remove(); st=null}

document.addEventListener("pointerup",function(){if(st) st.down=false},true);
document.addEventListener("pointercancel",function(){if(st) st.down=false},true);
document.addEventListener("visibilitychange",function(){if(st&&!document.hidden) tick()});
/* 동작 줄이기 설정이 바뀌면 CSS가 전환을 끄고, 값 갱신은 그대로 이어진다 */
if(RM.addEventListener) RM.addEventListener("change",function(){if(st) update()});

E.monitor={open:open,close:close,__step:function(){if(st){monStep(st,false); update()}},__state:function(){return st},__sort:doSort};
})();
