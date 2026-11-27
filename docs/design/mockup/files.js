"use strict";
/* ---------- files: AirDrop-like transfer between the user's devices (ref1 numbers and tiles, ref3 bars) ---------- */
if(!D.files) D.files=JSON.parse(JSON.stringify(window.EMBER_DEMO.files));
var F=D.files, flDir="all", flPeer="all", flTick=null, flOfferShown=false, flDrag=null, flPickTo=null;

/* device and file-type glyphs (Heroicons outline 24 paths, same 1.5 stroke as icons.js) */
var FL_IC={
 phone:"M10.5 1.5H8.25A2.25 2.25 0 0 0 6 3.75v16.5a2.25 2.25 0 0 0 2.25 2.25h7.5A2.25 2.25 0 0 0 18 20.25V3.75a2.25 2.25 0 0 0-2.25-2.25H13.5m-3 0V3h3V1.5m-3 0h3m-3 18.75h3",
 tablet:"M10.5 19.5h3m-6.75 2.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-15a2.25 2.25 0 0 0-2.25-2.25H6.75A2.25 2.25 0 0 0 4.5 4.5v15a2.25 2.25 0 0 0 2.25 2.25Z",
 desktop:"M9 17.25v1.007a3 3 0 0 1-.879 2.122L7.5 21h9l-.621-.621A3 3 0 0 1 15 18.257V17.25m6-12V15a2.25 2.25 0 0 1-2.25 2.25H5.25A2.25 2.25 0 0 1 3 15V5.25m18 0A2.25 2.25 0 0 0 18.75 3H5.25A2.25 2.25 0 0 0 3 5.25m18 0V12a2.25 2.25 0 0 1-2.25 2.25H5.25A2.25 2.25 0 0 1 3 12V5.25",
 laptop:"M4.5 15V6.75A1.5 1.5 0 0 1 6 5.25h12a1.5 1.5 0 0 1 1.5 1.5V15M2.25 15h19.5l-.6 1.8a1.5 1.5 0 0 1-1.42 1.02H4.27a1.5 1.5 0 0 1-1.42-1.02L2.25 15Z",
 server:"M21.75 17.25v-.228a4.5 4.5 0 0 0-.12-1.03l-2.268-9.64a3.375 3.375 0 0 0-3.285-2.602H7.923a3.375 3.375 0 0 0-3.285 2.602l-2.268 9.64a4.5 4.5 0 0 0-.12 1.03v.228m19.5 0a3 3 0 0 1-3 3H5.25a3 3 0 0 1-3-3m19.5 0a3 3 0 0 0-3-3H5.25a3 3 0 0 0-3 3m16.5 0h.008v.008h-.008v-.008Zm-3 0h.008v.008h-.008v-.008Z",
 lock:"M16.5 10.5V6.75a4.5 4.5 0 1 0-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H6.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25Z",
 img:"m2.25 15.75 5.159-5.159a2.25 2.25 0 0 1 3.182 0l5.159 5.159m-1.5-1.5 1.409-1.409a2.25 2.25 0 0 1 3.182 0l2.909 2.909M3.75 21h16.5A2.25 2.25 0 0 0 22.5 18.75V5.25A2.25 2.25 0 0 0 20.25 3H3.75A2.25 2.25 0 0 0 1.5 5.25v13.5A2.25 2.25 0 0 0 3.75 21Zm10.5-11.25h.008v.008h-.008V9.75Z",
 video:"m15.75 10.5 4.72-4.72a.75.75 0 0 1 1.28.53v11.38a.75.75 0 0 1-1.28.53l-4.72-4.72M4.5 18.75h9a2.25 2.25 0 0 0 2.25-2.25v-9A2.25 2.25 0 0 0 13.5 5.25h-9A2.25 2.25 0 0 0 2.25 7.5v9a2.25 2.25 0 0 0 2.25 2.25Z",
 audio:"m9 9 10.5-3m0 6.553v3.75a2.25 2.25 0 0 1-1.632 2.163l-1.32.377a1.803 1.803 0 1 1-.99-3.467l2.31-.66a2.25 2.25 0 0 0 1.632-2.163Zm0 0V2.25L9 5.25v10.303m0 0v3.75a2.25 2.25 0 0 1-1.632 2.163l-1.32.377a1.803 1.803 0 0 1-.99-3.467l2.31-.66A2.25 2.25 0 0 0 9 15.553Z",
 zip:"m20.25 7.5-.625 10.632a2.25 2.25 0 0 1-2.247 2.118H6.622a2.25 2.25 0 0 1-2.247-2.118L3.75 7.5m8.25 3v6.75m0 0-3-3m3 3 3-3M3.375 7.5h17.25c.621 0 1.125-.504 1.125-1.125v-1.5c0-.621-.504-1.125-1.125-1.125H3.375c-.621 0-1.125.504-1.125 1.125v1.5c0 .621.504 1.125 1.125 1.125Z",
 doc:"M19.5 14.25v-2.625a3.375 3.375 0 0 0-3.375-3.375h-1.5A1.125 1.125 0 0 1 13.5 7.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H8.25m0 12.75h7.5m-7.5 3H12M10.5 2.25H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 0 0-9-9Z",
 reveal:"M3.75 9.776c.112-.017.227-.026.344-.026h15.812c.117 0 .232.009.344.026m-16.5 0a2.25 2.25 0 0 0-1.883 2.542l.857 6a2.25 2.25 0 0 0 2.227 1.932H19.05a2.25 2.25 0 0 0 2.227-1.932l.857-6a2.25 2.25 0 0 0-1.883-2.542m-16.5 0V6A2.25 2.25 0 0 1 6 3.75h3.879a1.5 1.5 0 0 1 1.06.44l2.122 2.12a1.5 1.5 0 0 0 1.06.44H18A2.25 2.25 0 0 1 20.25 9v.776",
 share:"M9 8.25H7.5a2.25 2.25 0 0 0-2.25 2.25v9a2.25 2.25 0 0 0 2.25 2.25h9a2.25 2.25 0 0 0 2.25-2.25v-9a2.25 2.25 0 0 0-2.25-2.25H15m0-3-3-3m0 0-3 3m3-3V15"
};
function flI(n){return FL_IC[n]?'<svg class="i" viewBox="0 0 24 24"><path d="'+FL_IC[n]+'"/></svg>':I(n)}
var FL_TYPE={zip:"압축",img:"이미지",video:"동영상",audio:"오디오",doc:"문서"};
var FL_PATH={near:["근처","green","같은 네트워크에서 직접 연결됩니다"],p2p:["직접 연결","blue","인터넷을 거쳐 P2P로 직접 연결됩니다"],relay:["릴레이","","Ember 릴레이를 거쳐 연결됩니다. 내용은 릴레이도 읽을 수 없습니다"],self:["이 장비","",""]};

