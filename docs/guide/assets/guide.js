/* Ember guide: the only JavaScript on the site.
   Three jobs: theme toggle, sidebar toggle on narrow screens, copy buttons.
   Everything else (navigation, language switch) is plain links and works
   with JavaScript disabled. */
(function () {
  "use strict";

  var root = document.documentElement;

  /* ---- theme ---------------------------------------------------------- */
  /* No stored value means "follow the system"; the stylesheet does that with
     prefers-color-scheme. Clicking the toggle stores an explicit choice. */
  var toggle = document.getElementById("themetoggle");
  if (toggle) {
    var label = function () {
      var explicit = root.getAttribute("data-theme");
      var dark = explicit
        ? explicit === "dark"
        : window.matchMedia("(prefers-color-scheme: dark)").matches;
      toggle.textContent = dark ? toggle.dataset.light : toggle.dataset.dark;
      toggle.setAttribute("aria-pressed", dark ? "true" : "false");
    };
    label();
    toggle.addEventListener("click", function () {
      var explicit = root.getAttribute("data-theme");
      var dark = explicit
        ? explicit === "dark"
        : window.matchMedia("(prefers-color-scheme: dark)").matches;
      var next = dark ? "light" : "dark";
      root.setAttribute("data-theme", next);
      try { localStorage.setItem("dxc-theme", next); } catch (e) { /* private mode */ }
      label();
    });
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", function () {
      if (!root.getAttribute("data-theme")) label();
    });
  }

  /* ---- remember the reader's language for the entry page --------------- */
  var lang = root.getAttribute("lang");
  if (lang) { try { localStorage.setItem("dxc-lang", lang); } catch (e) { /* ignore */ } }

  /* ---- sidebar on narrow screens --------------------------------------- */
  var navtoggle = document.getElementById("navtoggle");
  var sidebar = document.getElementById("sidebar");
  if (navtoggle && sidebar) {
    navtoggle.addEventListener("click", function () {
      var open = sidebar.classList.toggle("open");
      navtoggle.setAttribute("aria-expanded", open ? "true" : "false");
    });
    sidebar.addEventListener("click", function (event) {
      if (event.target.tagName === "A") {
        sidebar.classList.remove("open");
        navtoggle.setAttribute("aria-expanded", "false");
      }
    });
  }

  /* ---- copy buttons on code blocks ------------------------------------- */
  if (navigator.clipboard) {
    var blocks = document.querySelectorAll(".codeblock");
    for (var i = 0; i < blocks.length; i++) {
      (function (block) {
        var pre = block.querySelector("pre");
        if (!pre) return;
        var button = document.createElement("button");
        button.type = "button";
        button.className = "copy";
        button.textContent = block.dataset.copy || "Copy";
        button.addEventListener("click", function () {
          navigator.clipboard.writeText(pre.innerText).then(function () {
            button.textContent = block.dataset.copied || "Copied";
            button.dataset.done = "true";
            setTimeout(function () {
              button.textContent = block.dataset.copy || "Copy";
              button.removeAttribute("data-done");
            }, 1600);
          });
        });
        block.appendChild(button);
      })(blocks[i]);
    }
  }
})();
