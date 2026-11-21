/* Ember agent characters: six team styles drawn from one seed (member name + harness).
   API: window.EmberChar = { styles, draw(member, opts), stateOf(member), seedName(member) }
   draw returns an HTML string: <span class="ec ..."><svg>...</svg></span>. Pure, no DOM access, no dependencies.
   Motion lives in chars.css and stops under prefers-reduced-motion. */
(function(){
"use strict";
var STYLES=[
 {id:"mongle",name:"몽글",desc:"눈, 입, 볼이 있는 기본 캐릭터입니다. 이름에 따라 몸 모양과 색이 조금씩 달라집니다."},
 {id:"malang",name:"말랑",desc:"이름으로 정해지는 말랑한 블롭입니다. 얼굴은 눈만 있고 상태는 몸의 움직임으로 보입니다."},
 {id:"crew",name:"크루",desc:"짧은 팔다리가 달린 둥근 사각 몸에 역할 소품(왕관, 넥타이, 헤드폰, 모자)을 답니다."},
 {id:"pocket",name:"포켓",desc:"삼각형, 구름, 토끼, 물방울 같은 단순한 몸에 소품 하나를 단 마스코트입니다. 둥둥 떠 있습니다."},
 {id:"cream",name:"크림",desc:"크림색 둥근 몸에 하네스 색 옷을 입습니다. 점 눈과 작은 입만 있습니다."},
 {id:"keyring",name:"키링",desc:"고리에 매달려 흔들리는 작은 참입니다. 좁은 자리에 맞습니다."}];

/* ---------- seed ---------- */
function hash(s){var h=2166136261>>>0; s=String(s); for(var i=0;i<s.length;i++){h^=s.charCodeAt(i); h=Math.imul(h,16777619)>>>0} return h}
function rng(seed){var a=seed>>>0; return function(){a=(a+0x6D2B79F5)>>>0; var t=a; t=Math.imul(t^(t>>>15),t|1); t^=t+Math.imul(t^(t>>>7),t|61); return ((t^(t>>>14))>>>0)/4294967296}}
function harness(agent){agent=String(agent||""); return /codex/i.test(agent)?"codex":/gemini/i.test(agent)?"gemini":"claude"}
function seedName(m){return m.seedName||m.name||m.label||m.id||"member"}
var STATE={run:"run",wait:"wait",fail:"fail",done:"done",unread:"done",idle:"idle"};
function stateOf(m){return STATE[m&&m.status]||"idle"}

/* ---------- colour: families of the app palette (--v-claude orange, --v-codex blue, --v-gemini sky),
   muted to sit on the warm grey surface; lightness bands keep Codex (deeper) and Gemini (paler) apart ---------- */
var FAM={claude:{h:[20,34],s:[48,62],l:[60,72]},codex:{h:[218,238],s:[30,42],l:[54,66]},gemini:{h:[196,212],s:[40,54],l:[72,82]}};
function palette(t,h){var f=FAM[h], r=t.r2;
 var hue=f.h[0]+r[0]*(f.h[1]-f.h[0]), sat=f.s[0]+r[1]*(f.s[1]-f.s[0]), lig=f.l[0]+r[2]*(f.l[1]-f.l[0]);
 var hs=function(dl,ds){return "hsl("+hue.toFixed(0)+" "+Math.max(0,sat+(ds||0)).toFixed(0)+"% "+Math.min(96,Math.max(8,lig+dl)).toFixed(0)+"%)"};
 return {fill:hs(0),hi:hs(12,-6),dk:hs(-16,4),acc:hs(-24,6),ink:lig<58?"#fbfaf7":"#26221f"}}

/* ---------- traits ---------- */
var SHAPES=["round","organic","boxy","capsule","cloud","droplet","nub","pebble"];
function traits(name,h,pin){var r=rng(hash(name+"|"+h)), t={};
 t.shape=(pin&&pin.shape)||SHAPES[Math.floor(r()*SHAPES.length)];
 t.w=.86+r()*.24; t.h=.84+r()*.22; t.wob=.04+r()*.07; t.rot=r()*6.28;
 t.r2=[r(),r(),r()];
 t.eyeW=4.2+r()*2.6; t.eyeH=8.5+r()*6; t.eyeGap=8.5+r()*5; t.eyeY=-2+r()*6; t.gaze=(r()-.5)*3; t.eyeRound=r()>.35;
 t.j=[]; for(var i=0;i<12;i++) t.j.push(r());
 t.k=r(); t.k2=r();
 return t}

/* ---------- silhouette ---------- */
function radius(t,a){var s=t.shape, c=Math.cos(a), si=Math.sin(a), r=1;
 if(s==="boxy"){r=1/Math.pow(Math.pow(Math.abs(c),4)+Math.pow(Math.abs(si),4),1/4)*.92}
 else if(s==="capsule"){r=1/Math.pow(Math.pow(Math.abs(c)/1.25,4)+Math.pow(Math.abs(si)/.82,2.2),1/3.2)}
 else if(s==="cloud"){r=.9+.1*Math.cos(5*a+t.rot)}
 else if(s==="droplet"){r=1+.32*Math.pow(Math.max(0,-si),4)}
 else if(s==="nub"){r=.94+.14*Math.pow(Math.max(0,Math.cos(a-(-1.57+.6*(t.j[0]-.5)))),24)}
 else if(s==="pebble"){r=1/Math.pow(Math.pow(Math.abs(c)/1.1,2.6)+Math.pow(Math.abs(si)/.9,2.6),1/2.6)}
 else if(s==="organic"){r=1+.09*Math.sin(3*a+t.rot)}
 return r}
function blobPath(t,cx,cy,R){var n=t.shape==="cloud"?20:16, P=[], i;
 for(i=0;i<n;i++){var a=i/n*6.2832-1.5708, k=radius(t,a)*(1+(t.j[i%12]-.5)*t.wob*(t.shape==="boxy"?.4:1)); P.push([cx+Math.cos(a)*R*t.w*k, cy+Math.sin(a)*R*t.h*k])}
 var f=function(v){return v.toFixed(2)}, d="M"+f(P[0][0])+" "+f(P[0][1]);
 for(i=0;i<n;i++){var p0=P[(i-1+n)%n],p1=P[i],p2=P[(i+1)%n],p3=P[(i+2)%n];
  d+=" C"+f(p1[0]+(p2[0]-p0[0])/6)+" "+f(p1[1]+(p2[1]-p0[1])/6)+" "+f(p2[0]-(p3[0]-p1[0])/6)+" "+f(p2[1]-(p3[1]-p1[1])/6)+" "+f(p2[0])+" "+f(p2[1])}
 return d+"Z"}

/* ---------- props ---------- */
function crown(cx,top){return '<g class="ec-crown" transform="translate('+cx+' '+(top-1)+')"><path d="M-9 0 L-10 -10 L-4.5 -5 L0 -12 L4.5 -5 L10 -10 L9 0Z" fill="#f2b84b" stroke="#c98a1d" stroke-width="1" stroke-linejoin="round"/></g>'}
function prop(kind,cx,top,R,ink,acc){
 if(kind==="cap") return '<g transform="translate('+cx+' '+(top+3)+')"><path d="M-13 0a13 10 0 0 1 26 0z" fill="'+acc+'"/><path d="M8 0h12" stroke="'+acc+'" stroke-width="3.2" stroke-linecap="round"/></g>';
 if(kind==="phones") return '<path d="M'+(cx-R*.78)+' '+(top+R*.55)+'a'+(R*.78)+' '+(R*.62)+' 0 0 1 '+(R*1.56)+' 0" fill="none" stroke="'+ink+'" stroke-width="2.6" stroke-linecap="round"/><rect x="'+(cx-R*.92)+'" y="'+(top+R*.48)+'" width="7" height="12" rx="3.5" fill="'+acc+'"/><rect x="'+(cx+R*.92-7)+'" y="'+(top+R*.48)+'" width="7" height="12" rx="3.5" fill="'+acc+'"/>';
 if(kind==="tie") return '<path d="M'+cx+' '+(top+R*1.32)+'l-3.4 4 3.4 11 3.4-11z" fill="'+acc+'"/>';
 if(kind==="bow") return '<path d="M'+cx+' '+(top+R*1.36)+'l-8-4.4v8.8zM'+cx+' '+(top+R*1.36)+'l8-4.4v8.8z" fill="'+acc+'"/><circle cx="'+cx+'" cy="'+(top+R*1.36)+'" r="2" fill="'+acc+'"/>';
 if(kind==="beret") return '<ellipse cx="'+(cx+2)+'" cy="'+(top+2)+'" rx="13" ry="5" fill="'+acc+'"/><circle cx="'+(cx+2)+'" cy="'+(top-3)+'" r="1.8" fill="'+acc+'"/>';
 return ""}
var ROLEPROP={"리뷰어":"tie","검토":"tie","테스터":"phones","테스트":"phones","디자인":"cap","문서":"bow","카피":"bow","조사":"beret","빌드":"phones","배포":"cap","인프라":"cap"};
function roleProp(m,t){var k=ROLEPROP[m.role]; if(k) return k; return ["tie","phones","cap","bow","beret"][Math.floor(t.k*5)]}

/* ---------- eyes ---------- */
function eyes(t,cx,cy,ink,st,style){var g=t.eyeGap, w=t.eyeW, h=t.eyeH, y=cy+t.eyeY;
 if(style==="dot"){w=h=Math.max(3.6,t.eyeW*1.0); g=Math.min(g,9)}
 if(st==="fail") return '<path d="M'+(cx-g-w/2)+' '+(y+1)+'h'+(w+2)+'M'+(cx+g-w/2-2)+' '+(y+1)+'h'+(w+2)+'" stroke="'+ink+'" stroke-width="2.2" stroke-linecap="round"/>';
 if(st==="done") return '<path d="M'+(cx-g-3.5)+' '+(y+1)+'q3.5-4.5 7 0M'+(cx+g-3.5)+' '+(y+1)+'q3.5-4.5 7 0" stroke="'+ink+'" stroke-width="2.2" fill="none" stroke-linecap="round"/>';
 var rr=style==="dot"?w/2:(t.eyeRound?w/2:1.4);
 return '<g class="ec-ey"><rect x="'+(cx-g-w/2+t.gaze)+'" y="'+(y-h/2)+'" width="'+w+'" height="'+h+'" rx="'+rr+'" fill="'+ink+'"/><rect x="'+(cx+g-w/2+t.gaze)+'" y="'+(y-h/2)+'" width="'+w+'" height="'+h+'" rx="'+rr+'" fill="'+ink+'"/></g>'}

/* ---------- styles ---------- */
var uid=0;
var MONGLE={claude:["round","bean","egg"],codex:["squircle","pill","squircle"],gemini:["star","drop","star"]};
function mongleBody(kind,c){
 if(kind==="round") return '<path d="M50 24c16 0 27 11 27 28S66 80 50 80 23 69 23 52s11-28 27-28z" fill="'+c+'"/>';
 if(kind==="bean") return '<path d="M38 26c12-4 20 2 26 0 10-3 16 8 14 24-2 18-12 30-28 30S22 68 22 52c0-12 6-23 16-26z" fill="'+c+'"/>';
 if(kind==="egg") return '<path d="M50 20c14 0 25 16 25 34 0 16-11 26-25 26S25 70 25 54c0-18 11-34 25-34z" fill="'+c+'"/>';
 if(kind==="squircle") return '<path d="M34 26h32c8 0 12 4 12 12v28c0 8-4 12-12 12H34c-8 0-12-4-12-12V38c0-8 4-12 12-12z" fill="'+c+'"/>';
 if(kind==="pill") return '<rect x="26" y="22" width="48" height="58" rx="24" fill="'+c+'"/>';
 if(kind==="drop") return '<path d="M50 20c9 13 26 25 26 40 0 13-11 20-26 20S24 73 24 60c0-15 17-27 26-40z" fill="'+c+'"/>';
 return '<path d="M50 20c4 13 12 21 27 27-15 5-23 14-27 31-4-17-12-26-27-31 15-6 23-14 27-27z" fill="'+c+'"/>'}
var MOUTH={done:"M44 60q6 5 12 0",run:"M45 61h10",wait:"M47 60a3 3 0 0 0 6 0",fail:"M44 63q6-5 12 0",idle:"M45 60q5 3.5 10 0"};

function draw(m,o){
 m=m||{}; o=o||{};
 var style=o.style||"mongle", h=harness(m.agent||o.agent), name=o.name||seedName(m), st=o.state||stateOf(m), size=o.size||36, lead=o.lead!=null?!!o.lead:!!m.lead;
 var t=traits(name,h,o.pin), c=palette(t,h), cx=50, cy=58, R=30, body="", face="", extra="", defs="", cls="ec ec-"+style+" ec-s-"+st, top=0;
 if(style==="malang"){
  var hid="ech"+(uid++);
  defs='<linearGradient id="'+hid+'" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".28"/><stop offset=".45" stop-color="#fff" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".1"/></linearGradient>';
  var bp=blobPath(t,cx,cy,R); body='<path d="'+bp+'" fill="'+c.fill+'"/><path d="'+bp+'" fill="url(#'+hid+')"/>';
  face=eyes(t,cx,cy-2,c.ink,st,"pill"); top=cy-R*t.h;
 } else if(style==="mongle"){
  var kinds=MONGLE[h], kind=kinds[Math.floor(t.k*kinds.length)];
  body=mongleBody(kind,c.fill)+'<path d="M33 60h4M63 60h4" stroke="rgba(255,255,255,.4)" stroke-width="2.6" stroke-linecap="round"/>';
  var ey=st==="done"?'<path d="M37 49q4-4 8 0M55 49q4-4 8 0" stroke="#26221f" stroke-width="2.4" fill="none" stroke-linecap="round"/>':st==="fail"?'<path d="M37 50h7M56 50h7" stroke="#26221f" stroke-width="2.4" stroke-linecap="round"/>':'<g class="ec-ey"><ellipse cx="41" cy="49" rx="3" ry="4" fill="#26221f"/><ellipse cx="59" cy="49" rx="3" ry="4" fill="#26221f"/></g>';
  face=ey+'<path d="'+MOUTH[st]+'" fill="none" stroke="#26221f" stroke-width="2.2" stroke-linecap="round"/>';
  if(st==="wait") extra+='<g class="ec-wave" style="transform-origin:74px 46px"><circle cx="82" cy="38" r="5" fill="'+c.fill+'"/></g>';
  if(st==="run") extra+='<g class="ec-busy"><circle cx="78" cy="22" r="2.4" fill="var(--c-blue,#3f74d4)"/><circle cx="85" cy="16" r="1.8" fill="var(--c-blue,#3f74d4)"/></g>';
  if(st==="fail") extra+='<path d="M77 30c2 3 3 4.6 3 6a3 3 0 0 1-6 0c0-1.4 1-3 3-6z" fill="var(--c-sky,#9dbde9)"/>';
  top=kind==="star"?22:24;
 } else if(style==="crew"){
  var bw=40+t.k*10, bh=42+t.k2*8, bx=50-bw/2, by=22, br=11+t.k*6, legY=by+bh, armY=by+bh*.55;
  body='<g class="ec-legs"><rect x="'+(50-bw*.28-4)+'" y="'+(legY-4)+'" width="8" height="14" rx="4" fill="'+c.dk+'"/><rect x="'+(50+bw*.28-4)+'" y="'+(legY-4)+'" width="8" height="14" rx="4" fill="'+c.dk+'"/></g>'+
   '<g class="ec-arm ec-arm-l" style="transform-origin:'+(bx+2)+'px '+armY+'px"><rect x="'+(bx-7)+'" y="'+armY+'" width="8" height="16" rx="4" fill="'+c.dk+'"/></g>'+
   '<g class="ec-arm ec-arm-r" style="transform-origin:'+(bx+bw-2)+'px '+armY+'px"><rect x="'+(bx+bw-1)+'" y="'+armY+'" width="8" height="16" rx="4" fill="'+c.dk+'"/></g>'+
   '<rect x="'+bx+'" y="'+by+'" width="'+bw+'" height="'+bh+'" rx="'+br+'" fill="'+c.fill+'"/><rect x="'+(bx+4)+'" y="'+(by+3)+'" width="'+(bw-8)+'" height="'+(bh*.32)+'" rx="'+(br-3)+'" fill="#fff" opacity=".16"/>';
  t.eyeGap=8; t.eyeW=4.2; t.eyeH=6.4; t.eyeY=0; t.gaze=0; t.eyeRound=true;
  face=eyes(t,50,by+bh*.42,c.ink,st,"pill");
  var pk=roleProp(m,t); if(!(lead&&pk==="cap")) extra+=prop(pk,50,by,bw/2*.95,c.ink,c.acc);
  top=by;
 } else if(style==="pocket"){
  var kd=["tri","cloud","bunny","drop"][Math.floor(t.k*4)], d;
  if(kd==="tri") d="M50 26 C54 26 82 76 80 80 C78 84 22 84 20 80 C18 76 46 26 50 26Z";
  else if(kd==="cloud") d="M30 80 C14 80 14 58 28 56 C26 40 46 34 54 44 C64 34 82 44 76 58 C90 62 86 80 72 80Z";
  else if(kd==="bunny") d="M38 46 C34 30 36 18 41 18 C46 18 46 34 46 44 L54 44 C54 34 54 18 59 18 C64 18 66 30 62 46 C76 52 78 82 50 82 C22 82 24 52 38 46Z";
  else d="M50 24 C62 42 76 54 76 66 C76 80 64 86 50 86 C36 86 24 80 24 66 C24 54 38 42 50 24Z";
  body='<path d="'+d+'" fill="'+c.fill+'"/>'; t.eyeGap=8; t.eyeW=4.2; t.gaze=0; t.eyeY=0;
  face=eyes(t,50,kd==="tri"?66:62,c.ink,st,"dot");
  var dp={tri:"bow",cloud:"beret",bunny:"phones",drop:"tie"}[kd];
  extra+=prop(dp,50,kd==="bunny"?34:(kd==="tri"?30:40),24,c.ink,c.acc);
  top=kd==="bunny"?18:(kd==="tri"?28:36); cls+=" ec-float";
 } else if(style==="cream"){
  body='<ellipse cx="50" cy="58" rx="29" ry="27" fill="var(--ec-cream,#f4ecdc)"/><path d="M24 70 C30 86 70 86 76 70 C70 76 30 76 24 70Z" fill="'+c.fill+'"/><path d="M42 72 L50 79 L58 72" fill="none" stroke="'+c.dk+'" stroke-width="2"/>';
  t.eyeGap=8.5; t.eyeW=4; t.gaze=0; t.eyeY=0;
  face=eyes(t,50,54,"#2a2522",st,"dot")+(st==="fail"||st==="done"?"":'<path d="M46.5 63 q3.5 2.4 7 0" stroke="#2a2522" stroke-width="1.7" fill="none" stroke-linecap="round"/>');
  top=32;
 } else if(style==="keyring"){
  body='<path d="'+blobPath(t,cx,cy+4,R*.86)+'" fill="'+c.fill+'"/>';
  face=eyes(t,cx,cy+2,c.ink,st,"dot");
  extra='<circle cx="50" cy="12" r="5" fill="none" stroke="var(--c-grey,#b4b2ac)" stroke-width="2"/><path d="M50 17 V'+(cy+4-R*.86*t.h+1)+'" stroke="var(--c-grey,#b4b2ac)" stroke-width="2"/>';
  top=cy+4-R*.86*t.h; cls+=" ec-swing";
 }
 if(lead) extra+=crown(style==="keyring"?62:50,style==="keyring"?top+6:top);
 var label=(o.label||name)+(lead?" (리더)":"");
 return '<span class="'+cls+'" style="width:'+size+'px;height:'+size+'px" role="img" aria-label="'+String(label).replace(/[<>"&]/g,"")+'"><svg viewBox="0 0 100 100" aria-hidden="true">'+(defs?'<defs>'+defs+'</defs>':"")+
  '<ellipse class="ec-sh" cx="50" cy="93" rx="20" ry="3"/><g class="ec-bd">'+body+'<g class="ec-fc">'+face+'</g>'+extra+'</g></svg></span>'}

window.EmberChar={styles:STYLES,draw:draw,stateOf:stateOf,seedName:seedName,harness:harness,traits:traits,SHAPES:SHAPES};
})();