function flSize(mb){return mb>=1024?(mb/1024).toFixed(mb>=10240?0:1)+" GB":mb>=1?(mb>=100?Math.round(mb):mb.toFixed(1))+" MB":Math.max(1,Math.round(mb*1024))+" KB"}
function flTime(s){s=Math.max(0,Math.round(s)); if(s<60) return s+"초"; var m=Math.floor(s/60); return m<60?m+"분 "+(s%60?(s%60)+"초":""):Math.floor(m/60)+"시간 "+(m%60)+"분"}
function flDev(id){return F.devices.filter(function(d){return d.id===id})[0]}
function flPeerName(id){var d=flDev(id); if(d) return d.name; var s=sess(id); return s?mname(s):id}
function flOnline(d){return d.state==="online"}
function flRoute(path){var p=FL_PATH[path]||FL_PATH.relay; return '<span class="fl-route" title="'+esc(p[2])+'">'+flI("lock")+'<span>종단 간 암호화 · '+(path==="near"?"근처 직접 연결":path==="p2p"?"P2P 직접 연결":"릴레이 경유")+'</span></span>'}

/* ---------- KPIs ---------- */
function flKpis(){var on=F.devices.filter(function(d){return d.id!==F.me&&flOnline(d)}).length, all=F.devices.length-1, act=F.transfers.filter(function(t){return t.state==="run"}).length;
 return '<div class="kpis fl-kpis">'+
 '<div class="kpi"><div class="l">연결된 장비</div><div class="v">'+on+'<span class="pill green">전체 '+all+'</span></div></div>'+
 '<div class="kpi"><div class="l">지금 전송 중</div><div class="v">'+act+'<span class="pill'+(act?" blue":"")+'">'+(F.transfers.length-act?"멈춤 "+(F.transfers.length-act):"모두 진행")+'</span></div></div>'+
 '<div class="kpi"><div class="l">이번 주 보낸 용량</div><div class="v">'+F.week.sent+'<span class="fl-unit">GB</span><span class="pill">받음 '+F.week.recv+' GB</span></div></div>'+
 '<div class="kpi"><div class="l">직접 연결 비율</div><div class="v">'+F.week.direct+'<span class="fl-unit">%</span></div><div class="fl-meter" aria-hidden="true"><i style="width:'+F.week.direct+'%"></i></div></div></div>'}

