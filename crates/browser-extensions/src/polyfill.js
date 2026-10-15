// NxBrowser: Chrome-only extension APIs that WebKit does not implement. Loaded before the
// extension's own code so store builds that register listeners at startup keep running.
(() => {
  "use strict";
  const g = globalThis;
  // `using` declarations (TypeScript's helpers, wasm-bindgen classes) need these well-known
  // symbols, which this WebKit lacks; without them Bitwarden's login and logout throw.
  for (const name of ["dispose", "asyncDispose"]) {
    if (typeof Symbol[name] !== "symbol") {
      Object.defineProperty(Symbol, name, { value: Symbol(`Symbol.${name}`) });
    }
  }
  // WebKit's extension error list keeps only the first argument of `console.error`, so
  // "Unhandled error in angular" arrives without the error; fold the rest into one line.
  if (g.console && g.console.error) {
    const consoleError = g.console.error.bind(g.console);
    const describe = (value) => {
      if (value instanceof Error) return `${value.name}: ${value.message}${value.stack ? `\n${value.stack}` : ""}`;
      if (value && typeof value === "object") {
        try {
          return JSON.stringify(value);
        } catch (_) {}
      }
      return String(value);
    };
    g.console.error = (...args) => consoleError(args.length > 1 || args[0] instanceof Error ? args.map(describe).join(" ") : args[0]);
  }
  if (!g.chrome && !g.browser) return;
  const chrome = g.chrome || g.browser;
  const event = () => {
    const listeners = new Set();
    return {
      addListener: (fn) => listeners.add(fn),
      removeListener: (fn) => listeners.delete(fn),
      hasListener: (fn) => listeners.has(fn),
      hasListeners: () => listeners.size > 0,
    };
  };
  // Chrome APIs take an optional trailing callback and otherwise return a promise.
  const method = (fn) => (...args) => {
    const callback = typeof args[args.length - 1] === "function" ? args.pop() : null;
    const result = Promise.resolve().then(() => fn(...args));
    if (!callback) return result;
    result.then((value) => callback(value), () => callback());
  };
  const unsupported = (name) => method(() => {
    throw new Error(`${name} is not supported in NxBrowser`);
  });
  const define = (target, key, value) => {
    if (target && target[key] === undefined) {
      try {
        Object.defineProperty(target, key, { value, configurable: true, enumerable: true, writable: true });
      } catch (_) {}
    }
  };
  const names = new Proxy({}, { get: (_, key) => (typeof key === "string" ? key : undefined) });

  let nextNotification = 1;
  define(chrome, "notifications", {
    create: method((id, options) => {
      if (typeof id !== "string") {
        options = id;
        id = `nx-${nextNotification++}`;
      }
      // Shown as a web notification where the page is allowed to; otherwise dropped silently.
      try {
        if (g.Notification && g.Notification.permission === "granted") {
          new g.Notification((options && options.title) || "", { body: (options && options.message) || "" });
        }
      } catch (_) {}
      return id;
    }),
    update: method(() => true),
    clear: method(() => true),
    getAll: method(() => ({})),
    getPermissionLevel: method(() => "granted"),
    onClicked: event(),
    onButtonClicked: event(),
    onClosed: event(),
    onPermissionLevelChanged: event(),
    onShowSettings: event(),
    TemplateType: names,
    PermissionLevel: names,
  });

  // Side panels open as their own window, via a tab at the panel page. Only for extensions without a
  // popup (Kluff calls `sidePanel` unguarded): ones with a popup take `sidePanel` as a cue to move their
  // UI into a panel we can't attach, and Phantom then clears its popup and reopens it by script.
  const manifest = chrome.runtime && chrome.runtime.getManifest ? chrome.runtime.getManifest() : {};
  const hasPopup = !!((manifest.action || manifest.browser_action || {}).default_popup);
  let panelBehavior = { openPanelOnActionClick: false };
  let panelOptions = {};
  const panelPath = () => panelOptions.path || (manifest.side_panel && manifest.side_panel.default_path) || "";
  if (!hasPopup) define(chrome, "sidePanel", {
    setPanelBehavior: method((behavior) => {
      panelBehavior = { ...panelBehavior, ...behavior };
    }),
    getPanelBehavior: method(() => panelBehavior),
    setOptions: method((options) => {
      panelOptions = { ...panelOptions, ...options };
    }),
    getOptions: method(() => ({ enabled: true, path: panelPath(), ...panelOptions })),
    open: method(() => {
      const path = panelPath();
      if (path && chrome.tabs && chrome.tabs.create) return chrome.tabs.create({ url: chrome.runtime.getURL(path) }).then(() => undefined);
    }),
    onOpened: event(),
    onClosed: event(),
  });

  // WebKit runs extension service workers on the web process's main thread, and its worker
  // WebSocket blocks that thread waiting on itself: `new WebSocket()` freezes the extension's
  // process for good, popups included (Bitwarden opens one for live sync right after login).
  // A socket that fails like a dropped connection lets the extension retry or fall back instead.
  if (typeof ServiceWorkerGlobalScope !== "undefined" && g instanceof ServiceWorkerGlobalScope && g.WebSocket) {
    class UnavailableWebSocket extends EventTarget {
      constructor(url) {
        super();
        this.url = String(url);
        this.protocol = "";
        this.extensions = "";
        this.binaryType = "blob";
        this.bufferedAmount = 0;
        this.readyState = 0;
        this.onopen = this.onmessage = this.onerror = this.onclose = null;
        setTimeout(() => {
          this.readyState = 3;
          const error = new Event("error");
          this.dispatchEvent(error);
          if (this.onerror) this.onerror(error);
          const close = new CloseEvent("close", { code: 1006, reason: "WebSocket is unavailable in WebKit extension service workers" });
          this.dispatchEvent(close);
          if (this.onclose) this.onclose(close);
        }, 0);
      }
      send() {
        throw new DOMException("WebSocket is closed", "InvalidStateError");
      }
      close() {}
    }
    for (const [name, value] of Object.entries({ CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 })) {
      UnavailableWebSocket[name] = value;
      UnavailableWebSocket.prototype[name] = value;
    }
    g.WebSocket = UnavailableWebSocket;
  }

  // Chrome keeps the background alive while extension pages use it. WebKit unloads it after 30 idle
  // seconds, and after 2 minutes even with ports open unless the background itself posts on one; the
  // ports it drops stay dead (Phantom's window holds one and never reconnects: a black window). So each
  // extension page, and a hidden `nx-keepalive.html` the engine keeps per extension, holds a port the
  // background pings, which keeps it loaded and already listening when a page connects.
  const KEEPALIVE = "nx-keepalive";
  const isServiceWorker = typeof ServiceWorkerGlobalScope !== "undefined" && g instanceof ServiceWorkerGlobalScope;
  let isBackgroundPage = false;
  if (typeof window !== "undefined" && g === window) {
    try {
      isBackgroundPage = !!(chrome.extension && chrome.extension.getBackgroundPage && chrome.extension.getBackgroundPage() === window);
    } catch (_) {}
  }
  if ((isServiceWorker || isBackgroundPage) && chrome.runtime && chrome.runtime.onConnect) {
    chrome.runtime.onConnect.addListener((port) => {
      if (port.name !== KEEPALIVE) return;
      const timer = setInterval(() => {
        try {
          port.postMessage(0);
        } catch (_) {}
      }, 20000);
      port.onDisconnect.addListener(() => clearInterval(timer));
    });
  } else if (typeof window !== "undefined" && g === window && chrome.runtime && chrome.runtime.connect) {
    const hold = () => {
      try {
        chrome.runtime.connect({ name: KEEPALIVE }).onDisconnect.addListener(() => {
          void chrome.runtime.lastError;
          setTimeout(hold, 1000);
        });
      } catch (_) {
        setTimeout(hold, 5000);
      }
    };
    hold();
  }

  // No `chrome.offscreen` stub: extensions route core features through it once it exists
  // (Bitwarden's storage does), and a failing one hangs them.

  define(chrome, "tabGroups", {
    query: method(() => []),
    get: unsupported("chrome.tabGroups"),
    update: unsupported("chrome.tabGroups"),
    move: unsupported("chrome.tabGroups"),
    onCreated: event(),
    onUpdated: event(),
    onRemoved: event(),
    onMoved: event(),
    TAB_GROUP_ID_NONE: -1,
    Color: names,
  });
  if (chrome.tabs) {
    define(chrome.tabs, "group", unsupported("chrome.tabs.group"));
    define(chrome.tabs, "ungroup", method(() => undefined));
  }

  // Phantom reads the redirect URL when its background loads; without it the background (and so the popup) never starts.
  define(chrome, "identity", {
    getRedirectURL: (path = "") => `https://${chrome.runtime.id}.chromiumapp.org/${String(path).replace(/^\//, "")}`,
    launchWebAuthFlow: unsupported("chrome.identity.launchWebAuthFlow"),
    getAuthToken: unsupported("chrome.identity.getAuthToken"),
    removeCachedAuthToken: method(() => undefined),
    clearAllCachedAuthTokens: method(() => undefined),
    getProfileUserInfo: method(() => ({ email: "", id: "" })),
    onSignInChanged: event(),
  });

  // WebKit throws on permissions it doesn't know ("'privacy' is not a valid permission") where Chrome
  // answers, which crashes Bitwarden's popup. Those can't be granted here, so the answer is "no".
  // `patch_pages` points the extension's permissions calls here; WebKit's object can't be patched.
  // (Written without a dot after `permissions`, so that rewrite leaves this file alone.)
  const permissions = chrome["permissions"];
  if (permissions) {
    const guarded = (name) => (...args) => {
      try {
        return permissions[name](...args);
      } catch (error) {
        if (!/is not a valid permission/.test(String(error && error.message))) throw error;
        const callback = typeof args[args.length - 1] === "function" ? args[args.length - 1] : null;
        if (!callback) return Promise.resolve(false);
        setTimeout(() => callback(false), 0);
      }
    };
    g.nxPermissions = new Proxy(permissions, {
      get: (target, key) => {
        if (key === "contains" || key === "request" || key === "remove") return guarded(key);
        const value = target[key];
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
  }

  // Registered content scripts, as Chrome runs them. WebKit gets two things wrong:
  // - it records a script's ID only once it has saved it, so overlapping `registerContentScripts`
  //   calls both pass its duplicate check; calls here run one at a time instead.
  // - it injects a newly registered script into pages already open, where Chrome waits for the next
  //   load. Extensions re-register on every start (Phantom: unregister all, register again), so open
  //   pages got a second copy ("Attempting to change value of a readonly property"). An unregister
  //   is held back briefly here, and re-registering the same script in that time keeps the old one.
  // `patch_pages` points the extension's calls here.
  const scripting = chrome["scripting"];
  if (scripting) {
    const UNREGISTER_DELAY = 1500;
    let queue = Promise.resolve();
    const later = new Map(); // id -> timer for a held-back unregister
    const enqueue = (task) => {
      const result = queue.then(task);
      queue = result.catch(() => {});
      return result;
    };
    const defaults = { js: [], css: [], excludeMatches: [], allFrames: false, runAt: "document_idle", world: "isolated", matchOriginAsFallback: false };
    const same = (a, b) => {
      const norm = (script) => JSON.stringify(Object.keys(defaults).concat("matches").map((key) => {
        const value = script[key] === undefined ? defaults[key] : script[key];
        return typeof value === "string" ? value.toLowerCase() : value;
      }));
      return norm(a) === norm(b);
    };
    const live = async (filter) => (await scripting.getRegisteredContentScripts(filter || {})).filter((script) => !later.has(script.id));
    const flush = (ids) => {
      ids = ids.filter((id) => later.has(id));
      ids.forEach((id) => {
        clearTimeout(later.get(id));
        later.delete(id);
      });
      return ids.length ? scripting.unregisterContentScripts({ ids }) : Promise.resolve();
    };
    const ops = {
      getRegisteredContentScripts: (filter) => live(filter),
      unregisterContentScripts: async (filter) => {
        const registered = (await live()).map((script) => script.id);
        const ids = filter && filter.ids ? filter.ids : registered;
        const missing = ids.find((id) => !registered.includes(id));
        if (missing !== undefined) throw new Error(`Invalid call to scripting.unregisterContentScripts(). No script with ID '${missing}'.`);
        ids.forEach((id) => later.set(id, setTimeout(() => enqueue(() => flush([id])), UNREGISTER_DELAY)));
      },
      registerContentScripts: async (scripts) => {
        const current = await scripting.getRegisteredContentScripts({});
        const fresh = [];
        for (const script of scripts) {
          const held = later.has(script.id);
          const existing = current.find((registered) => registered.id === script.id);
          if (held && existing && same(existing, script)) {
            clearTimeout(later.get(script.id));
            later.delete(script.id);
            continue;
          }
          if (held) await flush([script.id]);
          fresh.push(script);
        }
        if (fresh.length) await scripting.registerContentScripts(fresh);
      },
      updateContentScripts: async (scripts) => {
        await flush([...later.keys()]);
        return scripting.updateContentScripts(scripts);
      },
    };
    g.nxScripting = new Proxy(scripting, {
      get: (target, key) => {
        if (Object.prototype.hasOwnProperty.call(ops, key)) {
          return (...args) => {
            const callback = typeof args[args.length - 1] === "function" ? args.pop() : null;
            const result = enqueue(() => ops[key](...args));
            if (!callback) return result;
            result.then((value) => callback(value), () => callback());
          };
        }
        const value = target[key];
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
  }

  define(chrome, "debugger", {
    attach: unsupported("chrome.debugger"),
    detach: method(() => undefined),
    sendCommand: unsupported("chrome.debugger"),
    getTargets: method(() => []),
    onEvent: event(),
    onDetach: event(),
  });

  const setting = () => ({
    get: method(() => ({ value: false, levelOfControl: "not_controllable" })),
    set: method(() => undefined),
    clear: method(() => undefined),
    onChange: event(),
  });
  const settings = () => new Proxy({}, { get: (cache, key) => (cache[key] = cache[key] || setting()) });
  define(chrome, "privacy", { services: settings(), network: settings(), websites: settings() });
})();
