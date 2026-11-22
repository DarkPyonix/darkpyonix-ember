"use strict";
/* each channel gets its own animated mark: motif by room, colour by project */
var ROOM_MOTIF={r1:"pulse",r2:"bars",r3:"radar",r4:"orbit",r5:"wave",r6:"spark"}, MOTIFS=["pulse","bars","radar","orbit","wave","spark"];
var ROOM_TINT={ember:"var(--c-blue)",core:"var(--c-orange)",pyx:"var(--c-sky)",site:"var(--c-green)",unsorted:"var(--c-grey)"};
function roomIcon(r,size){var m=ROOM_MOTIF[r.id]||MOTIFS[(r.id||"").split("").reduce(function(a,ch){return a+ch.charCodeAt(0)},0)%MOTIFS.length], c=ROOM_TINT[r.project]||"var(--c-blue)", g;
 if(m==="pulse") g='<circle class="rp1" cx="20" cy="20" r="6"/><circle class="rp2" cx="20" cy="20" r="6"/><circle cx="20" cy="20" r="4.5" class="fillc"/>';
 else if(m==="bars") g='<rect class="rb rb1" x="11" y="12" width="4" height="16" rx="2"/><rect class="rb rb2" x="18" y="12" width="4" height="16" rx="2"/><rect class="rb rb3" x="25" y="12" width="4" height="16" rx="2"/>';
 else if(m==="radar") g='<circle cx="20" cy="20" r="11" class="ln"/><circle cx="20" cy="20" r="5" class="ln"/><path class="sweep" d="M20 20 L20 9 A11 11 0 0 1 29.5 14.5 Z"/>';
 else if(m==="orbit") g='<circle cx="20" cy="20" r="10" class="ln"/><circle cx="20" cy="20" r="3.5" class="fillc"/><g class="orb"><circle cx="30" cy="20" r="2.6" class="fillc"/></g>';
 else if(m==="wave") g='<path class="wv" d="M4 20 q4 -7 8 0 t8 0 t8 0 t8 0 t8 0 t8 0"/>';
 else g='<path class="sp sp1" d="M20 10 l2 6 6 2 -6 2 -2 6 -2 -6 -6 -2 6 -2z"/><circle class="sp sp2" cx="29" cy="12" r="1.6"/><circle class="sp sp3" cx="12" cy="28" r="1.3"/>';
 return '<span class="roomicon ri-'+m+'" style="--rc:'+c+';width:'+size+'px;height:'+size+'px" role="img" aria-label="'+esc(r.name)+' 채널"><svg viewBox="0 0 40 40" aria-hidden="true">'+g+'</svg></span>'}
