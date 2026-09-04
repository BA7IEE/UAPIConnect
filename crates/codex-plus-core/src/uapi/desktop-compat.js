(() => {
  // 只接触 Codex 主页面，不进入内嵌浏览器，不加载任何额外 UI 或用户脚本。
  if (window.top !== window || window.self !== window || !window.electronBridge
    || !/^app:\/\/\-\//i.test(window.location.href)) return;
  const config = window.__UAPI_DESKTOP_COMPAT_CONFIG__;
  if (!config) return;
  const revision = `1:${JSON.stringify(config)}`;
  const previousState = window.__UAPI_DESKTOP_COMPATIBILITY__;
  if (previousState?.revision === revision
    && (previousState.locale === "pending" || previousState.reasoning === "pending"
      || (["ready", "off"].includes(previousState.locale)
        && ["ready", "off"].includes(previousState.reasoning) && previousState.gates === "ready"))) return;
  const state = { revision, locale: "pending", reasoning: "pending" };
  window.__UAPI_DESKTOP_COMPATIBILITY__ = state;
  const wantsUltra = config.reasoningEfforts.includes("ultra");
  let localeGateReady = !config.forceChinese;
  let ultraGateReady = !wantsUltra;
  const ownershipKey = "uapiConnect.desktopCompatibility.managed.v1";
  const reloadKey = "uapiConnect.desktopCompatibility.reload.v1";
  let patchedClient = false;

  function reloadOnce() {
    // 存储不可用时不自动刷新，防止无法记住标记而形成刷新循环。
    try {
      if (window.sessionStorage.getItem(reloadKey) === revision) return;
      window.sessionStorage.setItem(reloadKey, revision);
    } catch { return; }
    window.location.reload();
  }

  function patchClient(client) {
    if (!client || typeof client !== "object") return;
    // 新版从 getLayer 读取翻译开关，旧版从 getDynamicConfig 读取。
    for (const method of ["getLayer", "getDynamicConfig"]) {
      const original = client[method];
      if (typeof original !== "function") continue;
      if (original.__uapiCompatibility) { localeGateReady = true; continue; }
      const wrapped = function (name, ...args) {
        const result = original.call(this, name, ...args);
        if (!window.__UAPI_DESKTOP_COMPAT_CONFIG__?.forceChinese || name !== "72216192" || !result) return result;
        return {
          ...result,
          value: { ...result.value, enable_i18n: true, locale_source: "SYSTEM" },
          get(key, fallback) {
            if (key === "enable_i18n") return true;
            if (key === "locale_source") return "SYSTEM";
            return typeof result.get === "function" ? result.get(key, fallback) : result.value?.[key] ?? fallback;
          },
        };
      };
      wrapped.__uapiCompatibility = true;
      client[method] = wrapped;
      if (client[method] === wrapped) { localeGateReady = true; patchedClient = true; }
    }
    // 只允许目录已经声明的 Ultra 通过显示过滤；不修改模型能力或官方订阅权限。
    for (const method of ["checkGate", "getFeatureGate"]) {
      const original = client[method];
      if (typeof original !== "function") continue;
      if (original.__uapiCompatibility) { ultraGateReady = true; continue; }
      const wrapped = function (name, ...args) {
        const result = original.call(this, name, ...args);
        if (!window.__UAPI_DESKTOP_COMPAT_CONFIG__?.reasoningEfforts.includes("ultra") || name !== "1186680773") return result;
        return method === "checkGate" ? true : { ...result, value: true };
      };
      wrapped.__uapiCompatibility = true;
      client[method] = wrapped;
      if (client[method] === wrapped) { ultraGateReady = true; patchedClient = true; }
    }
    state.gates = localeGateReady && ultraGateReady ? "ready" : "pending";
  }

  function patchRoot(root) {
    if (!root || typeof root !== "object") return;
    for (const key of ["firstInstance", "instance"]) {
      let current = root[key];
      patchClient(typeof current === "function" ? current.call(root) : current);
      const descriptor = Object.getOwnPropertyDescriptor(root, key);
      if (descriptor?.configurable === false || descriptor?.get?.__uapiCompatibility === revision) continue;
      const getter = () => current;
      getter.__uapiCompatibility = revision;
      Object.defineProperty(root, key, {
        configurable: true, enumerable: descriptor?.enumerable ?? true, get: getter,
        set(next) {
          current = next;
          try { patchClient(typeof next === "function" ? next.call(root) : next); }
          catch { state.gates = "unavailable"; }
        },
      });
    }
    for (const client of Object.values(root.instances || {})) patchClient(client);
  }

  // 在首轮渲染前拦住延迟创建的 Statsig，避免 React 缓存未开启的翻译开关。
  try {
    let root = window.__STATSIG__;
    patchRoot(root);
    if (Object.getOwnPropertyDescriptor(window, "__STATSIG__")?.configurable !== false) {
      Object.defineProperty(window, "__STATSIG__", {
        configurable: true, get: () => root,
        set(next) {
          root = next;
          try { patchRoot(next); } catch { state.gates = "unavailable"; }
        },
      });
    }
  } catch { state.gates = "unavailable"; }

  function setting(method, params) {
    return new Promise((resolve, reject) => {
      const requestId = `uapi-compat-${crypto.randomUUID()}`;
      const cleanup = () => {
        window.clearTimeout(timer);
        window.removeEventListener("message", onMessage);
      };
      const onMessage = (event) => {
        if (event.source && event.source !== window) return;
        const message = event.data;
        if (message?.type !== "fetch-response" || message.requestId !== requestId) return;
        cleanup();
        if (message.responseType !== "success") { reject(new Error("setting-unavailable")); return; }
        try { resolve(JSON.parse(message.bodyJsonString || "null")); } catch { reject(new Error("invalid-setting-response")); }
      };
      const timer = window.setTimeout(() => { cleanup(); reject(new Error("setting-timeout")); }, 5000);
      window.addEventListener("message", onMessage);
      Promise.resolve().then(() => window.electronBridge.sendMessageFromView({
        type: "fetch", requestId, method: "POST", url: `vscode://codex/${method}`,
        body: JSON.stringify({ params }),
      })).catch(() => { cleanup(); reject(new Error("setting-unavailable")); });
    });
  }

  const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
  async function writeSetting(key, value) {
    await setting("set-setting", { key, value });
    const actual = (await setting("get-setting", { key }))?.value ?? null;
    if (!equal(actual, value)) throw new Error("setting-not-applied");
  }
  async function syncSetting(key, desired, owned) {
    const current = (await setting("get-setting", { key }))?.value ?? null;
    const previous = owned[key];
    if (previous !== undefined && (!previous || typeof previous !== "object"
      || !Object.hasOwn(previous, "before") || !Object.hasOwn(previous, "applied"))) throw new Error("invalid-ownership");
    const next = desired(current);
    if (next === undefined) {
      if (!previous) return false;
      if (equal(current, previous.applied)) {
        await writeSetting(key, previous.before);
      }
      delete owned[key];
      window.localStorage.setItem(ownershipKey, JSON.stringify(owned));
      return equal(current, previous.applied);
    }
    if (equal(current, next)) return false;
    // 用户在两次启动间改过该项时，用新值作为可恢复的基线。
    owned[key] = { before: previous && equal(current, previous.applied) ? previous.before : current, applied: next };
    window.localStorage.setItem(ownershipKey, JSON.stringify(owned));
    await writeSetting(key, next);
    return true;
  }

  async function synchronize() {
    let owned;
    try {
      owned = JSON.parse(window.localStorage.getItem(ownershipKey) || "{}");
      if (!owned || typeof owned !== "object" || Array.isArray(owned)) throw new Error();
    } catch { state.locale = state.reasoning = "ownership-unavailable"; return; }
    let changed = false;
    try {
      if (config.forceChinese || Object.hasOwn(owned, "localeOverride")) {
        changed = await syncSetting("localeOverride", () => config.forceChinese ? "zh-CN" : undefined, owned) || changed;
      }
      state.locale = config.forceChinese ? "ready" : "off";
    } catch { state.locale = "setting-unavailable"; }
    try {
      if (config.reasoningEfforts.length || Object.hasOwn(owned, "enabled-reasoning-efforts")) {
        changed = await syncSetting("enabled-reasoning-efforts", (current) => {
          if (!config.reasoningEfforts.length) return undefined;
          if (!Array.isArray(current) || !current.every((value) => typeof value === "string")) throw new Error();
          return [...new Set([...current, ...config.reasoningEfforts])];
        }, owned) || changed;
      }
      if (wantsUltra || Object.hasOwn(owned, "show-ultra-in-model-picker-slider")) {
        changed = await syncSetting("show-ultra-in-model-picker-slider", () => wantsUltra ? true : undefined, owned) || changed;
      }
      state.reasoning = config.reasoningEfforts.length ? "ready" : "off";
    } catch { state.reasoning = "setting-unavailable"; }
    if (changed || (patchedClient && document.readyState !== "loading")) reloadOnce();
  }
  state.gates ??= localeGateReady && ultraGateReady ? "ready" : "pending";
  window.__UAPI_DESKTOP_COMPAT_READY__ = synchronize().catch(() => {
    state.locale = state.reasoning = "unavailable";
  });
})();
