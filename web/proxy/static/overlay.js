(function(){
  // Embed mode: VS Code is inside the wrapper's (frame.html) iframe.
  // The overlay stays OFF (native layout fits the iframe exactly); the parent page
  // owns the nav bars and drives us through the window.__xmo API exposed below.
  const XMO_EMBED = /(^|[?&])xmo=embed(&|$)/i.test(location.search) ||
    (function(){ try { return window.self !== window.top; } catch(e){ return true; } })();
  // Whether the PARENT is drawing the mobile chrome. The wrapper is served to every client
  // now, so on a wide desktop window it turns this off and we hand VS Code its native
  // activity bar and bottom status bar back. The initial value rides in on the iframe URL
  // (no flash on load); `setChrome()` takes over from the first resize onward.
  let XMO_CHROME = !/(^|[?&])xmochrome=off(&|$)/i.test(location.search);
  // Orientation comes FROM THE PARENT, never from our own viewport: the wrapper makes this
  // iframe 48px wider (hidden activity-bar gutter) and one bar shorter, so a near-square
  // window reads landscape in here while the device is portrait. Trusting the iframe left
  // the portrait compensation off and shoved the title bar 48px past the screen edge.
  // null = nobody told us yet -> fall back to our own viewport.
  let XMO_PORTRAIT = (function(){
    const m = /(^|[?&])xmoorient=(portrait|landscape)(&|$)/i.exec(location.search);
    return m ? (m[2].toLowerCase() === 'portrait') : null;
  })();
  function xmoPortrait(){
    if(XMO_EMBED && XMO_PORTRAIT !== null) return XMO_PORTRAIT;
    return matchMedia('(orientation: portrait)').matches;
  }
  // ---- Native view containers = VS Code's own modality, read live from the DOM ----
  // Hard-coded lists could never show extension-contributed containers (Claude, ...).
  // Scanning the real composite bar picks up whatever the user has installed.
  // ACTIVITY BAR ONLY, on purpose: the secondary side bar keeps its own visible tab strip
  // (Chat / Claude Code / Codex) above the panel, so mirroring it into our bar was pure
  // duplication — and it made containers that live nowhere in the activity bar look as if
  // they did. Our bar adds home/agent/terminal/settings, which a phone has no other way to
  // reach; it does not invent entries VS Code already shows somewhere else.
  const XMO_VIEW_SOURCES = [
    ['activity', '.monaco-workbench .part.activitybar .composite-bar .actions-container > .action-item']
  ];
  // Skip the chat/agent container (it has its own dedicated bar button) and VS Code's own
  // "Additional Views" overflow chevron — our bar scrolls instead of overflowing.
  const XMO_VIEW_SKIP = /^(chat|copilot|채팅|additional views|추가 보기|more actions)\b/i;
  // Sticky cache: an entry survives while its part is closed — AND across reloads, because a
  // short viewport (landscape phone: activity bar ~217px) hides EVERY container behind the
  // overflow chevron, so a fresh load there would otherwise find nothing to put in the bar.
  // Seeded from localStorage per workspace; activation goes through the overflow menu.
  const xmoViewCache = [];
  function xmoViewCacheKey(){
    // v2: v1 caches still hold secondary-side-bar entries (Codex) we no longer show.
    try { return 'xmo_views2_' + (new URL(location.href).searchParams.get('folder') || ''); } catch(e){ return ''; }
  }
  (function xmoLoadViewCache(){
    try {
      const raw = localStorage.getItem(xmoViewCacheKey());
      if(!raw) return;
      const arr = JSON.parse(raw);
      if(Array.isArray(arr)) arr.forEach(function(v){
        if(v && v.label && v.source === 'activity') xmoViewCache.push(v);
      });
    } catch(e){}
  })();
  let xmoViewCacheSaved = '';
  function xmoSaveViewCache(){
    try {
      const data = JSON.stringify(xmoViewCache.map(function(v){
        return {label: v.label, source: v.source, codicon: v.codicon || '', glyph: v.glyph || '',
                img: v.img || '', mask: v.mask || ''};
      }));
      if(data === xmoViewCacheSaved) return;
      xmoViewCacheSaved = data;
      localStorage.setItem(xmoViewCacheKey(), data);
    } catch(e){}
  }
  function xmoViewLabel(li){
    const a = li.querySelector('a.action-label, .action-label') || li;
    const s = a.getAttribute('aria-label') || li.getAttribute('aria-label') || a.title || '';
    // "Explorer (Ctrl+Shift+E)", "Source Control (Ctrl+Shift+G) - 3 changes" -> "Explorer"
    return String(s).split(/\s*\(/)[0].split(' - ')[0].trim();
  }
  function xmoCssUrl(v){
    const m = v && v !== 'none' && v.match(/url\(["']?([^"')]+)["']?\)/);
    return m ? m[1] : '';
  }
  function xmoViewIcon(li){
    const a = li.querySelector('a.action-label, .action-label') || li;
    let codicon = '', glyph = '', img = '', mask = '';
    for(let i = 0; i < a.classList.length; i++){
      const c = a.classList[i];
      if(c.indexOf('codicon-') === 0 && c !== 'codicon-modifier-spin'){ codicon = c.slice(8); break; }
    }
    // The glyph itself, so the frame parent (which has no codicon CSS) can render it.
    try {
      const cb = getComputedStyle(a, '::before').content;
      const m = cb && cb.match(/^["'](.+)["']$/);
      if(m && m[1] !== 'none') glyph = m[1];
    } catch(e){}
    try {
      const cs = getComputedStyle(a), cb = getComputedStyle(a, '::before');
      // Extension icons come in two flavours: a plain background-image, or (for
      // `uri-icon` containers such as Claude Code) a MASK tinted with currentColor
      // so the icon follows the theme. Both must be carried across to our button.
      img  = xmoCssUrl(cs.backgroundImage) || xmoCssUrl(cb.backgroundImage);
      mask = xmoCssUrl(cs.webkitMaskImage || cs.maskImage) || xmoCssUrl(cb.webkitMaskImage || cb.maskImage);
    } catch(e){}
    return { codicon: codicon, glyph: glyph, img: img, mask: mask };
  }
  function xmoViewActive(li){
    const a = li.querySelector('a.action-label, .action-label') || li;
    return li.classList.contains('checked') || a.getAttribute('aria-expanded') === 'true'
        || a.getAttribute('aria-selected') === 'true' || a.getAttribute('aria-checked') === 'true';
  }
  function xmoScanViews(){
    XMO_VIEW_SOURCES.forEach(function(src){
      document.querySelectorAll(src[1]).forEach(function(li){
        const label = xmoViewLabel(li);
        if(!label || XMO_VIEW_SKIP.test(label)) return;
        let e = null;
        for(let i = 0; i < xmoViewCache.length; i++){ if(xmoViewCache[i].label === label){ e = xmoViewCache[i]; break; } }
        if(!e){ e = { label: label, source: src[0] }; xmoViewCache.push(e); }
        else if(e.source !== src[0]) e.source = src[0];
        const ic = xmoViewIcon(li);
        if(ic.codicon) e.codicon = ic.codicon;
        if(ic.glyph)   e.glyph = ic.glyph;
        if(ic.img)     e.img = ic.img;
        if(ic.mask)    e.mask = ic.mask;
        e.active = xmoViewActive(li);
      });
    });
    xmoSaveViewCache();
    return xmoViewCache;
  }
  function xmoFindViewItem(label){
    let found = null;
    XMO_VIEW_SOURCES.forEach(function(src){
      if(found) return;
      document.querySelectorAll(src[1]).forEach(function(li){
        if(!found && xmoViewLabel(li) === label) found = li;
      });
    });
    return found;
  }
  // VS Code moves activity-bar items it cannot fit into an "Additional Views" (⋯) overflow
  // MENU and removes them from the DOM. On a landscape phone the bar is ~217px tall and
  // *every* container ends up in there (measured: 1 item left, the chevron itself).
  const XMO_PART_OF = { activity: '.monaco-workbench .part.activitybar' };
  function xmoOverflowItem(partSel){
    const sels = [partSel || XMO_PART_OF.activity];
    for(let s = 0; s < sels.length; s++){
      const items = document.querySelectorAll(sels[s] + ' .composite-bar .actions-container > .action-item');
      for(let i = 0; i < items.length; i++){
        const a = items[i].querySelector('a.action-label, .action-label') || items[i];
        const t = (a.getAttribute('aria-label') || '') + ' ' + (a.className || '');
        if(/additional views|추가 보기|codicon-more/i.test(t)) return items[i];
      }
    }
    return null;
  }
  // Containers that do not fit are moved into that overflow MENU and removed from the DOM,
  // so scanning alone can never see them. That is why a freshly installed extension did not
  // show up in our bar. Open the chevron once per load, read the labels, close it. The menu
  // is made invisible while we do it (`data-xmo-scan`), so the user never sees a flash.
  const xmoOverflowScanned = {};
  function xmoScanOverflow(source, done){
    done = done || function(){};
    if(xmoOverflowScanned[source] || !XMO_CHROME){ done(); return; }
    const ch = xmoOverflowItem(XMO_PART_OF[source]);
    if(!ch){ done(); return; }
    xmoOverflowScanned[source] = true;
    document.documentElement.setAttribute('data-xmo-scan', '1');
    xmoFireClick(ch.querySelector('a.action-label, .action-label') || ch);
    setTimeout(function(){
      xmoMenuRows().forEach(function(r){
        const l = r.querySelector('.action-label');
        const label = ((l ? l.textContent : r.textContent) || '').trim();
        if(!label || XMO_VIEW_SKIP.test(label)) return;
        for(let i = 0; i < xmoViewCache.length; i++){ if(xmoViewCache[i].label === label) return; }
        xmoViewCache.push({ label: label, source: source });
      });
      xmoSaveViewCache();
      xmoSendKey('Escape', 27);
      setTimeout(function(){ document.documentElement.removeAttribute('data-xmo-scan'); done(); }, 200);
    }, 500);
  }
  // Driving the overflow menu: VS Code's menus ignore SYNTHETIC mouse events (verified —
  // the menu just stayed open) but do respond to keyboard. So navigate with ArrowDown to
  // the row index and press Enter. `keyCode` is not settable through KeyboardEventInit,
  // hence the defineProperty — VS Code's StandardKeyboardEvent reads exactly that.
  function xmoSendKey(name, keyCode){
    const t = document.activeElement || document.body;
    ['keydown', 'keyup'].forEach(function(type){
      let e;
      try { e = new KeyboardEvent(type, {key: name, code: name, bubbles: true, cancelable: true}); }
      catch(err){ return; }
      try {
        Object.defineProperty(e, 'keyCode', {get: function(){ return keyCode; }});
        Object.defineProperty(e, 'which',   {get: function(){ return keyCode; }});
      } catch(err){}
      t.dispatchEvent(e);
    });
  }
  function xmoMenuRows(root){
    return Array.prototype.filter.call(
      root ? root.querySelectorAll('.action-item')
           : document.querySelectorAll('.context-view .action-item'),
      function(r){ return !r.classList.contains('separator') &&
                          r.getBoundingClientRect().height > 0 &&
                          (r.textContent || '').trim(); });
  }
  // Menus answer to the keyboard, not to synthesized clicks (see xmoSendKey), so every
  // activation is "walk the selection to row N, press Enter". Where the selection already
  // is matters: a menu opened from the keyboard arrives with its first row focused, one
  // opened by a tap with nothing focused. Read it off the DOM instead of assuming.
  function xmoMenuActivateIndex(rows, idx){
    if(idx < 0 || idx >= rows.length) return false;
    let cur = -1;
    for(let i = 0; i < rows.length; i++){ if(rows[i].classList.contains('focused')){ cur = i; break; } }
    let steps = (cur < 0) ? idx + 1 : idx - cur;
    const key = steps < 0 ? ['ArrowUp', 38] : ['ArrowDown', 40];
    steps = Math.abs(steps);
    for(let i = 0; i < steps; i++) xmoSendKey(key[0], key[1]);
    xmoSendKey('Enter', 13);
    return true;
  }
  // Keys go to document.activeElement — which is not the menu when it was opened by a tap.
  function xmoFocusMenu(menu){
    if(!menu) return;
    const a = document.activeElement;
    if(a && menu.contains(a)) return;
    const f = menu.querySelector('.monaco-action-bar') || menu;
    try {
      if(!f.hasAttribute('tabindex')) f.setAttribute('tabindex', '-1');
      f.focus({ preventScroll: true });
    } catch(e){}
  }
  // Row text carries the keybinding too ("Source ControlCtrl+Shift+G"), so match on prefix.
  // The menu can take a moment to render — keep looking for ~3s.
  function xmoClickMenuRow(label, tries){
    tries = tries || 0;
    const want = label.trim().toLowerCase();
    const rows = xmoMenuRows();
    let idx = -1;
    for(let i = 0; i < rows.length; i++){
      const t = (rows[i].textContent || '').trim().toLowerCase();
      if(t.indexOf(want) === 0){ idx = i; break; }
    }
    if(idx >= 0) return xmoMenuActivateIndex(rows, idx);
    if(tries < 20){ setTimeout(function(){ xmoClickMenuRow(label, tries + 1); }, 150); }
    return false;
  }
  function xmoActivateView(label, tries){
    tries = tries || 0;
    const li = xmoFindViewItem(label);
    if(li){ xmoFireClick(li.querySelector('a.action-label, .action-label') || li); return true; }
    if(tries === 0){
      // Hidden behind the activity bar's overflow chevron? Open it and pick the row by name.
      const of = xmoOverflowItem(XMO_PART_OF.activity);
      if(of){
        xmoFireClick(of.querySelector('a.action-label, .action-label') || of);
        xmoClickMenuRow(label);
        return true;
      }
    }
    if(tries < 8) setTimeout(function(){ xmoActivateView(label, tries + 1); }, 160);
    return false;
  }
  // Absolute URL of VS Code's codicon font, so the frame parent page can @font-face it
  // and render the very same glyphs our view items report.
  let xmoCodiconUrl = null;   // stays null until found, so an early call can retry
  function xmoCodiconFontUrl(){
    if(xmoCodiconUrl) return xmoCodiconUrl;
    for(let i = 0; i < document.styleSheets.length; i++){
      const ss = document.styleSheets[i];
      let rules = null;
      try { rules = ss.cssRules; } catch(e){ continue; }   // cross-origin sheet
      if(!rules) continue;
      for(let j = 0; j < rules.length; j++){
        const r = rules[j];
        if(r.type !== 5) continue;                          // CSSRule.FONT_FACE_RULE
        if(!/codicon/i.test(r.style.fontFamily || '')) continue;
        const m = (r.style.src || '').match(/url\(["']?([^"')]+)["']?\)/);
        if(!m) continue;
        try { xmoCodiconUrl = new URL(m[1], ss.href || location.href).href; }
        catch(e){ xmoCodiconUrl = m[1]; }
        return xmoCodiconUrl;
      }
    }
    return '';
  }
  // ---- DarkPyonix custom bottom bar (icon-only) ----
  // Forward a realistic pointer+mouse sequence (activity items ignore a bare .click()).
  function xmoFireClick(el){
    if(!el) return false;
    const r = el.getBoundingClientRect();
    const x = r.left + r.width/2, y = r.top + r.height/2;
    const o = {bubbles:true, cancelable:true, view:window, clientX:x, clientY:y, button:0};
    try {
      el.dispatchEvent(new PointerEvent('pointerdown', {...o, pointerId:1, pointerType:'mouse', isPrimary:true}));
      el.dispatchEvent(new MouseEvent('mousedown', o));
      el.dispatchEvent(new PointerEvent('pointerup', {...o, pointerId:1, pointerType:'mouse', isPrimary:true}));
      el.dispatchEvent(new MouseEvent('mouseup', o));
      el.dispatchEvent(new MouseEvent('click', o));
    } catch(e){ return false; }
    return true;
  }
  function xmoClickCodicon(cc){
    const icon = document.querySelector('.monaco-workbench .part.activitybar .codicon-'+cc);
    if(!icon) return false;
    const item = icon.closest('.action-item') || icon;
    return xmoFireClick(item.querySelector('a.action-label, .action-label') || item);
  }
  function xmoClickAria(sub){
    const el = document.querySelector('[aria-label="'+sub+'"], [aria-label*="'+sub+'"]');
    return el ? xmoFireClick(el) : false;
  }
  // Click a workbench toolbar button by aria-label (exact, then partial). Used to drive
  // native Maximize / Restore of the Panel and Secondary Side Bar so the terminal and
  // chat can go full-screen in portrait (VS Code re-fits their contents on maximize).
  function xmoClickLabel(labels){
    if(typeof labels === 'string') labels = [labels];
    for(let i=0;i<labels.length;i++){
      const b = document.querySelector('[aria-label="'+labels[i]+'"]') || document.querySelector('[aria-label*="'+labels[i]+'"]');
      if(b){ b.click(); return true; }
    }
    return false;
  }
  function xmoRestoreMaximized(){ xmoClickLabel('Restore Panel'); xmoClickLabel('Restore Secondary Side Bar'); }
  // Drag a sash for real, so VS Code re-lays out and the pane CONTENT re-fits (a CSS resize
  // would only move the frame — §5). VS Code's Sash listens for pointer events.
  function xmoSashDrag(sash, toX){
    const r = sash.getBoundingClientRect();
    const y = Math.round(r.top + r.height / 2), x0 = Math.round(r.left + r.width / 2);
    function send(type, x, buttons){
      sash.dispatchEvent(new PointerEvent(type, {bubbles:true, cancelable:true, view:window,
        clientX:x, clientY:y, button:0, buttons:buttons, pointerId:1, pointerType:'mouse', isPrimary:true}));
    }
    send('pointerdown', x0, 1);
    for(let i = 1; i <= 8; i++) send('pointermove', Math.round(x0 + (toX - x0) * i / 8), 1);
    send('pointerup', Math.round(toX), 0);
  }
  // LANDSCAPE: the agent panel shares the screen with the workspace 50/50 instead of taking
  // it all — you can watch the edit land in the editor while you talk to the agent.
  function xmoAuxHalfSoon(tries){
    if(xmoPortrait()) return;
    tries = tries || 0;
    const aux = document.querySelector('.monaco-workbench .part.auxiliarybar');
    const r = aux ? aux.getBoundingClientRect() : null;
    if(!r || r.width === 0){
      if(tries < 24) setTimeout(function(){ xmoAuxHalfSoon(tries + 1); }, 250);
      return;
    }
    const want = Math.round(innerWidth / 2);
    if(Math.abs(r.width - want) <= 24) return;              // already about half
    const sash = Array.prototype.filter.call(document.querySelectorAll('.monaco-sash.vertical'),
      function(sh){
        if(sh.classList.contains('disabled')) return false;
        const sr = sh.getBoundingClientRect();
        return Math.abs((sr.left + sr.width / 2) - r.left) <= 12;
      })[0];
    if(!sash){
      if(tries < 24) setTimeout(function(){ xmoAuxHalfSoon(tries + 1); }, 250);
      return;
    }
    xmoSashDrag(sash, innerWidth - want);
    if(tries < 6) setTimeout(function(){ xmoAuxHalfSoon(tries + 1); }, 500);   // settle
  }
  // Portrait: whatever the user just opened should own the whole screen. Closing the panel
  // and the secondary side bar first lets VS Code lay the primary side bar out full width by
  // itself — it squeezes the editor group down to a couple of pixels without complaint — so
  // the view's CONTENT re-fits too. (A CSS stretch of `.part.sidebar` only moved the frame;
  // the tree inside kept its old width. HANDOFF §5.)
  function xmoSoloPortrait(){
    if(!xmoPortrait()) return;
    if(xmoPartOpen('.monaco-workbench .part.panel')){
      xmoClickLabel('Restore Panel'); xmoClickAria('Toggle Panel');
    }
    if(xmoPartOpen('.monaco-workbench .part.auxiliarybar')){
      xmoClickLabel('Restore Secondary Side Bar');
      xmoClickLabel(['Hide Secondary Side Bar', 'Toggle Secondary Side Bar']);
    }
  }
  function xmoPartOpen(sel){ const e = document.querySelector(sel); return !!e && e.getAttribute('aria-hidden') !== 'true' && e.getBoundingClientRect().width > 0; }
  // Keep clicking the native Maximize button until the given part is open AND maximized
  // (chat/terminal init can be slow, and a layout pass can undo an early click), then stop.
  function xmoMaximizeSoon(partSel, maxLabel, restoreLabel, tries){
    if(!xmoPortrait()) return;
    tries = tries || 0;
    if(tries > 25) return;
    const part = document.querySelector(partSel);
    const open = !!part && part.getAttribute('aria-hidden') !== 'true' && part.getBoundingClientRect().width > 0;
    const maximized = !!(document.querySelector('[aria-label="'+restoreLabel+'"]') || document.querySelector('[aria-label*="'+restoreLabel+'"]'));
    if(open && !maximized){
      const b = document.querySelector('[aria-label="'+maxLabel+'"]') || document.querySelector('[aria-label*="'+maxLabel+'"]');
      if(b) b.click();
    }
    if(!(open && maximized)){ setTimeout(function(){ xmoMaximizeSoon(partSel, maxLabel, restoreLabel, tries + 1); }, 180); }
  }
  // Robust activity-bar action activator. Tries the themable codicon class first,
  // then falls back to aria-label matches (scoped to the activity bar), and RETRIES
  // a few times to cover render-delay races where the icon/global-action is not in
  // the DOM yet. This fixes Git / Manage / Accounts sometimes not opening.
  function xmoActivate(opts){
    if(!opts) return false;
    const tries = opts.tries || 0;
    let el = null;
    if(opts.codicon){
      const icon = document.querySelector('.monaco-workbench .part.activitybar .codicon-'+opts.codicon);
      if(icon) el = icon.closest('.action-item') || icon;
    }
    if(!el && opts.aria){
      for(let i=0;i<opts.aria.length;i++){
        const a = document.querySelector('.monaco-workbench .part.activitybar [aria-label="'+opts.aria[i]+'"], .monaco-workbench .part.activitybar [aria-label*="'+opts.aria[i]+'"]');
        if(a){ el = a.closest('.action-item') || a; break; }
      }
    }
    if(el){ xmoFireClick(el.querySelector('a.action-label, .action-label') || el); return true; }
    if(tries < 6){ opts.tries = tries + 1; setTimeout(function(){ xmoActivate(opts); }, 130); }
    return false;
  }
  // Measure the box-shadow VS Code's own context menus get, by styling a throwaway probe
  // with the same classes. Read from the live stylesheet rather than hard-coded, so it
  // tracks the theme (it is NOT --vscode-widget-shadow; that resolves transparent here).
  let xmoMenuShadowCache = '';
  function xmoMenuShadow(){
    if(xmoMenuShadowCache) return xmoMenuShadowCache;
    if(!document.body) return '';
    try {
      const p = document.createElement('div');
      p.className = 'context-view monaco-component';
      p.style.cssText = 'position:fixed;left:-9999px;top:-9999px;width:1px;height:1px;pointer-events:none;visibility:hidden;';
      document.body.appendChild(p);
      const s = getComputedStyle(p).boxShadow;
      p.remove();
      if(s && s !== 'none') xmoMenuShadowCache = s;
    } catch(e){}
    return xmoMenuShadowCache;
  }
  // Embed mode: the parent bar told us which button opened this menu, so put the menu next
  // to THAT button instead of wherever VS Code anchored it (the hidden activity-bar icon,
  // far away on the left). Nudge with a transform — invisible to VS Code's own layout, so
  // it survives the relayouts that would undo a left/top rewrite (same trick as the lift).
  function xmoPlaceMenuAt(anchor){
    if(!anchor) return;
    const portrait = xmoPortrait();
    const margin = 8;
    document.querySelectorAll('.context-view').forEach(function(node){
      if(node.dataset.xmoPlace) node.style.transform = '';
      const r = node.getBoundingClientRect();
      if(r.width === 0 || r.height === 0) return;
      let wantLeft, wantTop;
      if(portrait){
        wantLeft = anchor.x + anchor.w - r.width;        // right-aligned with the trigger
        wantTop  = anchor.y - r.height - margin;         // and above the bottom bar
      } else {
        wantLeft = anchor.x + anchor.w + margin;         // just right of the left bar
        wantTop  = anchor.y + anchor.h / 2 - r.height / 2;
      }
      wantLeft = Math.max(margin, Math.min(wantLeft, innerWidth - r.width - margin));
      wantTop  = Math.max(margin, Math.min(wantTop, innerHeight - r.height - margin));
      const dx = Math.round(wantLeft - r.left), dy = Math.round(wantTop - r.top);
      if(dx || dy){
        node.style.transform = 'translate(' + dx + 'px,' + dy + 'px)';
        node.dataset.xmoPlace = '1';
      } else if(node.dataset.xmoPlace){
        node.style.transform = ''; delete node.dataset.xmoPlace;
      }
    });
  }
  function xmoPlaceMenuSoon(anchor){
    [20, 90, 180, 320, 480].forEach(function(t){ setTimeout(function(){ xmoPlaceMenuAt(anchor); }, t); });
  }
  // Home = workspace switcher: a full overlay listing recent workspaces.
  // Portrait: the side bar owns the whole screen and the editor slot is collapsed to 0, so
  // anything that OPENS AN EDITOR from the side bar (a file, a Claude Code session, a diff)
  // would appear behind it and read as "nothing happened". Close the side bar the moment an
  // editor opens — the standard phone pattern: list → tap → full-screen content.
  // The editor DOM is intact while collapsed (only its width is 0), so the tab label is
  // still readable and usable as the change signal.
  let xmoLastEditorKey = null;
  function xmoEditorKey(){
    const g = document.querySelector('.monaco-workbench .part.editor .editor-group-container.active')
           || document.querySelector('.monaco-workbench .part.editor .editor-group-container');
    if(!g) return '';
    const t = g.querySelector('.tabs-container .tab.active');
    if(t) return (t.getAttribute('aria-label') || t.textContent || '').trim();
    const l = g.querySelector('.title .label-name, .title .title-label');
    return l ? (l.textContent || '').trim() : '';
  }
  function xmoAutoHideSidebarOnEditor(){
    if(!XMO_EMBED || !XMO_CHROME || !xmoPortrait()){ xmoLastEditorKey = null; return; }
    const key = xmoEditorKey();
    if(xmoLastEditorKey === null){ xmoLastEditorKey = key; return; }   // first tick = baseline
    const changed = key && key !== xmoLastEditorKey;
    xmoLastEditorKey = key;
    if(changed){
      // Give the editor the whole screen: panel + secondary side bar out of the way, and the
      // primary side bar closed. Without the solo step an editor opened by an extension
      // action ("Claude Code open", "Codex sidebar") lands in a ~190px sliver next to the
      // still-open chat.
      xmoSoloPortrait();
      if(xmoPartOpen('.monaco-workbench .part.sidebar')){
        xmoClickLabel(['Toggle Primary Side Bar', '기본 사이드 바']);
      }
    }
  }
  function xmoCurrentFolder(){
    try { return new URL(location.href).searchParams.get('folder'); } catch(e){ return null; }
  }
  // ---- Recent agent chat capture (feeds the Home launcher cards) ----
  // Keyed by the workspace folder so the Home page (same origin) can read it via
  // localStorage['xmo_chats_' + folder]. Captured on Enter from the chat input editor.
  function xmoSaveChat(text){
    const folder = xmoCurrentFolder(); if(!folder || !text) return;
    text = String(text).replace(/\s+/g, ' ').trim().slice(0, 200); if(!text) return;
    const key = 'xmo_chats_' + folder;
    let arr = [];
    try { arr = JSON.parse(localStorage.getItem(key)) || []; } catch(e){ arr = []; }
    if(arr.length && arr[0] && arr[0].text === text) return;
    arr.unshift({ ts: Date.now(), text: text });
    arr = arr.slice(0, 8);
    try { localStorage.setItem(key, JSON.stringify(arr)); } catch(e){}
  }
  function xmoReadChatInputText(){
    const a = document.activeElement;
    let editor = (a && a.closest) ? a.closest('.interactive-input-editor') : null;
    if(!editor) editor = document.querySelector('.monaco-workbench .interactive-input-editor');
    if(!editor) return '';
    let text = '';
    editor.querySelectorAll('.view-line').forEach(function(l){ text += (l.textContent || '') + ' '; });
    return text;
  }
  let xmoChatCaptureReady = false;
  // Android: tapping the title bar's command centre ("Open Quick Access") did nothing.
  // Measured — a real mouse click opens the quick input, a pure touch sequence does not:
  // VS Code's own gesture handling swallows touches on elements it has not registered, so
  // the browser never synthesizes the follow-up click. Bridge it: if a touch on those
  // controls is NOT followed by a click, dispatch one ourselves.
  let xmoTapPending = null;
  // Context menu rows are a harder case than the title bar: a tap does nothing at all
  // (reported on the notebook cell menu — "Cut Cell … Toggle Cell Toolbar Position"), and
  // the click bridge below cannot help, because these menus ignore synthesized mouse events
  // by design. Drive the selection with the keyboard instead, exactly like xmoClickMenuRow.
  // A terminal opened at another size comes back mis-measured. VS Code lays the panel out
  // on window resize, and our bar/maximize dance changes the panel size without one, so
  // nudge it — xterm re-measures the cell grid and reflows on that pass.
  function xmoRelayoutSoon(){
    [120, 400, 900, 1600].forEach(function(t){
      setTimeout(function(){
        try { window.dispatchEvent(new Event('resize')); } catch(e){}
      }, t);
    });
  }
  function xmoInitMenuTapBridge(){
    document.addEventListener('touchend', function(e){
      if(!XMO_CHROME) return;
      if(!e.target || !e.target.closest) return;
      const row = e.target.closest('.context-view .action-item');
      if(!row || row.classList.contains('separator') || row.classList.contains('disabled')) return;
      const menu = row.closest('.monaco-menu') || row.closest('.context-view');
      const rows = xmoMenuRows(menu);
      const idx = rows.indexOf(row);
      if(idx < 0) return;
      // Stop the browser's follow-up click: it lands outside anything the menu listens to
      // and just dismisses it — which would race the keys we are about to send.
      e.preventDefault();
      xmoFocusMenu(menu);
      xmoMenuActivateIndex(rows, idx);
    }, true);
  }
  function xmoInitTouchClickBridge(){
    document.addEventListener('touchend', function(e){
      if(!XMO_CHROME) return;
      if(!e.target || !e.target.closest) return;
      // closest() only decides WHETHER to bridge. The click itself must be dispatched on
      // the deepest touched element: the command centre ignores a sequence aimed at the
      // container (verified — container = nothing happens, e.target = quick input opens).
      const scope = e.target.closest('.part.titlebar .command-center')
                 || e.target.closest('.part.titlebar .action-item');
      if(!scope) return;
      const t = e.target;
      if(xmoTapPending) clearTimeout(xmoTapPending);
      xmoTapPending = setTimeout(function(){ xmoTapPending = null; xmoFireClick(t); }, 350);
    }, true);
    document.addEventListener('click', function(){
      if(xmoTapPending){ clearTimeout(xmoTapPending); xmoTapPending = null; }  // browser did it
    }, true);
  }
  // Phone keyboard policy: the keyboard must appear only AFTER the user taps a text
  // surface — never on its own. VS Code focuses the editor's input surface the moment a
  // file opens, which popped the soft keyboard unasked and then would not go away.
  // Two tools, because one does not fit every widget: BLUR where dropping the focus is
  // harmless (editor, terminal, search box, chat), and a HOLD (inputmode=none) where the
  // focus must survive or the widget closes with it (quick access / command palette).
  // ⚠️ VS Code 1.132 uses the EditContext API: that surface is a `div.native-edit-context`,
  // NOT a textarea (measured: activeElement on load = DIV.native-edit-context). Blurring
  // only textarea/input therefore did nothing.
  const XMO_KB_INPUT = 'textarea, input, [contenteditable="true"], .native-edit-context';
  // Tapping any of these counts as "the user asked for the keyboard" — text surfaces plus
  // the terminal body (xterm's own input is a hidden textarea behind a canvas).
  // The quick input is deliberately NOT in this list: it opens from the command centre, and
  // its LIST is for reading. Only its input box (.monaco-inputbox, already here) means
  // "I want to type" — a tap on a result row must not drag the keyboard back up.
  const XMO_KB_SURFACE = '.monaco-editor, .interactive-input-part,' +
    ' .monaco-inputbox, .suggest-widget, .xterm, .terminal, .terminal-wrapper, ' + XMO_KB_INPUT;
  // Surfaces we must never blur: VS Code hides the quick input the moment its input loses
  // focus, so the old blur-everything policy would have closed the palette the user just
  // opened — which is why quick access used to be exempted and popped the keyboard.
  const XMO_KB_HOLD = '.quick-input-widget';
  let xmoTappedInput = false;
  function xmoIsKbInput(el){ return !!(el && el.closest && el.closest(XMO_KB_INPUT)); }
  // Hold = keep the focus, keep the keyboard DOWN. `inputmode="none"` is the browser's own
  // way to say "this field has its own input method": focus, caret and the widget all stay,
  // the soft keyboard does not come up. It is set before the browser decides to raise the
  // keyboard (focusin fires first), and released by the user's own tap on the field.
  function xmoKbHold(el){
    if(!el || !el.setAttribute || el.hasAttribute('data-xmo-im')) return;
    el.setAttribute('data-xmo-im', el.getAttribute('inputmode') || '');
    el.setAttribute('inputmode', 'none');
  }
  function xmoKbRelease(el){
    if(!el || !el.hasAttribute || !el.hasAttribute('data-xmo-im')) return;
    const prev = el.getAttribute('data-xmo-im');
    el.removeAttribute('data-xmo-im');
    if(prev) el.setAttribute('inputmode', prev); else el.removeAttribute('inputmode');
  }
  // The tap often lands on a wrapper (.view-line, the input box border, the xterm canvas),
  // never on the field itself — so release everything held inside the tapped surface.
  function xmoKbReleaseIn(el){
    if(!el) return;
    xmoKbRelease(el);
    if(el.querySelectorAll) el.querySelectorAll('[data-xmo-im]').forEach(xmoKbRelease);
  }
  function xmoBlurInput(){
    const a = document.activeElement;
    if(!xmoIsKbInput(a)) return;
    // Blurring a quick input closes it. Hold the keyboard down instead and keep the widget.
    if(a.closest && a.closest(XMO_KB_HOLD)){ xmoKbHold(a.closest(XMO_KB_INPUT) || a); return; }
    try { a.blur(); } catch(e){}
  }
  // Taps on the PARENT's bar reach us only through this call, so it carries the same
  // click-away meaning as a tap anywhere else outside the palette.
  function xmoDismissKeyboard(){ xmoTappedInput = false; xmoBlurInput(); xmoCloseQuickInput(); }
  // Keyboard policy and DISMISSAL policy are different questions for the quick input.
  // The policy above never blurs it, because a blur is exactly what VS Code closes it on
  // and its list must survive a tap. But a tap on the workbench OUTSIDE the widget is a
  // click-away and must close it — on a desktop the click moves focus and does this for
  // free; on a phone the tap lands on nothing focusable, so the palette used to just sit
  // there. Blur it ourselves, and fall back to its own dismiss key if it holds on.
  function xmoQuickInputOpen(){
    const w = document.querySelector('.quick-input-widget');
    if(!w) return null;
    if(w.style.display === 'none') return null;
    return w.getBoundingClientRect().height > 0 ? w : null;
  }
  function xmoCloseQuickInput(){
    if(!xmoQuickInputOpen()) return;
    const a = document.activeElement;
    if(a && a.blur){ try { a.blur(); } catch(e){} }
    setTimeout(function(){ if(xmoQuickInputOpen()) xmoSendKey('Escape', 27); }, 150);
  }
  function xmoInitKeyboardPolicy(){
    document.addEventListener('pointerdown', function(e){
      if(!XMO_CHROME) return;
      const t = e.target && e.target.closest ? e.target : null;
      const surface = t ? t.closest(XMO_KB_SURFACE) : null;
      xmoTappedInput = !!surface;
      // The user's own tap on a text surface IS the request for the keyboard: lift the hold
      // here, in the capture phase, so it is gone before the browser reacts to the tap.
      if(surface) xmoKbReleaseIn(surface);
      else if(t && t.closest(XMO_KB_HOLD)){
        // Inside the quick input but not on its box — a list row. Leave focus (and the
        // widget) alone; the keyboard stays down because the hold is still on.
      } else {
        xmoBlurInput();                        // tapped elsewhere -> put the keyboard away
        xmoCloseQuickInput();                  // …and a tap outside dismisses the palette
      }
    }, true);
    document.addEventListener('focusin', function(e){
      if(!XMO_CHROME) return;
      const t = e.target;
      if(!t || !t.closest) return;
      const el = t.closest(XMO_KB_INPUT);
      if(!el) return;                          // not something that raises the keyboard
      if(xmoTappedInput){ xmoKbReleaseIn(el); return; }   // the user asked for this focus
      xmoKbHold(el);                           // unrequested -> the keyboard stays down
      if(el.closest(XMO_KB_HOLD)) return;      // …and the widget stays open (no blur)
      try { el.blur(); } catch(err){}
    }, true);
  }
  function xmoInitChatCapture(){
    if(xmoChatCaptureReady) return; xmoChatCaptureReady = true;
    document.addEventListener('keydown', function(e){
      if(e.key !== 'Enter' || e.shiftKey || e.altKey || e.isComposing) return;
      const a = document.activeElement;
      if(!(a && a.closest && a.closest('.interactive-input-part'))) return;
      const t = xmoReadChatInputText();
      if(t) xmoSaveChat(t);
    }, true);
    // Also capture when the Send button is tapped (mobile users often tap instead of Enter).
    document.addEventListener('click', function(e){
      const el = e.target;
      const btn = (el && el.closest) ? el.closest('.interactive-input-part [aria-label^="Send"], .interactive-input-part [aria-label*="Send "]') : null;
      if(!btn) return;
      const t = xmoReadChatInputText();
      if(t) xmoSaveChat(t);
    }, true);
  }
  function xmoSidebarShown(){
    const t = document.querySelector('[aria-label*="Toggle Primary Side Bar"]');
    return !!(t && t.getAttribute('aria-pressed') === 'true');
  }
  function xmoSidebarViewName(){
    const el = document.querySelector('.monaco-workbench .part.sidebar .composite.title .title-label, .monaco-workbench .part.sidebar .title-label');
    return el ? (el.textContent || '').trim() : '';
  }
  function xmoIsExplorerName(){ return /explorer|탐색기/i.test(xmoSidebarViewName()); }
  function xmoIsFilesActive(){ return xmoSidebarShown() && xmoIsExplorerName(); }
  // Frame/embed mode: move VS Code's status bar to the TOPMOST row (above the title bar,
  // not at the bottom). Shift the whole workbench down by the bar height so VS Code re-lays its parts
  // into the shorter area, keep the title bar pinned to the very top, and pin the status
  // bar into the gap. Writes are no-ops once everything already matches, so this is safe
  // to run every frame — which is what keeps the bar from ever being left at VS Code's
  // relaid-out (possibly off-screen) position when the viewport size changes.
  let xmoStatusBusy = false;
  let xmoLastTitleH = 0;
  let xmoStatusH = 0;   // measured ONCE and cached — a fluctuating value made the title flicker
  // Undo everything xmoEmbedStatusTop() pinned, so VS Code lays the title/status bars out
  // natively again. Needed the moment the parent turns its chrome off (window widened).
  function xmoEmbedChromeRestore(){
    const wb = document.querySelector('.monaco-workbench');
    if(wb){ ['position','top','height'].forEach(function(p){ wb.style.removeProperty(p); }); }
    ['.monaco-workbench .part.titlebar', '.monaco-workbench .part.statusbar'].forEach(function(sel){
      const e = document.querySelector(sel);
      if(!e) return;
      ['position','top','bottom','left','right','width','height','z-index','transform']
        .forEach(function(p){ e.style.removeProperty(p); });
    });
    xmoStatusH = 0; xmoLastTitleH = 0; xmoStatusBusy = false;
    try { window.dispatchEvent(new Event('resize')); } catch(e){}
  }
  function xmoEmbedStatusTop(){
    if(!XMO_EMBED || !XMO_CHROME) return;
    const sb = document.querySelector('.monaco-workbench .part.statusbar');
    if(!sb) return;
    // Cache the status-bar height once. Re-measuring every frame let it wobble (0 during a
    // relayout), which flipped the title-shift condition and left the title bar covered.
    if(!xmoStatusH){
      const m = Math.round(sb.getBoundingClientRect().height);
      xmoStatusH = (m >= 16 && m <= 40) ? m : 22;
    }
    const H = xmoStatusH;
    const want = H + 'px';
    const wb = document.querySelector('.monaco-workbench');
    // 1) shift the whole workbench down by H so VS Code re-lays its parts into the shorter
    //    area (one resize nudge makes it re-measure). Stable H -> applied once, no thrash.
    if(wb && !xmoStatusBusy && (wb.style.position !== 'relative' || wb.style.top !== want)){
      wb.style.setProperty('position','relative','important');
      wb.style.setProperty('top', want,'important');
      wb.style.setProperty('height','calc(100% - ' + want + ')','important');
      xmoStatusBusy = true;
      try { window.dispatchEvent(new Event('resize')); } catch(e){}
      setTimeout(function(){ xmoStatusBusy = false; }, 400);
    }
    // 2) Align each bar's leftmost CONTENT to screen x=0. The frame slides the whole iframe
    //    left (portrait) to hide the native activity-bar column, so shift the fixed bars right
    //    by that amount — BUT VS Code also offsets the items INSIDE each bar (reserving the
    //    activity-bar column), so subtract that measured internal offset. Over-shifting leaves
    //    the left looking empty; under-shifting clips the first item. Measuring both removes
    //    the guesswork (title bar and status bar can have different internal offsets).
    let frameLeft = 0;
    try { const fe = window.frameElement; if(fe){ const fr = fe.getBoundingClientRect(); if(fr.left < 0) frameLeft = Math.round(-fr.left); } } catch(e){}
    function xmoItemOff(bar, sel){
      if(!bar) return 0;
      const it = bar.querySelector(sel);
      if(!it) return 0;
      return Math.max(0, Math.round(it.getBoundingClientRect().left - bar.getBoundingClientRect().left));
    }
    const sOff = xmoItemOff(sb, '.statusbar-item');
    const statusLeft = Math.max(0, frameLeft - sOff);
    // 3) TITLE BAR pinned fixed directly BELOW the status bar. position:fixed escapes BOTH the
    //    workbench and the grid-view overflow:hidden that clipped the translated title bar down
    //    to a sliver. (Row order is status-on-top, title under it — see step 4.)
    const tb = document.querySelector('.monaco-workbench .part.titlebar');
    let titleH = xmoLastTitleH || 35;
    const tOff = xmoItemOff(tb, '.window-appicon, .menubar, .action-item, .codicon');
    const titleLeftPx = Math.max(0, frameLeft - tOff) + 'px';
    if(tb){
      if(tb.style.position !== 'fixed'){ const th = Math.round(tb.getBoundingClientRect().height); if(th >= 20){ titleH = th; xmoLastTitleH = th; } }
      else if(xmoLastTitleH) titleH = xmoLastTitleH;
      const tPx = titleH + 'px';
      // Check EVERY property we set, not just a few: VS Code writes `style.width` inline on
      // its own layout pass, and per CSSOM that drops our `!important` — the title bar then
      // kept the full iframe width (48px wider than the screen) and hung off the right edge.
      if(tb.style.position !== 'fixed' || tb.style.top !== want || tb.style.height !== tPx ||
         tb.style.left !== titleLeftPx || tb.style.right !== '0px' || tb.style.width !== 'auto'){
        tb.style.setProperty('position','fixed','important');
        tb.style.setProperty('top', want,'important');
        tb.style.setProperty('bottom','auto','important');
        tb.style.setProperty('left', titleLeftPx,'important');
        tb.style.setProperty('right','0px','important');
        tb.style.setProperty('width','auto','important');
        tb.style.setProperty('height', tPx,'important');
        tb.style.setProperty('z-index','26','important');
        tb.style.removeProperty('transform');
      }
    }
    // 4) STATUS BAR pinned fixed at the VERY TOP row (above the title bar) — the connection
    //    state is what matters most on a phone, so it gets the topmost strip. Write only on a
    //    real difference so running every frame is free while still correcting any relayout.
    // Publish the measured geometry so CSS can use it — the portrait full-screen side bar
    // has to start below the status+title band and right of the hidden gutter.
    const rs = document.documentElement.style;
    const bandPx = (H + titleH) + 'px', leftPx = frameLeft + 'px';
    if(rs.getPropertyValue('--xmo-top-band') !== bandPx) rs.setProperty('--xmo-top-band', bandPx);
    if(rs.getPropertyValue('--xmo-frame-left') !== leftPx) rs.setProperty('--xmo-frame-left', leftPx);
    const ab = document.querySelector('.monaco-workbench .part.activitybar');
    const abw = ab ? Math.round(ab.getBoundingClientRect().width) : 48;
    const abPx = (abw > 0 && abw < 120 ? abw : 48) + 'px';
    if(rs.getPropertyValue('--xmo-ab-w') !== abPx) rs.setProperty('--xmo-ab-w', abPx);
    const hPx = H + 'px', sLeftPx = statusLeft + 'px';
    if(sb.style.position !== 'fixed' || sb.style.top !== '0px' || sb.style.left !== sLeftPx ||
       sb.style.right !== '0px' || sb.style.height !== hPx || sb.style.zIndex !== '30' ||
       sb.style.bottom !== 'auto' || sb.style.width !== 'auto'){
      sb.style.setProperty('position','fixed','important');
      sb.style.setProperty('top','0px','important');
      sb.style.setProperty('bottom','auto','important');
      sb.style.setProperty('left', sLeftPx,'important');
      sb.style.setProperty('right','0px','important');
      sb.style.setProperty('width','auto','important');
      sb.style.setProperty('height', hPx,'important');
      sb.style.setProperty('z-index','30','important');
    }
  }
  // The workbench always loads inside the wrapper's iframe (embed). The old overlay mode,
  // which drew the bar inside the document, was removed: it left an undeleteable 48px
  // activity bar column behind.
  function apply(){
    const root=document.documentElement;
    root.setAttribute('data-xmo-embed','1');
    root.setAttribute('data-xmo-chrome', XMO_CHROME ? 'on' : 'off');
    root.setAttribute('data-xmo-orient', xmoPortrait() ? 'portrait' : 'landscape');
    xmoEmbedStatusTop();
    syncThemeColor();
  }
  // Dark theme -> light top bar, light theme -> dark top bar (per DarkPyonix spec)
  function syncThemeColor(){
    const b = document.body; if(!b) return;
    const isDark = b.classList.contains('vs-dark') || b.classList.contains('hc-black');
    const color = isDark ? '#ffffff' : '#000000';
    let m = document.querySelector('meta[name="theme-color"]');
    if(!m){ m = document.createElement('meta'); m.setAttribute('name','theme-color'); (document.head||document.documentElement).appendChild(m); }
    if(m.getAttribute('content') !== color) m.setAttribute('content', color);
  }
  apply();
  xmoInitChatCapture();
  xmoInitTouchClickBridge(); xmoInitMenuTapBridge(); xmoInitKeyboardPolicy();
  if(XMO_EMBED){
    // The high-level API the wrapper (parent page) uses. Every parent bar button goes through it.
    window.__xmo = {
      embed: true,
      // Parent tells us whether it is drawing the mobile chrome. Off => give VS Code its
      // native activity bar and bottom status bar back (wide desktop window).
      setChrome: function(on, portrait){
        if(typeof portrait === 'boolean' && portrait !== XMO_PORTRAIT){
          XMO_PORTRAIT = portrait;
          document.documentElement.setAttribute('data-xmo-orient', portrait ? 'portrait' : 'landscape');
          xmoEmbedStatusTop();   // the portrait padding just changed the measured offsets
        }
        on = !!on;
        if(on === XMO_CHROME) return;
        XMO_CHROME = on;
        document.documentElement.setAttribute('data-xmo-chrome', on ? 'on' : 'off');
        if(on) xmoEmbedStatusTop(); else xmoEmbedChromeRestore();
      },
      filesTap: function(){
        xmoRestoreMaximized();
        if(xmoIsFilesActive()){ var t = document.querySelector('[aria-label*="Toggle Primary Side Bar"]'); if(t) t.click(); }
        else { xmoSoloPortrait(); xmoClickCodicon('explorer-view-icon'); }
      },
      terminal: function(){
        if(xmoPartOpen('.monaco-workbench .part.auxiliarybar')){ xmoClickLabel('Restore Secondary Side Bar'); xmoClickLabel('Hide Secondary Side Bar'); }
        var open = xmoPartOpen('.monaco-workbench .part.panel');
        xmoClickAria('Toggle Panel');
        if(!open){ xmoMaximizeSoon('.monaco-workbench .part.panel', 'Maximize Panel', 'Restore Panel'); }
        xmoRelayoutSoon();
      },
      agent: function(){
        if(xmoPartOpen('.monaco-workbench .part.panel')){ xmoClickLabel('Restore Panel'); xmoClickAria('Toggle Panel'); }
        var open = xmoPartOpen('.monaco-workbench .part.auxiliarybar');
        if(open){ if(!xmoClickLabel('Hide Secondary Side Bar')){ if(!xmoClickAria('Toggle Chat')) xmoClickAria('Chat'); } }
        else { if(!xmoClickAria('Toggle Chat')) xmoClickAria('Chat'); xmoMaximizeSoon('.monaco-workbench .part.auxiliarybar', 'Maximize Secondary Side Bar', 'Restore Secondary Side Bar'); }
        xmoAuxHalfSoon();          // landscape: 50/50 with the workspace (no-op in portrait)
      },
      view: function(codicon, aria){ xmoRestoreMaximized(); xmoActivate({codicon: codicon, aria: aria}); },
      // `anchor` = the parent button's rect in iframe coordinates (optional).
      menu: function(codicon, aria, anchor){ xmoActivate({codicon: codicon, aria: aria}); xmoPlaceMenuSoon(anchor); },
      // Every view container VS Code's ACTIVITY BAR currently offers — explorer/search/git/
      // ... PLUS whatever extensions register there (Claude, ...). The parent renders one bar
      // button per entry, so a newly installed extension shows up without any code change.
      // Secondary-side-bar-only containers are deliberately absent: VS Code already shows
      // their own tab strip when that part is open.
      views: function(){
        return xmoScanViews().map(function(v){
          return { label: v.label, codicon: v.codicon || '', glyph: v.glyph || '',
                   img: v.img || '', mask: v.mask || '', active: !!v.active };
        });
      },
      activateView: function(label){
        xmoRestoreMaximized();
        // Always a primary side bar container now -> clear the other parts so VS Code
        // gives it the full width.
        xmoSoloPortrait();
        xmoActivateView(label);
      },
      // The parent page has no codicon CSS; hand it the font so it can draw the glyphs
      // reported by views() verbatim.
      codiconFont: xmoCodiconFontUrl,
      // Taps on the PARENT's bar cannot reach the iframe's focus by themselves.
      dismissKeyboard: xmoDismissKeyboard,
      kbState: function(){ return { tapped: xmoTappedInput }; },
      isFilesActive: xmoIsFilesActive,
      ready: function(){ return !!document.querySelector('.monaco-workbench .part.activitybar'); },
      // Width of the hidden native activity bar column (leftmost). The parent
      // shifts the iframe left by this much so the dead gutter sits off-screen.
      gutter: function(){
        var ab = document.querySelector('.monaco-workbench .part.activitybar');
        if(!ab) return 0;
        var r = ab.getBoundingClientRect();
        return (r.width > 0 && r.width < 120 && r.left <= 1) ? Math.round(r.width) : 0;
      },
      // Real title/status row heights so the parent can align its landscape bar
      // exactly between them (vscode.dev-style column).
      metrics: function(){
        var t = document.querySelector('.monaco-workbench .part.titlebar');
        var s = document.querySelector('.monaco-workbench .part.statusbar');
        var tr = t ? t.getBoundingClientRect() : null;
        var sr = s ? s.getBoundingClientRect() : null;
        var titleBottom = tr && tr.height ? tr.bottom : 0;
        // Status bar is relocated to the top band (below the title bar): fold it into
        // titleH and report 0 bottom reservation so the frame's side bar spans full height.
        var statusAtTop = sr && sr.height && sr.top < innerHeight / 2;
        if(statusAtTop){
          return { titleH: Math.round(Math.max(titleBottom, sr.bottom)) || 0, statusH: 0 };
        }
        return {
          titleH: Math.round(titleBottom) || 0,
          statusH: sr && sr.height ? Math.max(0, Math.round(innerHeight - sr.top)) : 0
        };
      },
      state: function(){
        return {
          files: xmoIsFilesActive(),
          terminal: xmoPartOpen('.monaco-workbench .part.panel'),
          agent: xmoPartOpen('.monaco-workbench .part.auxiliarybar')
        };
      },
      theme: function(){
        var w = document.querySelector('.monaco-workbench'); if(!w) return null;
        var cs = getComputedStyle(w);
        var b = document.body;
        return {
          bg: cs.getPropertyValue('--vscode-activityBar-background').trim(),
          fg: cs.getPropertyValue('--vscode-activityBar-inactiveForeground').trim(),
          fgActive: cs.getPropertyValue('--vscode-activityBar-foreground').trim(),
          border: cs.getPropertyValue('--vscode-panel-border').trim(),
          // The exact shadow VS Code puts on its context menus, so the parent's popup can
          // wear the same one — tapping a row in that popup opens such a menu right on top
          // of it, and two different depths in one gesture read as a rendering glitch.
          menuShadow: xmoMenuShadow(),
          dark: !!b && (b.classList.contains('vs-dark') || b.classList.contains('hc-black'))
        };
      }
    };
  }
  window.addEventListener('resize', apply, { passive:true });
  window.addEventListener('orientationchange', apply, { passive:true });
  try { matchMedia('(orientation: portrait)').addEventListener('change', apply); } catch(e){}
  // Re-sync top bar color whenever VS Code swaps the theme class on <body>
  if(document.body){
    const themeMo = new MutationObserver(syncThemeColor);
    themeMo.observe(document.body,{attributes:true,attributeFilter:['class']});
  }
  if(XMO_EMBED){
    // Keep the top status bar correct across VS Code relayouts and viewport-size changes.
    // A MutationObserver re-pins on the very next frame after any workbench DOM change
    // (debounced via rAF; the pin itself no-ops when already correct), an interval is the
    // fallback, and a burst catches VS Code's async multi-pass relayout after resize.
    var xmoPinQueued = false;
    function xmoSchedulePin(){
      if(xmoPinQueued) return; xmoPinQueued = true;
      requestAnimationFrame(function(){ xmoPinQueued = false; xmoEmbedStatusTop(); });
    }
    var xmoWb = document.querySelector('.monaco-workbench') || document.body;
    if(xmoWb){ try { new MutationObserver(xmoSchedulePin).observe(xmoWb, { childList:true, subtree:true }); } catch(e){} }
    setInterval(xmoEmbedStatusTop, 250);
    setInterval(xmoAutoHideSidebarOnEditor, 400);
    function xmoDiscover(){ xmoScanOverflow('activity'); }
    [3000, 12000].forEach(function(d){ setTimeout(xmoDiscover, d); });
    ['resize','orientationchange'].forEach(function(ev){
      window.addEventListener(ev, function(){
        [0, 100, 300, 600, 1000].forEach(function(d){ setTimeout(xmoEmbedStatusTop, d); });
      }, { passive:true });
    });
    // Rotating changes the cell grid under an open terminal; VS Code relayouts, but the
    // revived-terminal case needs the extra passes (and the input tuning on new terminals).
    window.addEventListener('orientationchange', function(){ xmoRelayoutSoon(); }, { passive:true });
  }
})();
