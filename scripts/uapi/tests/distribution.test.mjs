import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const manifest = JSON.parse(
  readFileSync(new URL("../../../distribution/uapi-connect.json", import.meta.url), "utf8"),
);

const rustPolicy = readFileSync(
  new URL("../../../crates/codex-plus-core/src/distribution.rs", import.meta.url),
  "utf8",
);
const managedIntegration = readFileSync(
  new URL("../../../crates/codex-plus-core/src/uapi.rs", import.meta.url),
  "utf8",
);
const managerEntry = readFileSync(
  new URL("../../../apps/codex-plus-manager/src/main.tsx", import.meta.url),
  "utf8",
);

function rustString(name) {
  const match = rustPolicy.match(new RegExp(`pub const ${name}: &str = "([^"]*)";`));
  assert.ok(match, `missing Rust distribution constant ${name}`);
  return match[1];
}

function rustBool(name) {
  const match = rustPolicy.match(new RegExp(`pub const ${name}: bool = (true|false);`));
  assert.ok(match, `missing Rust distribution constant ${name}`);
  return match[1] === "true";
}

test("distribution fixes one NewAPI endpoint without credentials", () => {
  assert.equal(manifest.fixedBaseUrl, "https://token.u-studio.cn/v1");
  assert.equal(manifest.fixedProviderId, "uapi_connect");
  assert.equal(manifest.features.fixedProviderEdition, true);
  assert.equal(manifest.features.adsEnabled, false);
  assert.equal(manifest.features.updatesEnabled, false);
  assert.doesNotMatch(JSON.stringify(manifest), /sk-[A-Za-z0-9_-]{16,}/);
});

test("Rust policy mirrors the public manifest", () => {
  assert.equal(rustString("PRODUCT_NAME"), manifest.productName);
  assert.equal(rustString("FIXED_PROVIDER_ID"), manifest.fixedProviderId);
  assert.equal(rustString("FIXED_BASE_URL"), manifest.fixedBaseUrl);
  assert.equal(rustString("MANAGER_BUNDLE_ID"), manifest.managerBundleId);
  assert.equal(rustBool("FIXED_PROVIDER_EDITION"), manifest.features.fixedProviderEdition);
  assert.equal(rustBool("ADS_ENABLED"), manifest.features.adsEnabled);
  assert.equal(rustBool("UPDATES_ENABLED"), manifest.features.updatesEnabled);
});

test("managed integration is dynamic and provider identifiers stay paired", () => {
  assert.match(managedIntegration, /format!\("\{\}\/models"/);
  assert.match(managedIntegration, /supported_endpoint_types/);
  assert.match(managedIntegration, /openai-response/);
  assert.match(managedIntegration, /\[model_providers\.\{\}\]/);
  assert.match(managedIntegration, /distribution::FIXED_PROVIDER_ID/);
  assert.doesNotMatch(managedIntegration, /const\s+DEFAULT_MODEL/);
  assert.doesNotMatch(managedIntegration, /DEFAULT[^\n]*gpt-5\.5|gpt-5\.5[^\n]*DEFAULT/i);
});

test("production manager entry uses the isolated U-API shell", () => {
  assert.match(managerEntry, /\.\/uapi\/UapiApp/);
  assert.doesNotMatch(managerEntry, /from\s+["']\.\/App["']/);
  assert.deepEqual(manifest.visibleRoutes, ["overview", "connection", "maintenance", "about"]);
});
