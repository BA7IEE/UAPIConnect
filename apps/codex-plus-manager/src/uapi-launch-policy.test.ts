import assert from "node:assert/strict";
import test from "node:test";

import { canLaunch } from "./uapi-launch-policy.ts";

test("official mode can launch when already authenticated", () => {
  assert.equal(
    canLaunch({
      connectionMode: "official",
      configured: false,
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
      officialAuthenticated: false,
    }),
    false,
  );
  assert.equal(
    canLaunch({
      connectionMode: "uapi",
      configured: true,
      officialAuthenticated: false,
      credentialStoreAvailable: false,
    }),
    true,
  );
});