/* ---------- device tiles ---------- */
function flDevTile(d){var self=d.id===F.me, on=flOnline(d), p=FL_PATH[d.path]||FL_PATH.relay;
 var foot=self?'<span class="pill">보내는 장비</span>':on?'<span class="pill '+p[1]+'" title="'+esc(p[2])+'">'+p[0]+'</span><span class="small muted">'+d.rtt+' ms</span>':
  d.state==="sleep"?'<span class="pill orange">잠자기</span><span class="small muted">'+esc(d.seen)+'</span>':d.state==="offline"?'<span class="pill">오프라인</span><span class="small muted">'+esc(d.seen)+'</span>':'<span class="pill">확인 중</span>';
 return '<div class="card fl-dev sq-off st-'+d.state+(self?" self":"")+'"'+(self?"":' data-fdev="'+d.id+'" role="button" tabindex="0"')+' aria-label="'+esc(d.name)+(self?" (이 장비)":" 장비로 보내기")+'">'+
  '<div class="fl-dev-ic">'+flI(d.kind)+'<span class="fl-sd"></span></div>'+
  '<b class="ellipsis">'+esc(d.name)+'</b><span class="small muted ellipsis">'+esc(d.os)+(d.battery?" · 배터리 "+d.battery+"%":"")+'</span>'+
  '<div class="fl-dev-foot">'+foot+'</div><div class="fl-drop-hint">'+flI("up")+'<span>놓으면 보냅니다</span></div></div>'}
function flAgentTile(s){return '<div class="card fl-dev fl-agent sq-off" data-fagent="'+s.id+'" role="button" tabindex="0" aria-label="'+esc(mname(s))+' 작업 폴더로 보내기">'+
  '<div class="fl-dev-ic bot">'+bot(s,34)+'</div><b class="ellipsis">'+esc(mname(s))+'</b><span class="small muted ellipsis">'+esc(s.computer)+'</span><span class="small muted ellipsis" title="'+esc(s.title)+'">'+esc(s.title)+'</span>'+
  '<div class="fl-dev-foot">'+pill(s.status)+'</div><div class="fl-drop-hint">'+flI("up")+'<span>대화에 첨부합니다</span></div></div>'}
function flAgents(){return D.sessions.filter(function(s){return s.status==="run"||s.status==="wait"}).sort(function(a,b){return (b.lead?1:0)-(a.lead?1:0)||b.t-a.t}).slice(0,4)}
function flStage(){var me=flDev(F.me);
 return '<section class="card fl-stage" aria-label="내 장비">'+
 '<div class="fl-stage-h"><div class="fl-me">'+flI(me.kind)+'<div class="col"><span class="small muted">보내는 장비</span><b>'+esc(me.name)+'</b></div></div><div class="spacer"></div>'+
  '<span class="fl-scan"><i></i>근처 장비를 찾는 중입니다</span></div>'+
 '<div class="fl-devgrid">'+F.devices.filter(function(d){return d.id!==F.me}).map(flDevTile).join("")+'</div>'+
 '<div class="fl-sub-h"><b>에이전트에게 보내기</b><span class="small muted">팀원이 일하는 컴퓨터의 작업 폴더로 보내고 그 대화에 첨부합니다</span></div>'+
 '<div class="fl-devgrid agents">'+flAgents().map(flAgentTile).join("")+'</div>'+
 '<div class="fl-tray" aria-label="보낼 파일"><div class="fl-tray-h"><b>보낼 파일</b><span class="small muted fl-hint-drag">파일을 장비 위로 끌어 놓으면 보냅니다. 컴퓨터의 파일을 바로 끌어와도 됩니다.</span><span class="small muted fl-hint-tap">장비를 누르면 보낼 파일을 고릅니다.</span></div>'+
 '<div class="fl-chips">'+F.tray.map(function(f,i){return '<div class="fl-file" draggable="true" data-ffile="'+i+'" title="'+esc(f.name)+'"><span class="fl-ft t-'+f.type+'">'+flI(f.type)+'</span><span class="col" style="min-width:0"><span class="fl-fname">'+esc(f.name)+'</span><span class="small muted fl-fmeta">'+FL_TYPE[f.type]+' · '+flSize(f.size)+'</span></span></div>'}).join("")+
 '<button class="fl-file add" data-act="fpick">'+I("plus")+'<span>파일 고르기</span></button></div></div></section>'}

