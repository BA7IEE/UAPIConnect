#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# U-API edition code should stay concentrated in these paths plus a short list
# of audited upstream integration points.
allowed='^(distribution/.*|docs/uapi/.*|scripts/uapi/.*|\.github/workflows/uapi-build\.yml|crates/codex-plus-core/src/(distribution|uapi)\.rs|crates/codex-plus-core/src/lib\.rs|crates/codex-plus-core/src/(ads|update)\.rs|crates/codex-plus-core/src/install/.*|crates/codex-plus-core/tests/installers\.rs|apps/codex-plus-launcher/src/main\.rs|apps/codex-plus-manager/src/uapi/.*|apps/codex-plus-manager/src/main\.tsx|apps/codex-plus-manager/src/styles\.css|apps/codex-plus-manager/src-tauri/(tauri\.conf\.json|src/(commands|lib|main|uapi_commands)\.rs))$'

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
  if grep -RIlF --exclude-dir=.git --exclude='*.lock' -- "$UAPI_FORBIDDEN_TEST_KEY" . >/tmp/uapi-secret-hits; then
    echo "forbidden test key found in source" >&2
    cat /tmp/uapi-secret-hits >&2
    unexpected=1
  fi
fi

# Catch obvious accidentally committed API keys without embedding any real key.
if grep -RInE --exclude-dir=.git --exclude='*.lock' --exclude='*.md' \
  'sk-[A-Za-z0-9_-]{24,}' crates apps distribution scripts .github 2>/dev/null \
  >/tmp/uapi-generic-secret-hits; then
  echo "possible API key found in source" >&2
  cat /tmp/uapi-generic-secret-hits >&2
  unexpected=1
fi

if grep -RIn --exclude-dir=.git --exclude='*.md' --exclude='*.lock' 'gpt-5\.5.*DEFAULT\|DEFAULT.*gpt-5\.5' crates apps distribution >/tmp/uapi-hardcoded-models; then
  echo "hard-coded model default found" >&2
  cat /tmp/uapi-hardcoded-models >&2
  unexpected=1
fi

exit "$unexpected"