/* ---------- channels (KakaoTalk-style group rooms) ---------- */
var room=null, chFilter="all";
function roomList(q){
 var l=D.rooms.slice().sort(function(a,b){return b.t-a.t}).filter(function(r){return (chFilter==="all"||r.project===chFilter)&&(!q||(r.name+lastMsg(r).text).toLowerCase().indexOf(q)>=0)});
 return l.map(function(r){var lm=lastMsg(r);return '<div class="room-item'+(room&&room.id===r.id?" on":"")+'" data-open="'+r.id+'">'+roomIcon(r,44)+'<div class="spacer"><div class="nm"><span class="ellipsis">'+esc(r.name)+'</span><small>'+r.members.length+'</small></div><div class="last ellipsis">'+esc(lm.text)+'</div></div><div class="meta"><span>'+lm.time.replace(/^(오전|오후) /,"$1 ")+'</span>'+(r.unread?'<span class="redbadge">'+r.unread+'</span>':'<span style="height:18px"></span>')+'</div></div>'}).join("")||'<div class="empty">채널이 없습니다</div>'
}
function roomHtml(){
 var r=room, out=[], prev=null;
 r.msgs.forEach(function(m){
  if(m.date){out.push('<div class="datediv">'+m.date+'</div>');prev=null;return}
  if(m.sys){out.push('<div class="sysmsg">'+esc(m.sys)+'</div>');prev=null;return}
  var mine=m.from===D.me, cont=prev&&prev.from===m.from&&prev.time===m.time;
  var stamp='<div class="stamp">'+(m.unread?'<span class="ur">'+m.unread+'</span>':"")+'<span>'+m.time+'</span></div>';
  var body=m.card?'<div class="agentcard"><div class="h">'+I("git")+esc(m.card.title)+'</div><div class="b">'+esc(m.text)+'<div class="small muted">'+esc(m.card.body)+'</div></div></div>':'<div class="bub">'+esc(m.text)+'</div>';
  var tag=(D.people[m.from]||{}).agent?' <span class="pill sm">에이전트</span>':"";
  if(mine) out.push('<div class="km mine"><div class="row">'+body+stamp+'</div></div>');
  else if(cont) out.push('<div class="km cont"><div class="row">'+body+stamp+'</div></div>');
  else out.push('<div class="km">'+av(m.from)+'<div class="col"><div class="nm">'+esc(m.from)+tag+'</div><div class="row">'+body+stamp+'</div></div></div>');
  prev=m;
 });
 return out.join("")
}
function renderRoom(){
 var d=$(".detail");
 d.innerHTML='<div class="topbar"><button class="icon-btn sm backbtn" data-back>'+I("back")+'</button>'+roomIcon(room,30)+'<b class="ellipsis">'+esc(room.name)+'</b><span class="muted">'+room.members.length+'</span><span class="ch-proj hide-sm"><span class="dot"></span>'+proj(room.project).name+'</span><div class="spacer"></div><button class="icon-btn sm" data-act="members" title="참여자">'+I("user")+'</button><button class="icon-btn sm" title="메뉴">'+I("menu")+'</button></div>'+
 '<div class="chat">'+roomHtml()+'</div><div class="inputbar"><button class="icon-btn sm">'+I("plus")+'</button><input id="rmsg" placeholder="메시지 입력"><button class="icon-btn sm hide-sm">'+I("smile")+'</button><button class="sendbtn" data-act="rsend">전송</button></div>';
 var c=$(".chat"); c.scrollTop=c.scrollHeight;
 $("#rmsg").addEventListener("keydown",function(e){if(e.key==="Enter"&&!e.isComposing){e.preventDefault();sendRoom()}});
}
function refreshRooms(){$("#roomList").innerHTML=roomList($("#rq").value.toLowerCase()); save(); var side=$(".side"),t=document.createElement("div"); t.innerHTML=shell("channels",""); side.innerHTML=t.querySelector(".side").innerHTML}
function openRoom(r){room=r; r.unread=0; r.msgs.forEach(function(m){if(m.unread&&m.from!==D.me) m.unread=Math.max(0,m.unread-1)}); renderRoom(); refreshRooms()}
function sendRoom(){
 var inp=$("#rmsg"), v=inp.value.trim(); if(!v) return; var r=room;
 var m={from:D.me,text:v,time:nowTime(),unread:r.members.length-1}; r.msgs.push(m); r.t=maxT(); renderRoom(); refreshRooms(); $("#rmsg").focus();
 var agents=r.members.filter(function(x){return (D.people[x]||{}).agent});
 setTimeout(function(){m.unread=Math.max(0,m.unread-agents.length); if(room===r) renderRoom(); save()},900);
 if(agents.length) setTimeout(function(){r.msgs.push({from:agents[0],text:"확인했습니다. 관련 세션에 반영합니다.",time:nowTime(),unread:r.members.length-2}); r.t=maxT(); if(room===r) renderRoom(); else r.unread++; refreshRooms()},1800);
}
function channels(){
 var seg='<div class="seg scrollx" style="padding:0 var(--sp3) var(--sp2)"><button class="on" data-chf="all">전체</button>'+D.projects.map(function(p){return '<button data-chf="'+p.id+'">'+(p.id==="unsorted"?"기본 프로젝트":p.name.replace("darkpyonix-",""))+'</button>'}).join("")+'</div>';
 render(shell("channels",'<div class="split"><div class="listpane"><div class="ph"><h2>채널</h2><div class="spacer"></div><button class="icon-btn sm" data-act="newroom" title="새 채널">'+I("plus")+'</button></div><label class="searchbox" style="width:auto;margin:0 var(--sp3) var(--sp2)">'+I("search")+'<input id="rq" placeholder="채널, 메시지 검색"></label>'+seg+'<div class="listscroll" id="roomList"></div></div><section class="detail room"></section></div>'));
 var id=location.hash.slice(1), r=D.rooms.filter(function(x){return x.id===id})[0];
 if(r) document.body.classList.add("view-detail");
 room=r||D.rooms.slice().sort(function(a,b){return b.t-a.t})[0];
 if(r||!matchMedia("(max-width:560px)").matches) openRoom(room); else {renderRoom(); refreshRooms()}
 $("#rq").addEventListener("input",function(){$("#roomList").innerHTML=roomList(this.value.toLowerCase())});
}
Ember.screens.channels={render:channels,click:function(e,t){var a;
 if((a=t.closest("[data-open]"))){var id=a.dataset.open; document.body.classList.add("view-detail"); history.replaceState(null,"","#"+id); openRoom(D.rooms.filter(function(r){return r.id===id})[0]); return true}
 if((a=t.closest("[data-chf]"))){chFilter=a.dataset.chf; a.parentNode.querySelectorAll("button").forEach(function(b){b.classList.toggle("on",b===a)}); $("#roomList").innerHTML=roomList($("#rq").value.toLowerCase()); return true}
 if((a=t.closest("[data-act=rsend]"))){sendRoom(); return true}
 if((a=t.closest("[data-act=members]"))){toast(room.members.join(", ")); return true}
 if((a=t.closest("[data-act=newroom]"))){toast("새 채널 (데모)"); return true}
}};
