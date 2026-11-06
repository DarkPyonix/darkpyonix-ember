(function () {
  "use strict";
  // Ember guest page (SPEC FR-T14). Served inline in /join; the server hashes this exact text
  // into the page's script-src. Rules: DOM text only through textContent; no external request;
  // the invite token lives in memory until accept and never in a URL, header or storage.
  // The credential is kept in sessionStorage ONLY: it ends with the tab. The persistent kind of
  // browser storage is deliberately not used, so a shared or borrowed browser keeps nothing after the tab closes.

  var STORE_KEY = "ember.guest";
  var MAX_BYTES = 8192;
  var POLL_MS = 3000;
  var memorySession = null;

  var S = { cred: null, channelId: null, guestId: null, after: 0, timer: null, ended: false, busy: false };

  function $(id) { return document.getElementById(id); }

  // ---- pure helpers -------------------------------------------------------------------------

  // The invite token of a location.hash value, or null.
  function tokenFromHash(hash) {
    var h = String(hash || "");
    if (h.charAt(0) === "#") { h = h.slice(1); }
    return h.indexOf("dpi_") === 0 && h.length > 4 ? h : null;
  }

  function byteLength(s) { return new TextEncoder().encode(s).length; }

  function validName(s) {
    var n = String(s).trim();
    return n.length >= 1 && n.length <= 64;
  }

  // Seconds to wait after a 429, from Retry-After (clamped to 1..120), else the poll step.
  function backoffMs(retryAfter) {
    var n = parseInt(retryAfter, 10);
    if (!(n > 0)) { return POLL_MS * 2; }
    return Math.min(Math.max(n, 1), 120) * 1000;
  }

  function errorText(status, body) {
    var code = body && body.code;
    if (status === 429 && code === "quota_exceeded") { return "이 채널에서 보낼 수 있는 메시지 수를 모두 사용했습니다."; }
    if (status === 429) { return "요청이 너무 많습니다. 잠시 후 다시 시도하세요."; }
    if (status === 400 && code === "not_a_member") { return "이 채널에 없는 멘션이 있습니다."; }
    if (body && typeof body.error === "string" && status === 400) { return body.error; }
    return "요청을 처리하지 못했습니다.";
  }

  // ---- session storage ----------------------------------------------------------------------

  function saveSession(s) {
    memorySession = s;
    try { window.sessionStorage.setItem(STORE_KEY, JSON.stringify(s)); } catch (e) { /* unavailable: memory only */ }
  }

  function loadSession() {
    try {
      var raw = window.sessionStorage.getItem(STORE_KEY);
      if (raw) {
        var s = JSON.parse(raw);
        if (s && typeof s.credential === "string" && typeof s.channel_id === "string") { return s; }
      }
    } catch (e) { /* unavailable or damaged */ }
    return memorySession;
  }

  function clearSession() {
    memorySession = null;
    try { window.sessionStorage.removeItem(STORE_KEY); } catch (e) { /* ignore */ }
  }

  // ---- views --------------------------------------------------------------------------------

  function show(name) {
    ["ended", "nameview", "chatview"].forEach(function (id) {
      $(id).classList.toggle("hidden", id !== name);
    });
  }

  function showEnded(text) {
    stopPolling();
    $("endedtext").textContent = text;
    show("ended");
  }

  function endInvitation() {
    S.ended = true;
    S.cred = null;
    clearSession();
    showEnded("초대가 종료되었습니다. 채널 소유자에게 새 초대 링크를 요청하세요.");
  }

  function stopPolling() {
    if (S.timer !== null) { window.clearTimeout(S.timer); S.timer = null; }
  }

  // ---- network (same-origin, only the four guest routes) --------------------------------------

  function call(method, path, body, cred) {
    var headers = { "content-type": "application/json" };
    if (cred) { headers.authorization = "Bearer " + cred; }
    var init = { method: method, headers: headers, credentials: "omit", cache: "no-store", referrerPolicy: "no-referrer" };
    if (body !== null) { init.body = JSON.stringify(body); }
    return window.fetch(path, init).then(function (res) {
      return res.json().catch(function () { return null; }).then(function (data) {
        return { status: res.status, data: data, retry: res.headers.get("retry-after") };
      });
    });
  }

  // ---- messages -----------------------------------------------------------------------------

  function addMessage(m) {
    var box = document.createElement("div");
    var kind = m.kind === "system" ? "system" : "";
    box.classList.add("msg");
    if (m.you === true) { box.classList.add("you"); }
    if (kind) { box.classList.add(kind); }
    var who = document.createElement("span");
    who.classList.add("who");
    var when = typeof m.at === "number" ? new Date(m.at).toLocaleTimeString("ko-KR", { hour: "2-digit", minute: "2-digit" }) : "";
    who.textContent = String(m.name) + (when ? "  " + when : "");
    var text = document.createElement("span");
    text.textContent = String(m.text);
    box.appendChild(who);
    box.appendChild(text);
    var log = $("log");
    var atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 60;
    log.appendChild(box);
    if (atBottom || m.you === true) { log.scrollTop = log.scrollHeight; }
  }

  function schedule(ms) {
    stopPolling();
    if (S.ended) { return; }
    S.timer = window.setTimeout(poll, ms);
  }

  function poll() {
    S.timer = null;
    if (S.ended) { return; }
    call("GET", "/api/guest/channels/" + encodeURIComponent(S.channelId) + "/messages?after=" + S.after, null, S.cred).then(function (r) {
      if (r.status === 401) { endInvitation(); return; }
      if (r.status === 404) { S.ended = true; showEnded("채널을 찾을 수 없습니다."); return; }
      if (r.status === 429) { schedule(backoffMs(r.retry)); return; }
      if (r.status === 200 && Array.isArray(r.data)) {
        r.data.forEach(function (m) {
          if (typeof m.seq === "number" && m.seq > S.after) { S.after = m.seq; addMessage(m); }
        });
        $("chaterr").textContent = "";
        schedule(POLL_MS);
        return;
      }
      schedule(POLL_MS * 2);
    }, function () {
      $("chaterr").textContent = "서버에 연결할 수 없습니다. 다시 시도하는 중입니다.";
      schedule(POLL_MS * 2);
    });
  }

  // ---- chat ---------------------------------------------------------------------------------

  function enterChat() {
    call("GET", "/api/guest/channels/" + encodeURIComponent(S.channelId), null, S.cred).then(function (r) {
      if (r.status === 401) { endInvitation(); return; }
      if (r.status !== 200 || !r.data) { showEnded("채널을 열 수 없습니다."); return; }
      $("chname").textContent = String(r.data.name);
      var members = Array.isArray(r.data.members) ? r.data.members : [];
      var agents = members.filter(function (m) { return m.is_agent; }).map(function (m) { return m.name; });
      var people = members.filter(function (m) { return !m.is_agent; }).map(function (m) { return m.name; });
      $("chmembers").textContent = "사람: " + (people.join(", ") || "-") + " / 에이전트: " + (agents.join(", ") || "-");
      show("chatview");
      poll();
    }, function () { showEnded("서버에 연결할 수 없습니다. 페이지를 새로 고치세요."); });
  }

  function startWith(sess) {
    S.cred = sess.credential;
    S.channelId = sess.channel_id;
    S.guestId = sess.guest_id;
    S.after = 0;
    S.ended = false;
    enterChat();
  }

  function onSend(ev) {
    ev.preventDefault();
    if (S.busy || S.ended) { return; }
    var box = $("text");
    var text = box.value;
    var err = $("chaterr");
    if (text.trim().length === 0) { return; }
    if (byteLength(text) > MAX_BYTES) { err.textContent = "메시지는 8192바이트까지 보낼 수 있습니다."; return; }
    S.busy = true;
    $("send").disabled = true;
    err.textContent = "";
    call("POST", "/api/guest/channels/" + encodeURIComponent(S.channelId) + "/messages", { text: text }, S.cred).then(function (r) {
      if (r.status === 401) { endInvitation(); return; }
      if (r.status === 201) { box.value = ""; stopPolling(); poll(); return; }
      err.textContent = errorText(r.status, r.data);
    }, function () {
      err.textContent = "서버에 연결할 수 없습니다.";
    }).then(function () {
      S.busy = false;
      $("send").disabled = false;
    });
  }

  // ---- join ---------------------------------------------------------------------------------

  function onJoin(token) {
    return function (ev) {
      ev.preventDefault();
      if (S.busy) { return; }
      var err = $("nameerr");
      var name = $("name").value.trim();
      if (!validName(name)) { err.textContent = "이름은 1자 이상 64자 이하로 입력하세요."; return; }
      S.busy = true;
      $("namego").disabled = true;
      err.textContent = "";
      call("POST", "/api/guest/accept", { token: token, name: name }, null).then(function (r) {
        if (r.status === 201 && r.data && typeof r.data.credential === "string") {
          saveSession(r.data);
          startWith(r.data);
          return;
        }
        if (r.status === 429) { err.textContent = errorText(429, r.data); return; }
        if (r.status === 400 && r.data && r.data.code === "invalid_name") { err.textContent = r.data.error || "이름을 확인하세요."; return; }
        if (r.status === 400 && r.data && typeof r.data.error === "string") { err.textContent = r.data.error; return; }
        showEnded("이 초대 링크는 사용할 수 없습니다. 이미 사용했거나 기간이 지났을 수 있습니다.");
      }, function () {
        err.textContent = "서버에 연결할 수 없습니다.";
      }).then(function () {
        S.busy = false;
        $("namego").disabled = false;
      });
    };
  }

  function main() {
    // Read the token, then take it out of the address bar before anything else runs.
    var token = tokenFromHash(window.location.hash);
    window.history.replaceState(null, "", window.location.pathname);

    $("chatform").addEventListener("submit", onSend);
    $("text").addEventListener("keydown", function (ev) {
      if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { $("chatform").requestSubmit(); ev.preventDefault(); }
    });

    // A fresh invite link wins over a session left in this tab; a reload has no token and resumes.
    if (token) {
      $("nameform").addEventListener("submit", onJoin(token));
      show("nameview");
      $("name").focus();
      return;
    }
    var sess = loadSession();
    if (sess) { startWith(sess); return; }
    showEnded("초대 링크로 접속하세요. 링크가 없거나 이미 사용되었습니다.");
  }

  main();
}());
