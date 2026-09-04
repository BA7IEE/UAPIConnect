const assert = require("node:assert/strict");
const vm = require("node:vm");
const { randomUUID } = require("node:crypto");
const copy = (value) => JSON.parse(JSON.stringify(value));

function storage(values = new Map(), blocked = false) {
  return {
    getItem(key) { if (blocked) throw new Error("blocked"); return values.get(key) ?? null; },
    setItem(key, value) { if (blocked) throw new Error("blocked"); values.set(key, value); },
  };
}
function client() {
  const original = { value: { enable_i18n: false, untouched: "keep" }, get(key, fallback) { return this.value[key] ?? fallback; } };
  return {
    original,
    getLayer() { assert.equal(this.original, original); return original; },
    getDynamicConfig() { return original; },
    checkGate() { return false; },
    getFeatureGate() { return { value: false, ruleID: "keep" }; },
  };
}
function harness(options = {}) {
  const listeners = new Set();
  const settings = options.settings ?? {
    localeOverride: "zh-CN",
    "enabled-reasoning-efforts": ["low", "medium", "high", "xhigh"],
    "show-ultra-in-model-picker-slider": false,
    unrelated: "keep",
  };
  const writes = [];
  let reloads = 0;
  const context = {
    crypto: { randomUUID },
    document: { readyState: options.readyState ?? "loading" },
    localStorage: options.storage ?? storage(),
    sessionStorage: options.sessionStorage ?? storage(),
    location: { href: options.url ?? "app://-/index.html", reload() { reloads++; } },
    setTimeout, clearTimeout,
    addEventListener(type, listener) { if (type === "message") listeners.add(listener); },
    removeEventListener(type, listener) { if (type === "message") listeners.delete(listener); },
    electronBridge: {
      async sendMessageFromView(message) {
        const { params } = JSON.parse(message.body);
        const failure = options.failKey === params.key;
        if (!failure && message.url.endsWith("/set-setting")) {
          if (options.ignoreWrites !== true) settings[params.key] = params.value;
          writes.push(copy(params));
        }
        const response = {
          type: "fetch-response", requestId: message.requestId,
          responseType: failure ? "error" : "success",
          bodyJsonString: JSON.stringify({ value: settings[params.key] ?? null }),
        };
        queueMicrotask(() => [...listeners].forEach((listener) => listener({ data: response })));
      },
    },
    __UAPI_DESKTOP_COMPAT_CONFIG__: options.config ?? { forceChinese: true, reasoningEfforts: ["max", "ultra"] },
  };
  context.window = context.self = context;
  context.top = options.iframe ? {} : context;
  if (options.client) context.__STATSIG__ = { firstInstance: options.client };
  vm.createContext(context);
  const run = () => vm.runInContext(source, context);
  const settle = async () => {
    for (let attempt = 0; attempt < 20; attempt++) await new Promise(setImmediate);
    assert.equal(listeners.size, 0, "setting RPC listeners must be cleaned up");
  };
  return { context, settings, writes, run, settle, reloads: () => reloads };
}