/* ---------- transfers ---------- */
function flTransfer(t){var pct=Math.round(t.done*100), left=(1-t.done)*t.size, dirTxt=t.dir==="out"?"보내는 중":"받는 중";
 var meta=t.state==="run"?'<span id="fls-'+t.id+'">'+t.speed+' MB/s · '+flTime(left/t.speed)+' 남음</span>':t.state==="wait"?'<span>상대 장비의 수락을 기다립니다</span>':t.state==="paused"?'<span>일시 정지 · '+flSize(left)+' 남음</span>':'<span class="fl-warn">'+I("warn")+'연결 끊김 · '+pct+'%에서 이어집니다</span>';
 return '<div class="fl-tr st-'+t.state+'" id="flt-'+t.id+'">'+
 '<div class="fl-tr-top"><span class="fl-ft t-'+t.type+'">'+flI(t.type)+'</span><div class="col spacer"><b class="ellipsis">'+esc(t.name)+'</b><span class="small muted ellipsis">'+(t.dir==="out"?I("up"):I("down"))+' '+dirTxt+' · '+esc(flPeerName(t.peer))+'</span></div>'+
 '<div class="fl-pct"><b id="flp-'+t.id+'">'+pct+'</b><span>%</span></div></div>'+
 '<div class="fl-bar"><i id="flb-'+t.id+'" style="width:'+pct+'%"></i></div>'+
 '<div class="fl-tr-foot small muted"><span id="fld-'+t.id+'">'+flSize(t.done*t.size)+' / '+flSize(t.size)+'</span>'+meta+'<span class="spacer"></span>'+
 (t.state==="wait"?"":t.state==="run"?'<button class="icon-btn sm" data-ftp="'+t.id+'" title="일시 정지" aria-label="일시 정지">'+I("pause")+'</button>':'<button class="icon-btn sm" data-ftr="'+t.id+'" title="이어서 전송" aria-label="이어서 전송">'+I("play")+'</button>')+
 '<button class="icon-btn sm" data-ftx="'+t.id+'" title="취소" aria-label="전송 취소">'+I("close")+'</button></div>'+
 (t.note&&t.state==="broken"?'<div class="fl-note small">'+esc(t.note)+'</div>':"")+
 '<div class="fl-tr-route">'+flRoute(t.path)+'</div></div>'}
function flTransfers(){return '<div class="card-h"><div class="ico">'+I("refresh")+'</div>전송 중<div class="spacer"></div><span class="pill">'+F.transfers.length+'</span></div>'+
 (F.transfers.length?'<div class="fl-trs">'+F.transfers.map(flTransfer).join("")+'</div>':'<div class="empty">진행 중인 전송이 없습니다</div>')}

/* ---------- received ---------- */
function flReceived(){return '<div class="card-h"><div class="ico">'+I("down")+'</div>받은 파일<div class="spacer"></div><span class="small muted mono-path" title="받은 파일이 저장되는 곳">'+esc(F.inbox)+'</span></div>'+
 (F.received.length?'<div class="fl-recv">'+F.received.slice(0,6).map(function(r){return '<div class="fl-rc">'+
  '<div class="fl-rc-prev t-'+r.type+'">'+flI(r.type)+'<span class="fl-type">'+FL_TYPE[r.type]+'</span></div>'+
  '<b class="ellipsis" title="'+esc(r.name)+'">'+esc(r.name)+'</b><span class="small muted ellipsis">'+flSize(r.size)+' · '+esc(flPeerName(r.from))+'</span><span class="small muted ellipsis">'+esc(r.time)+'</span>'+
  '<span class="small fl-saved ellipsis" title="'+esc(r.saved)+'">'+esc(r.saved)+'</span>'+
  '<div class="fl-rc-acts"><button class="btn sm" data-fopen="'+r.id+'">열기</button><button class="icon-btn sm" data-freveal="'+r.id+'" title="Finder에서 보기" aria-label="Finder에서 보기">'+flI("reveal")+'</button></div></div>'}).join("")+'</div>':'<div class="empty">받은 파일이 없습니다</div>')}

/* ---------- history ---------- */
function flHistRows(){var l=F.history.filter(function(h){return (flDir==="all"||(flDir==="fail"?h.state!=="done":h.dir===flDir&&h.state==="done"))&&(flPeer==="all"||h.peer===flPeer)});
 if(!l.length) return '<div class="empty">조건에 맞는 전송 기록이 없습니다</div>';
 var days=[]; l.forEach(function(h){if(days.indexOf(h.day)<0) days.push(h.day)});
 return days.map(function(day){return '<div class="fl-day"><span class="fl-day-l">'+esc(day)+'</span><div class="fl-hgrid">'+l.filter(function(h){return h.day===day}).map(function(h){
  var st=h.state==="done"?(h.dir==="out"?'<span class="pill green">보냄</span>':'<span class="pill blue">받음</span>'):h.state==="declined"?'<span class="pill orange">거절됨</span>':'<span class="pill red">실패</span>';
  return '<div class="fl-h st-'+h.state+'"><span class="fl-ft t-'+(h.agent?"agent":"plain")+'">'+(h.dir==="out"?I("up"):I("down"))+'</span><div class="col spacer"><b class="ellipsis">'+esc(h.name)+'</b><span class="small muted fl-wrap">'+(h.dir==="out"?"받는 곳 ":"보낸 곳 ")+esc(flPeerName(h.peer))+'</span><span class="small muted fl-wrap">'+esc(h.time)+(h.why?' · '+esc(h.why):"")+'</span></div>'+
   '<div class="col fl-h-r"><span class="fl-h-size">'+flSize(h.size)+'</span>'+st+'</div></div>'}).join("")+'</div></div>'}).join("")}
