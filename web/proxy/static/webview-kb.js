(function(){
  'use strict';
  const KB_INPUT = 'textarea, input, [contenteditable="true"], .native-edit-context';
  // Tapping these = "I want to type here".
  const KB_SURFACE = '.monaco-editor, .monaco-inputbox, .xterm, ' + KB_INPUT;
  // Only for phone chrome: on a desktop browser an unasked focus is harmless and stealing
  // it would be worse than the problem. Read the workbench's own flag (same origin), and
  // fall back to a touch+small-screen guess if that read is ever blocked.
  function chromeOn(){
    try {
      let w = window;
      for(let i = 0; i < 6; i++){
        const v = w.document && w.document.documentElement.getAttribute('data-xmo-chrome');
        if(v) return v === 'on';
        if(w === w.parent) break;
        w = w.parent;
      }
    } catch(e){}
    try { return matchMedia('(pointer: coarse)').matches
        && Math.min(screen.width, screen.height) <= 820; } catch(e){}
    return false;
  }
  // Same two tools as the workbench policy: HOLD keeps the focus but not the keyboard
  // (inputmode=none), BLUR drops the focus where that is harmless.
  function hold(el){
    if(!el || !el.setAttribute || el.hasAttribute('data-xmo-im')) return;
    el.setAttribute('data-xmo-im', el.getAttribute('inputmode') || '');
    el.setAttribute('inputmode', 'none');
  }
  function release(el){
    if(!el || !el.hasAttribute || !el.hasAttribute('data-xmo-im')) return;
    const prev = el.getAttribute('data-xmo-im');
    el.removeAttribute('data-xmo-im');
    if(prev) el.setAttribute('inputmode', prev); else el.removeAttribute('inputmode');
  }
  function releaseIn(el){
    if(!el) return;
    release(el);
    if(el.querySelectorAll) el.querySelectorAll('[data-xmo-im]').forEach(release);
  }
  // One tap flag per document tree: a tap inside the webview is what the extension's own
  // focus() call follows, so it is the only thing that tells intent apart from automation.
  let tapped = false;
  function install(doc){
    if(!doc || doc.__xmoKb) return;
    try { doc.__xmoKb = true; } catch(e){ return; }
    doc.addEventListener('pointerdown', function(e){
      if(!chromeOn()) return;
      const t = e.target && e.target.closest ? e.target : null;
      const surface = t ? t.closest(KB_SURFACE) : null;
      tapped = !!surface;
      if(surface) releaseIn(surface);          // the user's own tap -> let the keyboard up
      else {
        const a = doc.activeElement;
        if(a && a.closest && a.closest(KB_INPUT)){ try { a.blur(); } catch(e2){} }
      }
    }, true);
    doc.addEventListener('focusin', function(e){
      if(!chromeOn()) return;
      const t = e.target;
      if(!t || !t.closest) return;
      const el = t.closest(KB_INPUT);
      if(!el) return;
      if(tapped){ releaseIn(el); return; }     // the user asked for this focus
      hold(el);                                // unrequested (new chat, panel opened, …)
      try { el.blur(); } catch(err){}
    }, true);
  }
  // The content frame is created after this script runs and is replaced on every reload,
  // so keep looking. Same-origin access is what `allow-same-origin` buys us; a frame we
  // cannot reach (Electron's vscode-webview:// origin) just throws and is skipped.
  function sweep(){
    install(document);
    const fs = document.querySelectorAll('iframe');
    for(let i = 0; i < fs.length; i++){
      let d = null;
      try { d = fs[i].contentDocument; } catch(e){ continue; }
      if(d && d.readyState !== 'uninitialized') install(d);
    }
  }
  sweep();
  try { new MutationObserver(sweep).observe(document.documentElement, {childList:true, subtree:true}); } catch(e){}
  setInterval(sweep, 700);
})();
