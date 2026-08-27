import assert from "node:assert/strict";
import test from "node:test";

import { canLaunch, launchCommand } from "./uapi-launch-policy.ts";

test("official mode can launch when already authenticated", () => {
  assert.equal(
    canLaunch({
      connectionMode: "official",
      configured: false,
      uapiReady: false,
      officialAuthenticated: true,
    }),
    true,
  );
});

test("official mode can launch before native login", () => {
  assert.equal(
    canLaunch({
      connectionMode: "official",
      configured: false,
      uapiReady: false,
      officialAuthenticated: false,
    }),
    true,
  );
});

test("official mode can reach native login when credential storage is unavailable", () => {
  assert.equal(
    canLaunch({
      connectionMode: "official",
      configured: false,
      uapiReady: false,
      officialAuthenticated: false,
      credentialStoreAvailable: false,
    }),
    true,
  );
});

test("default U-API mode still requires a complete configuration", () => {
  assert.equal(
    canLaunch({
      connectionMode: "uapi",
      configured: false,
      uapiReady: false,
      officialAuthenticated: false,
    }),
    false,
  );
  assert.equal(
    canLaunch({
      connectionMode: "uapi",
      configured: true,
      uapiReady: true,
      officialAuthenticated: false,
      credentialStoreAvailable: false,
    }),
    true,
  );
});

test("U-API mode can launch when cached credentials can repair live config", () => {
  assert.equal(
    canLaunch({
      connectionMode: "uapi",
      configured: false,
      uapiReady: true,
      officialAuthenticated: false,
    }),
    true,
  );
});

test("pending configuration changes turn the next explicit launch into a restart", () => {
  assert.equal(launchCommand(false), "launch_codex_plus");
  assert.equal(launchCommand(true), "restart_codex_plus");
});