function flHistory(){var peers=[]; F.history.forEach(function(h){if(peers.indexOf(h.peer)<0) peers.push(h.peer)});
 return '<div class="card-h"><div class="ico">'+I("clock")+'</div>전송 기록<div class="spacer"></div></div>'+
 '<div class="fl-hfilter"><div class="seg scrollx">'+[["all","전체"],["out","보냄"],["in","받음"],["fail","실패와 거절"]].map(function(x){return '<button data-fdir="'+x[0]+'"'+(x[0]===flDir?' class="on"':"")+'>'+x[1]+'</button>'}).join("")+'</div>'+
 '<select class="field sm fl-peer" aria-label="장비로 거르기"><option value="all">모든 장비</option>'+peers.map(function(p){return '<option value="'+p+'"'+(p===flPeer?" selected":"")+'>'+esc(flPeerName(p))+'</option>'}).join("")+'</select></div>'+
 '<div id="flHist" class="card-b">'+flHistRows()+'</div>'}

/* ---------- page ---------- */
function flPage(){
 render(shell("files",'<main class="main fl-main">'+head("파일","연결된 모든 장비 사이에서 파일을 주고받습니다. 휴대폰과 태블릿도 포함됩니다.",null,
  '<button class="btn sm fl-sharebtn" data-act="fshare" title="휴대폰 공유 시트 미리보기">'+flI("share")+'<span class="lbl">공유 시트</span></button>',
  {primary:'<button class="btn primary" data-act="fpick" title="파일 보내기" aria-label="파일 보내기">'+I("up")+'<span class="lbl">보내기</span></button>'})+
 '<div id="flK">'+flKpis()+'</div>'+flStage()+
 '<div class="fl-cols"><section class="card" id="flT">'+flTransfers()+'</section><section class="card" id="flR">'+flReceived()+'</section></div>'+
 '<section class="card fl-histcard" id="flH">'+flHistory()+'</section></main>'));
 squircleAll(); flBindDrag(); flStart();
 if(F.offer&&!flOfferShown) setTimeout(flShowOffer,2200);
}
function flRefresh(part){var m={T:flTransfers,R:flReceived,H:flHistory}; (part||"TRH").split("").forEach(function(k){var el=$("#fl"+k); if(el){el.innerHTML=m[k](); sqRefresh(el)}}); var k=$("#flK"); if(k) k.innerHTML=flKpis()}

/* ---------- simulation ---------- */
function flStart(){clearInterval(flTick); flTick=setInterval(function(){var fin=[];
 F.transfers.forEach(function(t){if(t.state!=="run") return; var j=t.speed*(0.85+Math.random()*0.3); t.done=Math.min(1,t.done+j*0.5/t.size);
  var pct=Math.round(t.done*100), b=$("#flb-"+t.id), p=$("#flp-"+t.id), s=$("#fls-"+t.id), d=$("#fld-"+t.id);
  if(b) b.style.width=pct+"%"; if(p) p.textContent=pct; if(d) d.textContent=flSize(t.done*t.size)+" / "+flSize(t.size); if(s) s.textContent=Math.round(j)+" MB/s · "+flTime((1-t.done)*t.size/t.speed)+" 남음";
  if(t.done>=1) fin.push(t)});
 if(fin.length){fin.forEach(flFinish); save(); flRefresh()}
},500)}
function flFinish(t){F.transfers=F.transfers.filter(function(x){return x!==t});
 var h={name:t.name,size:t.size,dir:t.dir,peer:t.peer,agent:!!t.agent,path:t.path,day:"오늘",time:nowTime(),state:"done"}; F.history.unshift(h);
 if(t.dir==="in"){F.received.unshift({id:"r"+Date.now(),name:t.name,type:t.type,size:t.size,from:t.peer,time:"오늘 "+nowTime(),saved:F.inbox}); toast(t.name+"을(를) 받았습니다. "+F.inbox+"에 저장됩니다.")}
 else toast(flPeerName(t.peer)+"에 보냈습니다"+(t.agent?". 대화에 첨부되었습니다.":""));
 if(t.dir==="out") F.week.sent=Math.round((F.week.sent+t.size/1024)*10)/10; else F.week.recv=Math.round((F.week.recv+t.size/1024)*10)/10}
