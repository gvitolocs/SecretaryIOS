/*
 * lucina-shim.js — the first-party adapter Lucina's live view injects into Cua's
 * viewer page. lucina-server's view proxy rewrites the sandbox's
 * `viewer/index.html` to load this before `viewer.js`, so it runs first, on the
 * viewer's own origin (an opaque origin: the page is served under CSP `sandbox`).
 *
 * It exists because Cua's viewer is a third-party minified bundle we do not
 * patch, and three things about it are wrong on a phone:
 *
 * 1. Keyboard. `viewer.js` creates a hidden `<textarea class="cua-keys">` for
 *    keystrokes and calls `.focus()` on it from the surface `pointerdown`
 *    handler (`this.focus()` before it looks at `pointerType`) and from every
 *    toolbar button. On iOS focusing a textarea opens the software keyboard, so
 *    every tap raised it. On touch screens the textarea is kept `readonly` +
 *    `inputmode="none"` (neither of which stops Cua's keydown/beforeinput
 *    handlers) and focusing it is a no-op until the owner asks for the
 *    keyboard with `{type:"lucina:keyboard", open:true}`. With a mouse
 *    (the Mac app, a desktop browser) nothing changes.
 *
 * 2. Tickets and takeover. A viewer ticket is minted per view and mode, and
 *    Cua's media session (H.264 over `/media`) is created from it. Renewing or
 *    switching watch↔control used to mean reloading this whole document. The
 *    parent now posts `{type:"lucina:ticket", ticket, mode}` and this script
 *    hands it to the running viewer: `window.__cuaViewer` is the viewer's own
 *    test hook (`exposeTestHooks()` in `viewer.js`) and exposes the app
 *    instance, whose `api.endpoint.ticket` every Connect RPC re-reads. For a
 *    mode change it also re-attaches the media session (`session.stop()`,
 *    `openMedia()`, `attach()`) — the viewer fixes `interactive` when the
 *    session is built, so a watch session can never carry input afterwards.
 *    With no hook (a future viewer) it falls back to reloading this document,
 *    which is still cheap: the viewer's assets are immutable and cached.
 *
 * 3. Touch. Cua already has a good model — tap = left click, 550 ms
 *    long-press = right click, one-finger drag = left-button drag, two
 *    fingers = scroll — so it is kept as is. This adds what is missing: a
 *    two-finger tap for a right click, pinch to zoom the local view (never
 *    sent to the desktop) with a "Fit" chip to reset, a ring where the
 *    last click landed so the owner can see the mouse works, and
 *    `touch-action: none` on the whole viewer (Cua sets it only on the desktop,
 *    so a gesture that began on its toolbar zoomed the page instead).
 *
 * The page's key buttons (Esc, Tab, arrows…) arrive as `{type:"lucina:keys"}`
 * and go out on the live socket like any other key.
 *
 * It also reports the stream state to the parent (`lucina:state`) so the app
 * can stop showing "Connecting…" on the first frame rather than on load.
 */
