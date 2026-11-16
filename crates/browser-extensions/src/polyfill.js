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
  // WebKit's namespace objects take no new properties, so members it lacks come from a wrapper that
  // `patch_pages` points the extension's calls at; everything WebKit has passes through.
  // `overrides` replace members WebKit has but gets wrong.
  const extend = (target, extras, overrides = {}) =>
    new Proxy(target, {
      get: (t, key) => {
        if (Object.prototype.hasOwnProperty.call(overrides, key)) return overrides[key];
        const value = t[key];
        if (value === undefined && Object.prototype.hasOwnProperty.call(extras, key)) return extras[key];
        return typeof value === "function" ? value.bind(t) : value;
      },
    });

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
  // `patch_pages` turns the extension's `chrome-extension://` into WebKit's scheme so it recognizes its own pages,
  // but servers know an extension by its Chrome URL: an OAuth `redirect_uri` that says `webkit-extension://` is
  // refused ("not supported by client", Claude's sign-in). So what leaves for the web says `chrome-extension://` again.
  const id = chrome.runtime && chrome.runtime.id;
  const outward = (text) => {
    if (!id || typeof text !== "string") return text;
    const [ours, chromes] = [`webkit-extension://${id}`, `chrome-extension://${id}`];
    return text.split(ours).join(chromes).split(encodeURIComponent(ours)).join(encodeURIComponent(chromes));
  };
  const isWeb = (url) => /^https?:/i.test(String(url));
  const outwardUrl = (props) => (props && typeof props === "object" && isWeb(props.url) ? { ...props, url: outward(props.url) } : props);
  if (typeof g.fetch === "function") {
    const fetch = g.fetch.bind(g);
    g.fetch = (input, init) => {
      if (typeof input !== "string" && !(input instanceof URL)) return fetch(input, init);
      if (!isWeb(input)) return fetch(input, init);
      const body = init && init.body;
      if (typeof body === "string") init = { ...init, body: outward(body) };
      else if (body instanceof URLSearchParams) init = { ...init, body: new URLSearchParams(outward(body.toString())) };
      return fetch(outward(String(input)), init);
    };
  }

  const tabs = chrome["tabs"];
  if (tabs) {
    const overrides = {
      create: (props, ...rest) => tabs.create(outwardUrl(props), ...rest),
      update: (...args) => tabs.update(...args.map(outwardUrl)),
    };
    g.nxTabs = extend(tabs, {
      group: unsupported("chrome.tabs.group"),
      ungroup: method(() => undefined),
      // Pages here are suspended by the browser on its own; a discard request leaves the tab as it is.
      discard: method((tabId) => (tabId === undefined ? undefined : tabs.get(tabId))),
      // One selected tab at a time: highlighting activates the first tab named.
      highlight: method(async (info) => {
        const index = [].concat(info.tabs)[0];
        const list = await tabs.query(info.windowId === undefined ? { currentWindow: true } : { windowId: info.windowId });
        const tab = list.find((candidate) => candidate.index === index);
        if (!tab) throw new Error(`No tab at index: ${index}.`);
        await tabs.update(tab.id, { active: true });
        return chrome.windows ? chrome.windows.get(tab.windowId, { populate: true }) : undefined;
      }),
      getZoomSettings: method(() => ({ mode: "automatic", scope: "per-origin", defaultZoomFactor: 1 })),
      setZoomSettings: method(() => undefined),
      onZoomChange: event(),
      ZoomSettingsMode: names,
      ZoomSettingsScope: names,
    }, overrides);
  }

  // Runtime members WebKit lacks; extensions add listeners to these unguarded at startup.
  const runtime = chrome["runtime"];
  if (runtime) {
    // Chrome's message senders carry `origin`, which WebKit's leave out; Claude's sign-in trusts a message from
    // claude.ai only by it.
    const withOrigin = (sender) => {
      if (!sender || sender.origin !== undefined || !sender.url) return sender;
      try {
        const url = new URL(sender.url);
        return { ...sender, origin: `${url.protocol}//${url.host}` };
      } catch (_) {
        return sender;
      }
    };
    const messages = (target) => {
      const wrapped = new WeakMap();
      const wrap = (fn) => {
        if (typeof fn !== "function") return fn;
        if (!wrapped.has(fn)) wrapped.set(fn, (message, sender, sendResponse) => fn(message, withOrigin(sender), sendResponse));
        return wrapped.get(fn);
      };
      return {
        addListener: (fn) => target.addListener(wrap(fn)),
        removeListener: (fn) => target.removeListener(wrapped.get(fn) || fn),
        hasListener: (fn) => target.hasListener(wrapped.get(fn) || fn),
        hasListeners: () => (typeof target.hasListeners === "function" ? target.hasListeners() : false),
      };
    };
    const runtimeOverrides = {};
    for (const name of ["onMessage", "onMessageExternal"]) {
      if (runtime[name]) runtimeOverrides[name] = messages(runtime[name]);
    }
    g.nxRuntime = extend(runtime, {
      onSuspend: event(),
      onSuspendCanceled: event(),
      onUpdateAvailable: event(),
      onRestartRequired: event(),
      onUserScriptMessage: event(),
      getContexts: method(() => []),
      requestUpdateCheck: method(() => ({ status: "no_update" })),
      restart: method(() => undefined),
      restartAfterDelay: method(() => undefined),
      ContextType: names,
      OnRestartRequiredReason: names,
      RequestUpdateCheckStatus: names,
    }, runtimeOverrides);
  }

  // Rule enums WebKit lacks, with Chrome's values (not their names), which WebKit's rules accept.
  // WebKit also rejects a whole rule update over one header it doesn't know (Claude's `anthropic-client-platform`),
  // where Chrome modifies any header; such headers are dropped and the update retried, so the other rules apply.
  const declarativeNetRequest = chrome["declarativeNetRequest"];
  if (declarativeNetRequest) {
    const withoutHeader = (options, name) => {
      const keep = (headers) => headers && headers.filter((entry) => String(entry.header).toLowerCase() !== name);
      const addRules = (options.addRules || [])
        .map((rule) => {
          const action = rule.action || {};
          if (action.type !== "modifyHeaders") return rule;
          return { ...rule, action: { ...action, requestHeaders: keep(action.requestHeaders), responseHeaders: keep(action.responseHeaders) } };
        })
        .filter((rule) => rule.action.type !== "modifyHeaders" || (rule.action.requestHeaders || []).length + (rule.action.responseHeaders || []).length > 0);
      return { ...options, addRules };
    };
    const tolerant = (name) =>
      method(async (options) => {
        for (;;) {
          try {
            return await declarativeNetRequest[name](options);
          } catch (error) {
            const header = /header `([^`]+)` is not recognized/.exec(String(error && error.message));
            if (!header || !options || !options.addRules) throw error;
            const next = withoutHeader(options, header[1].toLowerCase());
            if (JSON.stringify(next) === JSON.stringify(options)) throw error;
            options = next;
          }
        }
      });
    g.nxDeclarativeNetRequest = extend(declarativeNetRequest, {
      RuleActionType: { BLOCK: "block", REDIRECT: "redirect", ALLOW: "allow", UPGRADE_SCHEME: "upgradeScheme", MODIFY_HEADERS: "modifyHeaders", ALLOW_ALL_REQUESTS: "allowAllRequests" },
      HeaderOperation: { APPEND: "append", SET: "set", REMOVE: "remove" },
      DomainType: { FIRST_PARTY: "firstParty", THIRD_PARTY: "thirdParty" },
      ResourceType: Object.fromEntries(
        ["main_frame", "sub_frame", "stylesheet", "script", "image", "font", "object", "xmlhttprequest", "ping", "csp_report", "media", "websocket", "webtransport", "webbundle", "other"].map((type) => [type.toUpperCase(), type])
      ),
      RequestMethod: { CONNECT: "connect", DELETE: "delete", GET: "get", HEAD: "head", OPTIONS: "options", PATCH: "patch", POST: "post", PUT: "put", OTHER: "other" },
    }, { updateSessionRules: tolerant("updateSessionRules"), updateDynamicRules: tolerant("updateDynamicRules") });
  }

  const selfInfo = () => {
    const optionsPage = manifest.options_page || (manifest.options_ui && manifest.options_ui.page);
    const icons = Object.entries(manifest.icons || {}).map(([size, path]) => ({ size: Number(size), url: chrome.runtime.getURL(path) }));
    return {
      id: chrome.runtime.id,
      name: manifest.name || "",
      shortName: manifest.short_name || manifest.name || "",
      description: manifest.description || "",
      version: manifest.version || "",
      versionName: manifest.version_name,
      mayDisable: true,
      enabled: true,
      installType: "normal",
      type: "extension",
      offlineEnabled: !!manifest.offline_enabled,
      optionsUrl: optionsPage ? chrome.runtime.getURL(optionsPage) : "",
      homepageUrl: manifest.homepage_url,
      permissions: (manifest.permissions || []).filter((permission) => !permission.includes("://")),
      hostPermissions: manifest.host_permissions || (manifest.permissions || []).filter((permission) => permission.includes("://")),
      icons,
    };
  };
  define(chrome, "management", {
    getSelf: method(selfInfo),
    // Other extensions stay hidden, as in Chrome without the `management` permission.
    getAll: method(() => [selfInfo()]),
    get: method((id) => {
      if (id !== chrome.runtime.id) throw new Error(`Failed to find extension with id ${id}.`);
      return selfInfo();
    }),
    getPermissionWarningsById: method(() => []),
    getPermissionWarningsByManifest: method(() => []),
    setEnabled: unsupported("chrome.management.setEnabled"),
    uninstall: unsupported("chrome.management.uninstall"),
    uninstallSelf: unsupported("chrome.management.uninstallSelf"),
    launchApp: unsupported("chrome.management.launchApp"),
    onInstalled: event(),
    onUninstalled: event(),
    onEnabled: event(),
    onDisabled: event(),
    ExtensionInstallType: names,
    ExtensionType: names,
    ExtensionDisabledReason: names,
    LaunchType: names,
  });

  // shortcut: always "active", so lock-on-idle and lock-on-screen-lock never fire; wire to the engine if a password manager needs it.
  define(chrome, "idle", {
    queryState: method(() => "active"),
    setDetectionInterval: method(() => undefined),
    getAutoLockDelay: method(() => 0),
    onStateChanged: event(),
    IdleState: names,
  });

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

  // WebKit has `setAccessLevel` on the session area only; Chrome has it on every area, and Kluff's background
  // calls it on `local` at startup, so its absence stops the background. WebKit can't restrict the other areas
  // to trusted contexts, so there it does nothing. `patch_pages` points the extension's storage calls here.
  const storage = chrome["storage"];
  if (storage) {
    const bound = (target, key) => {
      const value = target[key];
      return typeof value === "function" ? value.bind(target) : value;
    };
    const areas = new WeakMap();
    const area = (target) => {
      if (!areas.has(target)) {
        const setAccessLevel = method(() => undefined);
        areas.set(target, new Proxy(target, { get: (t, key) => (key === "setAccessLevel" && typeof t[key] !== "function" ? setAccessLevel : bound(t, key)) }));
      }
      return areas.get(target);
    };
    // No policies are set here, which Chrome reports as an empty managed area; WebKit has no such area.
    const managed = {
      get: method(() => ({})),
      getBytesInUse: method(() => 0),
      onChanged: event(),
    };
    g.nxStorage = new Proxy(storage, {
      get: (target, key) => {
        const value = target[key];
        if (key === "managed" && value === undefined) return managed;
        return value && typeof value === "object" && typeof value.get === "function" && typeof value.set === "function" ? area(value) : bound(target, key);
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
  // Registering a script identical to one already registered keeps it too, where Chrome throws "Duplicate
  // ID": Bitwarden re-registers its passkey scripts on every start without awaiting, so that only ever logs.
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
          if (existing && same(existing, script)) {
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