(async () => {
  // 9 月新版：localeOverride 已正确，翻译通过 getLayer 而不是 getDynamicConfig 读取。
  const c = client();
  const h = harness({ client: c, readyState: "complete" });
  h.run(); await h.settle();
  assert.equal(c.getLayer("72216192").get("enable_i18n", false), true);
  assert.equal(c.getDynamicConfig("72216192").get("locale_source", "IDE"), "SYSTEM");
  assert.equal(c.getLayer("other"), c.original);
  assert.equal(c.original.value.enable_i18n, false, "never mutate the SDK's cached result");
  assert.equal(c.checkGate("1186680773"), true);
  assert.equal(c.checkGate("unrelated-entitlement"), false);
  assert.deepEqual(copy(c.getFeatureGate("1186680773")), { value: true, ruleID: "keep" });
  assert.deepEqual(h.settings["enabled-reasoning-efforts"], ["low", "medium", "high", "xhigh", "max", "ultra"]);
  assert.equal(h.settings["show-ultra-in-model-picker-slider"], true);
  assert.equal(h.settings.unrelated, "keep");
  assert.equal(h.context.__UAPI_DESKTOP_COMPATIBILITY__.reasoning, "ready");
  assert.equal(h.reloads(), 1);
  h.run(); await h.settle(); assert.equal(h.reloads(), 1, "reinjection must not loop");

  // 切官方模式时恢复本兼容层增加的设置，不把 Ultra 的显示补丁带入官方权限。
  h.context.__UAPI_DESKTOP_COMPAT_CONFIG__ = { forceChinese: true, reasoningEfforts: [] };
  h.run(); await h.settle();
  assert.deepEqual(h.settings["enabled-reasoning-efforts"], ["low", "medium", "high", "xhigh"]);
  assert.equal(h.settings["show-ultra-in-model-picker-slider"], false);
  assert.equal(c.checkGate("1186680773"), false);

  // Statsig 在脚本之后创建，也必须在首轮读取之前完成补丁。
  const late = harness(); late.run();
  late.context.__STATSIG__ = {};
  const lateClient = client();
  delete lateClient.getDynamicConfig;
  late.context.__STATSIG__.firstInstance = lateClient;
  assert.equal(lateClient.getLayer("72216192").get("enable_i18n"), true);
  await late.settle();

  // Luna 没有 Ultra；无关模型没有任何新增推理档位。
  for (const efforts of [["max"], []]) {
    const c = client(), h = harness({ client: c, config: { forceChinese: false, reasoningEfforts: efforts } });
    h.run(); await h.settle();
    assert.equal(c.checkGate("1186680773"), false);
    assert.equal(c.getLayer("72216192"), c.original);
    assert.equal(h.settings["show-ultra-in-model-picker-slider"], false);
    assert.equal(h.settings["enabled-reasoning-efforts"].includes("ultra"), false);
    assert.equal(h.settings["enabled-reasoning-efforts"].includes("max"), efforts.length > 0);
  }

  // 用户修改后的值不可在退出 U-API 时被旧快照覆盖。
  const manual = harness({ settings: { localeOverride: "en-US", "enabled-reasoning-efforts": ["medium"], "show-ultra-in-model-picker-slider": false } });
  manual.run(); await manual.settle();
  manual.settings["enabled-reasoning-efforts"] = ["high"];
  manual.settings.localeOverride = "ja-JP";
  manual.context.__UAPI_DESKTOP_COMPAT_CONFIG__ = { forceChinese: false, reasoningEfforts: [] };
  manual.run(); await manual.settle();
  assert.deepEqual(manual.settings["enabled-reasoning-efforts"], ["high"]);
  assert.equal(manual.settings.localeOverride, "ja-JP");
  assert.equal(manual.settings["show-ultra-in-model-picker-slider"], false);

  for (const badStorage of [storage(new Map(), true), storage(new Map([["uapiConnect.desktopCompatibility.managed.v1", "broken"]]))]) {
    const h = harness({ storage: badStorage }); h.run(); await h.settle();
    assert.deepEqual(h.writes, []);
    assert.equal(h.reloads(), 0);
    assert.equal(h.context.__UAPI_DESKTOP_COMPATIBILITY__.locale, "ownership-unavailable");
  }
  const failedOptions = { failKey: "enabled-reasoning-efforts", client: client() };
  const failed = harness(failedOptions);
  failed.run(); await failed.settle();
  assert.equal(failed.context.__UAPI_DESKTOP_COMPATIBILITY__.reasoning, "setting-unavailable");
  assert.deepEqual(failed.writes, []);
  delete failedOptions.failKey;
  failed.run(); await failed.settle();
  assert.equal(failed.context.__UAPI_DESKTOP_COMPATIBILITY__.reasoning, "ready", "retry a transient setting failure");

  const ignored = harness({ ignoreWrites: true }); ignored.run(); await ignored.settle();
  assert.equal(ignored.context.__UAPI_DESKTOP_COMPATIBILITY__.reasoning, "setting-unavailable");
  const frozen = harness({ client: Object.freeze(client()) }); frozen.run(); await frozen.settle();
  assert.notEqual(frozen.context.__UAPI_DESKTOP_COMPATIBILITY__.gates, "ready");

  const unavailableSession = harness({ sessionStorage: storage(new Map(), true), readyState: "complete", client: client() });
  unavailableSession.run(); await unavailableSession.settle();
  assert.equal(unavailableSession.reloads(), 0);
  for (const options of [{ iframe: true }, { url: "https://example.test" }]) {
    const h = harness(options); h.run(); await h.settle();
    assert.deepEqual(h.writes, []);
    assert.equal(h.context.__UAPI_DESKTOP_COMPATIBILITY__, undefined);
  }
  console.log("desktop compatibility: locale, reasoning, delayed startup, restoration and isolation passed");
})().catch((error) => { console.error(error); process.exitCode = 1; });