function flSend(files,to,agent){var d=agent?null:flDev(to), s=agent?sess(to):null;
 if(d&&!flOnline(d)){toast(d.name+"은(는) 지금 연결되어 있지 않습니다. 연결되면 바로 보냅니다."); return}
 var path=s?((flDev(s.computer)||{}).path||"p2p"):d.path; if(path==="self") path="near";
 files.forEach(function(f,i){F.transfers.unshift({id:"t"+Date.now()+i,name:f.name,type:f.type,size:f.size,done:0,dir:"out",peer:to,agent:!!agent,path:path,speed:path==="near"?120:path==="p2p"?54:22,state:agent||d.kind==="desktop"||d.kind==="server"||d.kind==="laptop"?"run":"wait"})});
 var name=s?mname(s):d.name;
 if(d&&(d.kind==="phone"||d.kind==="tablet")){toast(name+"에서 수락을 기다립니다"); var ts=F.transfers.filter(function(t){return t.state==="wait"}); setTimeout(function(){ts.forEach(function(t){t.state="run"}); toast(name+"에서 수락했습니다"); flRefresh("T")},1800)}
 else toast(files.length+"개 파일을 "+name+(s?"의 작업 폴더로":"(으)로")+" 보냅니다");
 save(); flRefresh("T")}

/* ---------- drag and drop (demo chips and real files from the OS) ---------- */
function flTarget(t){return t&&t.closest?t.closest("[data-fdev],[data-fagent]"):null}
function flBindDrag(){var main=$(".fl-main"); if(!main) return;
 main.addEventListener("dragstart",function(e){var c=e.target.closest&&e.target.closest("[data-ffile]"); if(!c) return; flDrag=[F.tray[+c.dataset.ffile]]; e.dataTransfer.effectAllowed="copy"; try{e.dataTransfer.setData("text/plain",flDrag[0].name)}catch(x){} document.body.classList.add("fl-dragging")});
 main.addEventListener("dragend",function(){document.body.classList.remove("fl-dragging"); flClearDrop()});
 main.addEventListener("dragover",function(e){var tg=flTarget(e.target); if(!tg) return; e.preventDefault(); e.dataTransfer.dropEffect="copy"; document.body.classList.add("fl-dragging"); if(!tg.classList.contains("drop")){flClearDrop(); tg.classList.add("drop")}});
 main.addEventListener("dragleave",function(e){var tg=flTarget(e.target); if(tg&&!tg.contains(e.relatedTarget)) tg.classList.remove("drop")});
 main.addEventListener("drop",function(e){var tg=flTarget(e.target); if(!tg) return; e.preventDefault(); flClearDrop(); document.body.classList.remove("fl-dragging");
  var files=flDrag; if(!files&&e.dataTransfer.files&&e.dataTransfer.files.length) files=[].map.call(e.dataTransfer.files,function(f){return {name:f.name,type:/image/.test(f.type)?"img":/video/.test(f.type)?"video":/audio/.test(f.type)?"audio":/zip|tar|gz/.test(f.type+f.name)?"zip":"doc",size:Math.max(0.01,f.size/1048576)}});
  flDrag=null; if(files) flSend(files,tg.dataset.fdev||tg.dataset.fagent,!!tg.dataset.fagent)});
}
function flClearDrop(){document.querySelectorAll(".fl-dev.drop").forEach(function(x){x.classList.remove("drop")})}

/* ---------- sheets: file picker, incoming offer, share sheet ---------- */
function flPick(to,agent){flPickTo={id:to||null,agent:!!agent};
 var targets=F.devices.filter(function(d){return d.id!==F.me}).map(function(d){return {id:d.id,name:d.name,ic:flI(d.kind),on:flOnline(d),agent:false}}).concat(flAgents().map(function(s){return {id:s.id,name:mname(s),ic:bot(s,22),on:true,agent:true}}));
 sheet('<div class="sheet-h"><b class="sheet-t">파일 보내기</b><div class="sheet-acts"></div><button class="icon-btn sm" data-act="closemodal" aria-label="닫기">'+I("close")+'</button></div>'+
 '<div class="card-b fl-pick"><div class="fl-pick-l small muted">받는 곳</div><div class="fl-targets">'+targets.map(function(x){var on=flPickTo.id===x.id;
  return '<button class="fl-tg'+(on?" on":"")+(x.on?"":" off")+'" data-ftg="'+x.id+'" data-agent="'+(x.agent?1:"")+'" aria-pressed="'+on+'">'+x.ic+'<span class="ellipsis">'+esc(x.name)+'</span></button>'}).join("")+'</div>'+
 '<div class="fl-pick-l small muted">파일 (데모 파일 선택기)</div><div class="fl-picklist">'+F.tray.map(function(f,i){return '<label class="fl-pf"><input type="checkbox" name="fpf" value="'+i+'"'+(i===0?" checked":"")+'><span class="fl-ft t-'+f.type+'">'+flI(f.type)+'</span><span class="col spacer" style="min-width:0"><span class="ellipsis">'+esc(f.name)+'</span><span class="small muted">'+FL_TYPE[f.type]+' · '+flSize(f.size)+'</span></span></label>'}).join("")+'</div>'+
 '<div class="fl-pick-foot">'+flRoute("p2p")+'<span class="spacer"></span><button class="btn primary" data-act="fpicksend">'+I("up")+'보내기</button></div></div>',"flpick")}
