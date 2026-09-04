#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# U-API edition code should stay concentrated in these paths plus a short list
# of audited upstream integration points. The routes/settings/bridge/data test
# entries below are exact rustfmt-only or atomic-write regression repairs. The
# provider-sync source entry is the private deterministic seam for its TOCTOU
# regression test; the protocol-proxy test only replaces a flaky wall-clock
# assertion. The model editor paths are the audited per-model catalog points;
# watcher entries enforce the fixed distribution's companion-process boundary.
# vision.rs and floating_panel_outline.rs are rustfmt-only repairs to this
# pinned upstream snapshot; auto-compact.ts documents the opt-in empty value.
allowed='^(README\.md|Cargo\.(toml|lock)|distribution/.*|docs/uapi/.*|scripts/uapi/.*|\.github/workflows/(uapi-build|release-assets)\.yml|crates/codex-plus-core/Cargo\.toml|crates/codex-plus-core/src/(distribution|manager_activation|paths|uapi|vision|watcher)\.rs|crates/codex-plus-core/src/uapi/.*|crates/codex-plus-core/src/lib\.rs|crates/codex-plus-core/src/(ads|update|launcher|model_suffix|relay_config|relay_switch|routes|session_share|settings|stepwise)\.rs|crates/codex-plus-core/src/install/.*|crates/codex-plus-core/tests/(bridge_routes|dream_skin_runtime|floating_panel_outline|installers|launcher|model_suffix|protocol_proxy|relay_config|relay_switch|watcher)\.rs|crates/codex-plus-data/src/(provider_sync|storage)\.rs|crates/codex-plus-data/tests/(provider_sync|storage_adapter)\.rs|apps/codex-plus-launcher/(build\.rs|src/main\.rs)|apps/codex-plus-manager/(index\.html|src/uapi/.*|src/uapi-launch-policy(\.test)?\.ts|src/uapi-manager-activation(\.test)?\.ts|src/auto-compact\.ts|src/model-(routes\.test|windows(\.test)?)\.ts|src/App\.tsx|src/main\.tsx|src/styles\.css)|apps/codex-plus-manager/src-tauri/(build\.rs|tauri\.conf\.json|tests/windows_subsystem\.rs|src/(commands|lib|main|uapi_commands)\.rs))$'

unexpected=0
base_ref="${1:-}"
base_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$base_ref" ] && [ -f distribution/upstream-base.txt ]; then
  base_ref="$(tr -d '\r\n' < distribution/upstream-base.txt)"
  if ! [[ "$base_ref" =~ ^([0-9a-f]{40}|v[0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    echo "invalid pinned upstream base; expected a full commit SHA or stable release tag" >&2
    exit 1
  fi
fi
if [ -z "$base_ref" ]; then
  if [ -n "$base_version" ] && git rev-parse --verify --quiet "refs/tags/v${base_version}^{commit}" >/dev/null; then
    base_ref="v${base_version}"
  fi
fi
if [ -z "$base_ref" ]; then
  echo "cannot find the exact upstream release tag for Cargo.toml; pass a release tag or commit explicitly" >&2
  exit 1
fi
base_commit="$(git rev-parse --verify --end-of-options "${base_ref}^{commit}")"
if [ "${UAPI_REQUIRE_STABLE_UPSTREAM:-0}" = "1" ]; then
  stable_commit="$(git rev-parse --verify "refs/tags/v${base_version}^{commit}")"
  if [ "$base_commit" != "$stable_commit" ]; then
    echo "unreleased upstream preview cannot be published; pin the matching stable release tag first" >&2
    exit 1
  fi
fi
git merge-base --is-ancestor "$base_commit" HEAD || {
  echo "upstream base is not an ancestor of HEAD: $base_ref" >&2
  exit 1
}
echo "auditing customization surface against $base_ref"
while IFS= read -r file; do
  [ -z "$file" ] && continue
  if ! printf '%s\n' "$file" | grep -Eq "$allowed"; then
    echo "unexpected customization surface: $file" >&2
    unexpected=1
  fi
done < <({
  git diff --name-only "$base_commit" HEAD 2>/dev/null || true
  git diff --name-only 2>/dev/null || true
  git diff --name-only --cached 2>/dev/null || true
  git ls-files --others --exclude-standard 2>/dev/null || true
} | sort -u)

# The actual temporary key is supplied only by CI/local environment when available.
# Never store a real customer or test key in the repository.
if [ -n "${UAPI_FORBIDDEN_TEST_KEY:-}" ]; then
  secret_hits="$(grep -RIlF --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=target \
    --exclude='*.lock' -- "$UAPI_FORBIDDEN_TEST_KEY" . || true)"
  if [ -n "$secret_hits" ]; then
    echo "forbidden test key found in source" >&2
    printf '%s\n' "$secret_hits" >&2
    unexpected=1
  fi
fi

# Catch obvious accidentally committed API keys without embedding any real key.
generic_secret_hits="$(grep -RIlE --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=target \
  --exclude='*.lock' --exclude='*.md' \
  'sk-[A-Za-z0-9_-]{24,}' crates apps distribution scripts .github 2>/dev/null || true)"
if [ -n "$generic_secret_hits" ]; then
  echo "possible API key found in source" >&2
  printf '%s\n' "$generic_secret_hits" >&2
  unexpected=1
fi

hardcoded_model_hits="$(grep -RIl --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=target \
  --exclude='*.md' --exclude='*.lock' \
  'gpt-5\.5.*DEFAULT\|DEFAULT.*gpt-5\.5' crates apps distribution || true)"
if [ -n "$hardcoded_model_hits" ]; then
  echo "hard-coded model default found" >&2
  printf '%s\n' "$hardcoded_model_hits" >&2
  unexpected=1
fi

exit "$unexpected"