(function () {
  "use strict";

  var parentWindow = window.parent;
  if (parentWindow === window) return; // opened on its own: nothing to adapt

  var LONG_PRESS_MS = 550; // Cua's own threshold, so the markers agree
  var TAP_SLOP_PX = 10;
  var PINCH_RATIO = 0.12;
  var MAX_ZOOM = 4;
  var COARSE = !!(window.matchMedia && window.matchMedia("(pointer: coarse)").matches);

  function params() {
    var out = {};
    var raw = (location.hash || "").replace(/^#/, "");
    raw.split("&").forEach(function (pair) {
      if (!pair) return;
      var i = pair.indexOf("=");
      var k = i < 0 ? pair : pair.slice(0, i);
      var v = i < 0 ? "" : pair.slice(i + 1);
      try {
        out[decodeURIComponent(k)] = decodeURIComponent(v);
      } catch (e) {
        out[k] = v;
      }
    });
    return out;
  }

  var query = params();
  var ticket = query.ticket || "";
  var mode = query.mode === "watch" || query.mode === "control" ? query.mode : "control";

  function send(msg) {
    try {
      parentWindow.postMessage(msg, "*");
    } catch (e) {
      /* the parent went away */
    }
  }

  function app() {
    var hook = window.__cuaViewer;
    return hook && hook.viewer ? hook.viewer : null;
  }

  // ---------------------------------------------------------------- keyboard

  // Only on touch screens: with a mouse and a real keyboard, Cua's focus-on-click is right.
  // The gate is on the prototype, not on the element: Cua builds the textarea and focuses it in the
  // same task (`attach()` ends with `input.focus()`), before any observer could see the node.
  var keys = null; // the textarea the live input uses (the newest one)
  var keyboardOpen = false;
  var realFocus = HTMLTextAreaElement.prototype.focus;

  function isKeys(el) {
    return !!(el && el.classList && el.classList.contains("cua-keys"));
  }

  function guardKeys(el) {
    if (el.__lucinaGuarded) return;
    el.__lucinaGuarded = true;
    keys = el;
    el.setAttribute("inputmode", "none");
    el.setAttribute("autocapitalize", "none");
    el.setAttribute("autocorrect", "off");
    el.readOnly = true;
    if (document.activeElement === el && !keyboardOpen) el.blur();
    if (keyboardOpen) setKeyboard(true); // intent arrived before the textarea
  }

  function focusKeys() {
    try {
      realFocus.call(keys, { preventScroll: true });
    } catch (e) {
      /* not focusable */
    }
  }

  function setKeyboard(open) {
    keyboardOpen = !!open;
    if (!keys || !keys.isConnected) {
      var all = document.querySelectorAll(".cua-keys");
      keys = all.length ? all[all.length - 1] : null;
    }
    if (!keys) return;
    if (open) {
      keys.readOnly = false;
      keys.setAttribute("inputmode", "text");
      focusKeys();
    } else {
      keys.readOnly = true;
      keys.setAttribute("inputmode", "none");
      try {
        keys.blur();
      } catch (e) {
        /* nothing focused */
      }
    }
  }

  function findKeys() {
    var all = document.querySelectorAll(".cua-keys");
    for (var i = 0; i < all.length; i += 1) guardKeys(all[i]);
    if (all.length) keys = all[all.length - 1]; // the live input's is the newest
  }

  function watchForKeys() {
    if (!COARSE) return;
    HTMLTextAreaElement.prototype.focus = function (options) {
      if (isKeys(this)) {
        guardKeys(this);
        keys = this;
        if (!keyboardOpen) return; // a tap, a toolbar button, a re-attach: no keyboard
      }
      return realFocus.call(this, options);
    };
    findKeys();
    // Cua builds the textarea when the first control session attaches, and again on a re-attach.
    new MutationObserver(findKeys).observe(document.documentElement, { childList: true, subtree: true });
  }

  // ------------------------------------------------------------------ ticket

  function applyTicket(next) {
    if (!next || next === ticket) {
      ticket = next || ticket;
      return;
    }
    ticket = next;
    var viewer = app();
    if (viewer && viewer.api && viewer.api.endpoint) {
      viewer.api.endpoint.ticket = ticket; // every Connect RPC re-reads it
    }
  }

  // ------------------------------------------------------------ while driving

  // The page passes the click (or key) that took control through once control really works:
  // the new session is attached *and* streaming, or an input queued before then is dropped.
  var switching = false;
  var queued = [];

  function driving() {
    var viewer = app();
    var dot = document.querySelector(".cua-dot");
    return mode === "control" && !switching && !!viewer && !!viewer.session && !!dot && dot.getAttribute("data-state") === "streaming";
  }

  function whenDriving(fn) {
    if (driving()) {
      fn();
      return;
    }
    if (queued.length < 4) queued.push({ fn: fn, at: Date.now() });
  }

  function flushQueued() {
    if (!driving()) return;
    var run = queued.splice(0);
    for (var i = 0; i < run.length; i += 1) {
      if (Date.now() - run[i].at < 10000) {
        try {
          run[i].fn();
        } catch (e) {
          /* the session went away again */
        }
      }
    }
  }

  /** Keyboard focus to the stream (hardware keyboards: the Mac); never on touch, where it is the keyboard button's. */
  function focusStream() {
    if (COARSE) return;
    var viewer = app();
    try {
      if (viewer && viewer.input && typeof viewer.input.focus === "function") viewer.input.focus();
    } catch (e) {
      /* no input yet */
    }
  }

  /** Re-open the media session so a mode change takes effect (no reload). */
  function reattach(nextMode) {
    switching = true;
    var viewer = app();
    if (!viewer || typeof viewer.openMedia !== "function" || typeof viewer.attach !== "function") {
      reloadWithTicket(nextMode);
      return;
    }
    var old = viewer.session;
    viewer.session = null;
    // attach() builds a new input (and textarea) for a control session but never drops the old
    // one, whose listeners would stay on the stage and keep stealing focus: drop it here.
    try {
      if (viewer.input && typeof viewer.input.dispose === "function") viewer.input.dispose();
    } catch (e) {
      /* already gone */
    }
    viewer.input = null;
    try {
      if (old && typeof old.stop === "function") old.stop();
    } catch (e) {
      /* already stopped */
    }
    var opened;
    try {
      opened = viewer.openMedia(false); // audio uplink stays off
    } catch (e) {
      reloadWithTicket(nextMode);
      return;
    }
    Promise.resolve(opened).then(
      function (media) {
        viewer.attach(media);
        mode = nextMode;
        switching = false;
        flash("Switched to " + (nextMode === "control" ? "control" : "watching"));
      },
      function () {
        reloadWithTicket(nextMode);
      }
    );
  }

  /**
   * Last resort for a viewer without the test hook: reload this document with
   * the new ticket. The path query changes so the browser really reloads (a
   * fragment-only change would not), and the assets come from the HTTP cache.
   */
  function reloadWithTicket(nextMode) {
    // Cua strips the ticket from the fragment once it has read it, so put it back.
    var parts = location.hash.replace(/^#/, "").split("&").filter(function (pair) {
      return pair && pair.indexOf("ticket=") !== 0;
    });
    parts.unshift("ticket=" + encodeURIComponent(ticket));
    var seen = false;
    parts = parts.map(function (pair) {
      if (pair.indexOf("mode=") === 0) {
        seen = true;
        return "mode=" + nextMode;
      }
      return pair;
    });
    if (!seen) parts.push("mode=" + nextMode);
    var path = location.pathname + "?r=" + Date.now() + "#" + parts.join("&");
    try {
      location.replace(path);
    } catch (e) {
      location.href = path;
    }
  }

  // ------------------------------------------------------------------- touch

  var stage = null;
  var canvas = null;
  var pointers = new Map();
  var two = null; // the two-finger gesture in progress
  var pinching = false;
  var zoom = 1;
  var panX = 0;
  var panY = 0;

  function surface() {
    if (!stage || !stage.isConnected) stage = document.querySelector(".cua-stage");
    if (!canvas || !canvas.isConnected) canvas = document.querySelector(".cua-screen");
    return canvas;
  }

  /** Send a pointer pair through the viewer's own input queue. */
  /** Cua's own pointer coordinates: normalized to the desktop canvas as it is shown. */
  function normalizedAt(clientX, clientY) {
    var c = surface();
    if (!c) return null;
    var r = c.getBoundingClientRect();
    if (!r.width || !r.height) return null;
    var clamp = function (v) {
      return Math.min(1, Math.max(0, v));
    };
    return { x_normalized: clamp((clientX - r.left) / r.width), y_normalized: clamp((clientY - r.top) / r.height) };
  }

  /** A click through the viewer's own input queue, exactly as Cua sends one. */
  function clickAt(clientX, clientY, button) {
    var viewer = app();
    var p = normalizedAt(clientX, clientY);
    if (!viewer || typeof viewer.sendInput !== "function" || !p) return;
    try {
      viewer.sendInput({ kind: "pointer", phase: "move", button: null, x_normalized: p.x_normalized, y_normalized: p.y_normalized, modifiers: [] });
      viewer.sendInput({ kind: "pointer", phase: "down", button: button, x_normalized: p.x_normalized, y_normalized: p.y_normalized, modifiers: [] });
      viewer.sendInput({ kind: "pointer", phase: "up", button: button, x_normalized: p.x_normalized, y_normalized: p.y_normalized, modifiers: [] });
    } catch (e) {
      /* the session is between re-attaches */
    }
  }

  var ringTimer = null;
  function mark(clientX, clientY, strong) {
    var el = document.getElementById("lucina-click");
    if (!el) {
      el = document.createElement("div");
      el.id = "lucina-click";
      el.style.cssText =
        "position:fixed;left:0;top:0;width:34px;height:34px;margin:-17px 0 0 -17px;border-radius:50%;" +
        "border:2px solid rgba(255,255,255,.85);box-shadow:0 0 0 2px rgba(0,0,0,.35);" +
        "pointer-events:none;z-index:2147483647;opacity:0;transition:opacity .45s ease-out;" +
        "box-sizing:border-box";
      document.documentElement.appendChild(el);
    }
    el.style.transform = "translate(" + clientX + "px," + clientY + "px)";
    el.style.borderColor = strong ? "rgba(255,176,32,.95)" : "rgba(255,255,255,.85)";
    el.style.opacity = "0.95";
    if (ringTimer) window.clearTimeout(ringTimer);
    ringTimer = window.setTimeout(function () {
      el.style.opacity = "0";
    }, 220);
  }

  function applyTransform() {
    var s = stage || document.querySelector(".cua-stage");
    if (!s) return;
    if (zoom <= 1.02) {
      zoom = 1;
      panX = 0;
      panY = 0;
      s.style.transform = "";
      s.style.transformOrigin = "";
      chip(false);
      return;
    }
    s.style.transformOrigin = "0 0";
    s.style.transform = "translate(" + panX + "px," + panY + "px) scale(" + zoom + ")";
    chip(true);
  }

  var chipEl = null;
  function chip(show) {
    if (!show) {
      if (chipEl && chipEl.parentNode) chipEl.parentNode.removeChild(chipEl);
      chipEl = null;
      return;
    }
    if (!chipEl) {
      chipEl = document.createElement("button");
      chipEl.type = "button";
      chipEl.id = "lucina-zoom";
      chipEl.style.cssText =
        "position:fixed;right:calc(10px + env(safe-area-inset-right));bottom:calc(10px + env(safe-area-inset-bottom));" +
        "z-index:2147483646;padding:6px 12px;border-radius:999px;border:1px solid rgba(255,255,255,.25);" +
        "background:rgba(22,25,30,.92);color:#e8eaed;font:600 12px/1.2 -apple-system,BlinkMacSystemFont,sans-serif;" +
        "cursor:pointer;-webkit-tap-highlight-color:transparent";
      chipEl.textContent = "Fit";
      chipEl.addEventListener("click", function (event) {
        event.preventDefault();
        event.stopPropagation();
        zoom = 1;
        panX = 0;
        panY = 0;
        applyTransform();
      });
      document.documentElement.appendChild(chipEl);
    }
    chipEl.textContent = "Fit (" + zoom.toFixed(1) + "×)";
  }

  function mid(a, b) {
    return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  }

  function distance(a, b) {
    return Math.hypot(a.x - b.x, a.y - b.y);
  }

  function onDown(event) {
    if (event.pointerType !== "touch") return;
    if (event.target && event.target.closest && event.target.closest("#lucina-zoom")) return;
    var id = event.pointerId == null ? 0 : event.pointerId;
    pointers.set(id, { x: event.clientX, y: event.clientY, x0: event.clientX, y0: event.clientY, t0: Date.now() });
    if (pointers.size === 2) {
      var all = Array.from(pointers.values());
      two = {
        d0: Math.max(distance(all[0], all[1]), 1),
        mid0: mid(all[0], all[1]),
        moved: 0,
        zoom0: zoom,
        panX0: panX,
        panY0: panY,
      };
      pinching = false;
    }
  }

  function onMove(event) {
    if (event.pointerType !== "touch") return;
    var id = event.pointerId == null ? 0 : event.pointerId;
    var p = pointers.get(id);
    if (!p) return;
    p.x = event.clientX;
    p.y = event.clientY;
    if (pointers.size !== 2 || !two) return;
    var all = Array.from(pointers.values());
    var d = Math.max(distance(all[0], all[1]), 1);
    var m = mid(all[0], all[1]);
    // A pinch changes the fingers' distance more than it moves their midpoint (a swipe scrolls).
    if (!pinching && Math.abs(d / two.d0 - 1) > PINCH_RATIO && Math.abs(d - two.d0) > distance(m, two.mid0)) {
      pinching = true;
      two.zoom0 = zoom;
    }
    two.moved = Math.max(two.moved, distance({ x: m.x, y: m.y }, { x: two.mid0.x, y: two.mid0.y }));
    if (!pinching) return;
    // Swallow the gesture so Cua does not also scroll the desktop.
    event.stopPropagation();
    event.preventDefault();
    zoom = Math.min(MAX_ZOOM, Math.max(1, two.zoom0 * (d / two.d0)));
    panX = two.panX0 + (m.x - two.mid0.x);
    panY = two.panY0 + (m.y - two.mid0.y);
    applyTransform();
  }

  function onUp(event) {
    if (event.pointerType !== "touch") return;
    var id = event.pointerId == null ? 0 : event.pointerId;
    var p = pointers.get(id);
    if (p && two && !pinching && pointers.size === 2) {
      var all = Array.from(pointers.values());
      var m = mid(all[0], all[1]);
      var still = distance({ x: m.x, y: m.y }, { x: two.mid0.x, y: two.mid0.y }) < 20;
      if (still && two.moved < 20 && Date.now() - Math.min(all[0].t0, all[1].t0) < LONG_PRESS_MS) {
        // Two-finger tap: a right click, which Cua's touch model leaves out.
        clickAt(m.x, m.y, "right");
        mark(m.x, m.y, true);
      }
      if (pinching) event.stopPropagation();
    } else if (p && pointers.size === 1 && event.type === "pointerup") {
      // One finger: Cua clicked (tap) or right-clicked (long press) where it went down.
      var moved = distance({ x: event.clientX, y: event.clientY }, { x: p.x0, y: p.y0 });
      if (moved <= TAP_SLOP_PX) mark(p.x0, p.y0, Date.now() - p.t0 >= LONG_PRESS_MS);
    }
    pointers.delete(id);
    if (pointers.size < 2) {
      two = null;
      pinching = false;
    }
  }

  function wireTouch() {
    if (!COARSE) return;
    // Every gesture in the viewer is ours (or Cua's), never the browser's: Cua sets this only on the
    // desktop itself, so a swipe or pinch that started on its toolbar panned or zoomed the whole
    // page. No iOS long-press callout or text selection either (the long press is a right click).
    var style = document.createElement("style");
    style.textContent =
      "html,body,body *{touch-action:none!important;-webkit-touch-callout:none}" +
      // (not the keys textarea: iOS will not type into an unselectable field)
      "body *:not(textarea){-webkit-user-select:none;user-select:none}";
    (document.head || document.documentElement).appendChild(style);
    var opts = { capture: true, passive: true };
    document.addEventListener("pointerdown", onDown, opts);
    document.addEventListener("pointermove", onMove, { capture: true, passive: false });
    document.addEventListener("pointerup", onUp, true);
    document.addEventListener("pointercancel", onUp, true);
    window.addEventListener("orientationchange", function () {
      window.setTimeout(applyTransform, 250);
    });
  }

  /** A chord from the page's key buttons (`["control", "l"]`): all down, then up in reverse. */
  function pressKeys(list) {
    var viewer = app();
    if (!viewer || typeof viewer.sendInput !== "function" || !Array.isArray(list) || !list.length || list.length > 4) return;
    for (var i = 0; i < list.length; i += 1) {
      if (typeof list[i] !== "string" || !/^[a-z0-9]{1,16}$/.test(list[i])) return;
    }
    try {
      list.forEach(function (key) {
        viewer.sendInput({ kind: "key", key: key, state: "down", modifiers: [], repeat: false });
      });
      list.slice().reverse().forEach(function (key) {
        viewer.sendInput({ kind: "key", key: key, state: "up", modifiers: [], repeat: false });
      });
    } catch (e) {
      /* the session is between re-attaches */
    }
  }

  // ------------------------------------------------------------------ status

  var toast = null;
  function flash(text) {
    if (!toast) {
      toast = document.createElement("div");
      toast.id = "lucina-flash";
      toast.style.cssText =
        "position:fixed;left:50%;bottom:calc(16px + env(safe-area-inset-bottom));transform:translateX(-50%);" +
        "padding:7px 14px;border-radius:999px;background:rgba(22,25,30,.92);color:#e8eaed;" +
        "border:1px solid rgba(255,255,255,.18);font:13px/1.4 -apple-system,BlinkMacSystemFont,sans-serif;" +
        "z-index:2147483646;pointer-events:none;opacity:0;transition:opacity .3s";
      document.documentElement.appendChild(toast);
    }
    toast.textContent = text;
    toast.style.opacity = "1";
    window.setTimeout(function () {
      toast.style.opacity = "0";
    }, 1400);
  }

  var lastState = "";
  function watchStatus() {
    var report = function (state) {
      if (state === lastState) return;
      lastState = state;
      send({ type: "lucina:state", state: state });
    };
    var find = function () {
      var dot = document.querySelector(".cua-dot");
      return dot ? dot.getAttribute("data-state") || "" : "";
    };
    var dotTimer = window.setInterval(function () {
      var dot = document.querySelector(".cua-dot");
      if (!dot) return;
      window.clearInterval(dotTimer);
      report(dot.getAttribute("data-state") || "connecting");
      var observer = new MutationObserver(function () {
        report(dot.getAttribute("data-state") || "");
        flushQueued();
      });
      observer.observe(dot, { attributes: true, attributeFilter: ["data-state"] });
    }, 100);
  }

  // ------------------------------------------------------------ the viewer's own toolbar

  /**
   * Cua's fullscreen button asks the page instead: the page knows where it runs (the Fullscreen API
   * in a browser, its own full-screen layout on iPhone, the Mac app's focus mode inside Secretary
   * Code, where a frame's requestFullscreen would full-screen the whole window).
   */
  function routeFullscreen() {
    document.addEventListener(
      "click",
      function (event) {
        var target = event.target;
        var button = target && target.closest ? target.closest('[data-id="fullscreen"]') : null;
        if (!button) return;
        event.preventDefault();
        event.stopPropagation();
        send({ type: "lucina:fullscreen" });
      },
      true
    );
  }

  /**
   * The toolbar floats over the top of the desktop: it fades out two seconds after the pointer last
   * moved (or the finger last touched) and comes back on the next move or touch; while hidden it lets
   * clicks through to the desktop. It stays while hovered, while keyboard focus is on it (not the
   * focus a click leaves on a button), while one of its menus is open and while the stream is
   * connecting or failed.
   */
  var QUIET_MS = 2000;
  function autoHideToolbar() {
    var style = document.createElement("style");
    style.textContent =
      "html.lucina-quiet .cua-viewer:not([data-status=connecting]):not([data-status=failed]) " +
      ".cua-toolbar:not(:hover):not(:has(:focus-visible)):not(:has(.cua-popover.open)){opacity:0!important;pointer-events:none}" +
      ".cua-toolbar{transition:opacity .35s}";
    (document.head || document.documentElement).appendChild(style);
    var root = document.documentElement;
    var timer = null;
    var wake = function () {
      root.classList.remove("lucina-quiet");
      if (timer) window.clearTimeout(timer);
      timer = window.setTimeout(function () {
        root.classList.add("lucina-quiet");
      }, QUIET_MS);
    };
    document.addEventListener("pointermove", wake, { capture: true, passive: true });
    document.addEventListener("pointerdown", wake, { capture: true, passive: true });
    wake();
  }

  // ------------------------------------------------------------------ wiring

  window.addEventListener("message", function (event) {
    if (event.source !== parentWindow) return;
    var data = event.data;
    if (!data || typeof data.type !== "string" || data.type.indexOf("lucina:") !== 0) return;
    switch (data.type) {
      case "lucina:keyboard":
        setKeyboard(!!data.open);
        break;
      case "lucina:keys":
        pressKeys(data.keys);
        break;
      case "lucina:pass": {
        // The click or key that took control, replayed once control works.
        var pass = data;
        whenDriving(function () {
          if (pass.click && typeof pass.click.x === "number" && typeof pass.click.y === "number") {
            clickAt(pass.click.x, pass.click.y, pass.click.button === "right" ? "right" : "left");
            mark(pass.click.x, pass.click.y, pass.click.button === "right");
          }
          if (typeof pass.text === "string" && pass.text.length > 0 && pass.text.length <= 8) {
            var viewer = app();
            if (viewer) viewer.sendInput({ kind: "text_commit", text: pass.text });
          }
          if (Array.isArray(pass.keys)) pressKeys(pass.keys);
          focusStream();
        });
        break;
      }
      case "lucina:focus":
        whenDriving(focusStream);
        break;
      case "lucina:ticket": {
        var nextMode = data.mode === "watch" || data.mode === "control" ? data.mode : mode;
        var changed = nextMode !== mode;
        applyTicket(typeof data.ticket === "string" ? data.ticket : "");
        if (changed) reattach(nextMode);
        break;
      }
      default:
        break;
    }
  });

  watchForKeys();
  wireTouch();
  watchStatus();
  routeFullscreen();
  autoHideToolbar();
  send({ type: "lucina:ready", coarse: COARSE, mode: mode });
})();