function flShowOffer(){if(!F.offer||$(".fl-offer")||screenName()!=="files") return; flOfferShown=true; var o=F.offer, d=flDev(o.from), tot=o.files.reduce(function(a,f){return a+f.size},0);
 var el=document.createElement("div"); el.className="card fl-offer"; el.setAttribute("role","alertdialog"); el.setAttribute("aria-label","받기 요청");
 el.innerHTML='<div class="fl-offer-top"><div class="fl-dev-ic">'+flI(d.kind)+'</div><div class="col spacer"><b>'+esc(d.name)+'</b><span class="small muted">파일 '+o.files.length+'개, '+flSize(tot)+'를 보내려고 합니다</span></div></div>'+
  '<div class="fl-offer-files">'+o.files.map(function(f){return '<span class="fl-ft t-'+f.type+'" title="'+esc(f.name)+'">'+flI(f.type)+'</span>'}).join("")+'<span class="small muted ellipsis">'+esc(o.files.map(function(f){return f.name}).join(", "))+'</span></div>'+
  '<div class="small muted">저장 위치 <span class="mono-path">'+esc(F.inbox)+'</span></div>'+flRoute(d.path)+
  '<div class="fl-offer-acts"><button class="btn" data-act="fdecline">거절</button><button class="btn primary" data-act="faccept">받기</button></div>';
 document.body.appendChild(el); squircle(el)}
function flShare(){var devs=F.devices.filter(function(d){return d.id!=="iphone"});
 sheet('<div class="fl-share" aria-label="휴대폰 공유 시트 미리보기"><div class="fl-share-grab"></div>'+
 '<div class="fl-share-file"><span class="fl-ft t-img">'+flI("img")+'</span><div class="col spacer"><b>IMG_4102.HEIC 외 2개</b><span class="small muted">사진 3장 · 6.4 MB</span></div><button class="icon-btn sm" data-act="closemodal" aria-label="닫기">'+I("close")+'</button></div>'+
 '<div class="fl-share-l small muted">Ember로 보내기</div><div class="fl-share-row">'+devs.map(function(d){var on=flOnline(d);
  return '<button class="fl-sh-dev'+(on?"":" off")+'" data-fshto="'+d.id+'"><span class="fl-sh-av">'+flI(d.kind)+(on?'<i class="fl-sh-'+d.path+'"></i>':"")+'</span><span class="ellipsis">'+esc(d.name)+'</span><span class="fl-sh-s">'+(d.id===F.me?"Mac":on?FL_PATH[d.path][0]:"꺼짐")+'</span></button>'}).join("")+'</div>'+
 '<div class="fl-share-l small muted">팀원에게</div><div class="fl-share-row">'+flAgents().slice(0,3).map(function(s){return '<button class="fl-sh-dev" data-fshag="'+s.id+'"><span class="fl-sh-av bot">'+bot(s,30)+'</span><span class="ellipsis">'+esc(mname(s))+'</span><span class="fl-sh-s">'+esc(s.computer)+'</span></button>'}).join("")+'</div>'+
 '<div class="fl-share-acts"><div class="fl-sh-act">'+flI("doc")+'<span>파일에 저장</span></div><div class="fl-sh-act">'+I("chat")+'<span>채널에 올리기</span></div><div class="fl-sh-act">'+I("folder")+'<span>프로젝트에 넣기</span></div></div>'+
 '<div class="fl-share-note small muted">'+flI("lock")+'휴대폰에서 공유 버튼을 누르면 이 시트가 뜹니다. 받는 장비에서 수락하면 전송이 시작됩니다.</div></div>',"flshare")}

