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

  // Side panels open as their own window, via a tab at the panel page.
  let panelBehavior = { openPanelOnActionClick: false };
  let panelOptions = {};
  const panelPath = () => {
    const manifest = chrome.runtime && chrome.runtime.getManifest ? chrome.runtime.getManifest() : {};
    return panelOptions.path || (manifest.side_panel && manifest.side_panel.default_path) || "";
  };
  define(chrome, "sidePanel", {
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

  // Chrome keeps the service worker alive while an extension page has it open; WebKit unloads it
  // after 30 idle seconds, dropping its in-memory state and leaving the page's ports unanswered.
  // A message every 20 s counts as activity and keeps it loaded.
  if (typeof window !== "undefined" && g === window && chrome.runtime && chrome.runtime.sendMessage) {
    let isBackground = false;
    try {
      isBackground = chrome.extension && chrome.extension.getBackgroundPage && chrome.extension.getBackgroundPage() === window;
    } catch (_) {}
    if (!isBackground) {
      setInterval(() => {
        try {
          const sent = chrome.runtime.sendMessage({ nxKeepAlive: true });
          if (sent && sent.catch) sent.catch(() => {});
        } catch (_) {}
      }, 20000);
    }
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
