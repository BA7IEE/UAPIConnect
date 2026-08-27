#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# U-API edition code should stay concentrated in these paths plus a short list
# of audited upstream integration points. The routes/settings/bridge/data test
# entries below are exact rustfmt-only or atomic-write regression repairs. The
# provider-sync source entry is the private deterministic seam for its TOCTOU
# regression test; the protocol-proxy test only replaces a flaky wall-clock
# assertion. The model editor paths are the audited per-model catalog points.
allowed='^(Cargo\.(toml|lock)|distribution/.*|docs/uapi/.*|scripts/uapi/.*|\.github/workflows/uapi-build\.yml|crates/codex-plus-core/Cargo\.toml|crates/codex-plus-core/src/(distribution|uapi)\.rs|crates/codex-plus-core/src/uapi/.*|crates/codex-plus-core/src/lib\.rs|crates/codex-plus-core/src/(ads|update|launcher|model_suffix|relay_config|relay_switch|routes|settings)\.rs|crates/codex-plus-core/src/install/.*|crates/codex-plus-core/tests/(bridge_routes|dream_skin_runtime|installers|launcher|model_suffix|protocol_proxy|relay_config|relay_switch)\.rs|crates/codex-plus-data/src/provider_sync\.rs|crates/codex-plus-data/tests/(provider_sync|storage_adapter)\.rs|apps/codex-plus-launcher/(build\.rs|src/main\.rs)|apps/codex-plus-manager/(index\.html|src/uapi/.*|src/uapi-launch-policy(\.test)?\.ts|src/model-(routes\.test|windows(\.test)?)\.ts|src/App\.tsx|src/main\.tsx|src/styles\.css)|apps/codex-plus-manager/src-tauri/(build\.rs|tauri\.conf\.json|tests/windows_subsystem\.rs|src/(commands|lib|main|uapi_commands)\.rs))$'

unexpected=0
base_ref="${1:-}"
if [ -z "$base_ref" ]; then
  base_ref="$(git rev-list --max-parents=0 HEAD | tail -n 1)"
fi
while IFS= read -r file; do
  [ -z "$file" ] && continue
  if ! printf '%s\n' "$file" | grep -Eq "$allowed"; then
    echo "unexpected customization surface: $file" >&2
    unexpected=1
  fi
done < <({
  git diff --name-only "$base_ref" HEAD 2>/dev/null || true
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