/* ---------- screen ---------- */
Ember.screens.files={render:flPage,
 onModalClose:function(){flPickTo=null},
 click:function(e,t){var a;
  if((a=t.closest("[data-fdir]"))){flDir=a.dataset.fdir; document.querySelectorAll("[data-fdir]").forEach(function(b){b.classList.toggle("on",b===a)}); $("#flHist").innerHTML=flHistRows(); return true}
  if((a=t.closest("[data-ftp]"))){var x=F.transfers.filter(function(z){return z.id===a.dataset.ftp})[0]; x.state="paused"; save(); flRefresh("T"); toast("일시 정지했습니다. 받은 부분은 남아 있습니다."); return true}
  if((a=t.closest("[data-ftr]"))){var y=F.transfers.filter(function(z){return z.id===a.dataset.ftr})[0], dv=flDev(y.peer);
   if(y.state==="broken"&&dv&&!flOnline(dv)){y.state="run"; y.speed=22; y.path="relay"; delete y.note; toast(Math.round(y.done*100)+"%부터 이어서 보냅니다. 릴레이로 다시 연결했습니다.")} else {y.state="run"; if(!y.speed) y.speed=40; toast("이어서 전송합니다")}
   save(); flRefresh("T"); return true}
  if((a=t.closest("[data-ftx]"))){var z=F.transfers.filter(function(q){return q.id===a.dataset.ftx})[0]; F.transfers=F.transfers.filter(function(q){return q!==z}); F.history.unshift({name:z.name,size:z.size,dir:z.dir,peer:z.peer,path:z.path,day:"오늘",time:nowTime(),state:"fail",why:"취소했습니다"}); save(); flRefresh(); toast("전송을 취소했습니다"); return true}
  if((a=t.closest("[data-fopen]"))){var r=F.received.filter(function(q){return q.id===a.dataset.fopen})[0]; toast(r.name+"을(를) 기본 앱으로 엽니다 (데모)"); return true}
  if((a=t.closest("[data-freveal]"))){var r2=F.received.filter(function(q){return q.id===a.dataset.freveal})[0]; toast("Finder에서 "+r2.saved+"을(를) 엽니다 (데모)"); return true}
  if((a=t.closest("[data-fdev]"))){flPick(a.dataset.fdev,false); return true}
  if((a=t.closest("[data-fagent]"))){flPick(a.dataset.fagent,true); return true}
  if((a=t.closest("[data-ftg]"))){flPickTo={id:a.dataset.ftg,agent:!!a.dataset.agent}; document.querySelectorAll("[data-ftg]").forEach(function(b){b.classList.toggle("on",b===a); b.setAttribute("aria-pressed",b===a)}); return true}
  if((a=t.closest("[data-fshto],[data-fshag]"))){var ag=!!a.dataset.fshag, to=a.dataset.fshto||a.dataset.fshag; closeSheet("flshare"); flSend(F.offer?F.offer.files:[{name:"IMG_4102.HEIC",type:"img",size:3.1}],to,ag); return true}
  if((a=t.closest(".fl-sh-act"))){toast(a.textContent+" (데모)"); return true}
  if((a=t.closest("[data-act]"))){var act=a.dataset.act;
   if(act==="fpick"){flPick(null,false); return true}
   if(act==="fshare"){flShare(); return true}
   if(act==="fpicksend"){if(!flPickTo||!flPickTo.id){toast("받는 곳을 고르세요"); return true}
    var ch=[].map.call(document.querySelectorAll('input[name=fpf]:checked'),function(i){return F.tray[+i.value]}); if(!ch.length){toast("보낼 파일을 고르세요"); return true}
    var p=flPickTo; closeSheet("flpick"); flPickTo=null; flSend(ch,p.id,p.agent); return true}
   if(act==="faccept"||act==="fdecline"){var o=F.offer, el=$(".fl-offer"); if(el) el.remove(); F.offer=null;
    if(act==="faccept"){o.files.forEach(function(f,i){F.transfers.unshift({id:"ti"+Date.now()+i,name:f.name,type:f.type,size:f.size,done:0,dir:"in",peer:o.from,path:flDev(o.from).path,speed:28,state:"run"})}); toast("받기 시작했습니다")}
    else {F.history.unshift({name:o.files[0].name+" 외 "+(o.files.length-1)+"개",size:o.files.reduce(function(s,f){return s+f.size},0),dir:"in",peer:o.from,path:"near",day:"오늘",time:nowTime(),state:"declined"}); toast("거절했습니다. 보낸 장비에 알립니다.")}
    save(); flRefresh(); return true}
  }
  return false}};
document.addEventListener("change",function(e){if(e.target.classList&&e.target.classList.contains("fl-peer")){flPeer=e.target.value; var h=$("#flHist"); if(h) h.innerHTML=flHistRows()}});
document.addEventListener("keydown",function(e){if((e.key==="Enter"||e.key===" ")&&e.target.matches&&e.target.matches("[data-fdev],[data-fagent]")){e.preventDefault(); e.target.click()}});
