import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { MANAGER_ACTIVATION_POLL_MS, handleManagerActivation } from "./uapi-manager-activation.ts";

test("configure activation enters connection settings before refreshing local state", async () => {
  const calls: string[] = [];

  const handled = await handleManagerActivation(
    "configure",
    () => calls.push("connection"),
    async () => {
      calls.push("refresh-state");
    },
  );

  assert.equal(handled, true);
  assert.deepEqual(calls, ["connection", "refresh-state"]);
});

test("non-configure activation does not change route or refresh", async () => {
  const calls: string[] = [];

  const handled = await handleManagerActivation(
    null,
    () => calls.push("connection"),
    async () => {
      calls.push("refresh-state");
    },
  );

  assert.equal(handled, false);
  assert.deepEqual(calls, []);
});

test("activation polling reacts in well under one second", () => {
  assert.ok(MANAGER_ACTIVATION_POLL_MS > 0);
  assert.ok(MANAGER_ACTIVATION_POLL_MS < 1_000);
});

test("activation refresh reads local state without refreshing network models", () => {
  const source = readFileSync(new URL("./uapi/UapiApp.tsx", import.meta.url), "utf8");
  const refreshStart = source.indexOf("const refreshState = useCallback");
  const refreshEnd = source.indexOf("const run = useCallback", refreshStart);
  const activationFlow = source.slice(refreshStart, refreshEnd);

  assert.ok(refreshStart >= 0 && refreshEnd > refreshStart);
  assert.match(activationFlow, /invoke<OverviewResult>\("load_overview"\)/);
  assert.match(activationFlow, /invoke<CommandResult<UapiStatus>>\("uapi_status"\)/);
  assert.match(activationFlow, /invoke<VersionResult>\("backend_version"\)/);
  assert.match(activationFlow, /invoke<unknown>\("uapi_take_manager_activation"\)/);
  assert.match(activationFlow, /handleManagerActivation\([\s\S]*?refreshState/);
  assert.doesNotMatch(activationFlow, /uapi_refresh_models/);
});

test("critical U-API status is not discarded when auxiliary reads fail", () => {
  const source = readFileSync(new URL("./uapi/UapiApp.tsx", import.meta.url), "utf8");
  const refreshStart = source.indexOf("const refreshState = useCallback");
  const refreshEnd = source.indexOf("const run = useCallback", refreshStart);
  const refreshFlow = source.slice(refreshStart, refreshEnd);

  const auxiliaryStart = refreshFlow.indexOf("Promise.allSettled");
  const statusRead = refreshFlow.indexOf("await invoke<CommandResult<UapiStatus>>");
  const auxiliaryRead = refreshFlow.indexOf("await auxiliaryResults");

  assert.ok(auxiliaryStart >= 0);
  assert.ok(statusRead > auxiliaryStart);
  assert.ok(auxiliaryRead > statusRead);
  assert.match(refreshFlow, /setStatus\(statusResult\)/);
});

test("loading and in-flight mutations cannot be mistaken for editable ready state", () => {
  const source = readFileSync(new URL("./uapi/UapiApp.tsx", import.meta.url), "utf8");

  assert.match(source, /disabled=\{!status \|\| busy !== null\}/);
  assert.match(source, /statusLoading \? "正在检查本地状态"/);
  assert.match(source, /<Input[\s\S]*?disabled=\{busy !== null\}/);
  assert.match(source, /if \(result\.status === "ok"\) \{\s*setDiscovery\(result\);/);
  assert.match(source, /const refreshAfterMutation = useCallback[\s\S]*?catch \(error\)/);
  assert.match(source, /void refreshState\(\)\.catch\(\(error\) =>/);
  assert.match(source, /setStatusLoadFailed\(true\)/);
  assert.match(source, /statusUnavailable[\s\S]*?状态读取失败/);
  assert.match(source, /aria-current=\{route === item\.id \? "page" : undefined\}/);
  assert.match(source, /role=\{notice\.kind === "error" \? "alert" : "status"\}/);
  assert.match(source, /invoke\("open_external_url"[\s\S]*?\.catch\(\(error\) =>/);
});

test("mutation busy state covers the command and its trailing state refresh", () => {
  const source = readFileSync(new URL("./uapi/UapiApp.tsx", import.meta.url), "utf8");

  for (const action of ["configure", "refresh", "switchMode", "repair"]) {
    const start = source.indexOf(`await run("${action}", async () => {`);
    assert.ok(start >= 0, `${action} should keep its full transaction inside run()`);
  }
  assert.match(source, /await run\("configure", async \(\) => \{[\s\S]*?await refreshAfterMutation/);
  assert.match(source, /await run\("refresh", async \(\) => \{[\s\S]*?await refreshAfterMutation/);
  assert.match(source, /await run\("switchMode", async \(\) => \{[\s\S]*?await refreshLatestState/);
  assert.match(source, /await run\("repair", async \(\) => \{[\s\S]*?await refreshAfterMutation/);
  assert.match(source, /const generation = \+\+refreshGeneration\.current/);
  assert.match(source, /if \(isLatest\(\)\) \{[\s\S]*?setStatus\(statusResult\)/);
  assert.match(source, /class StaleRefreshError extends Error/);
  assert.match(source, /const refreshLatestState = useCallback/);
  assert.ok((source.match(/error instanceof StaleRefreshError/g) ?? []).length >= 3);
  assert.match(source, /const interactionGeneration = useRef\(0\)/);
  assert.match(source, /interactionGeneration\.current !== launchInteraction/);
});
