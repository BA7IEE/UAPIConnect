# U-API Connect distribution layer

`uapi-connect.json` is the public, credential-free product manifest.  The Rust
constants in `crates/codex-plus-core/src/distribution.rs` intentionally mirror
it and are covered by a consistency test.

Rules:

- Never store API keys or upstream channel credentials here.
- Add product behavior in `crates/codex-plus-core/src/uapi.rs` or
  `apps/codex-plus-manager/src/uapi/` before editing upstream modules.
- Changes to upstream files must remain small integration hooks.
- Run `scripts/uapi/audit-upstream-surface.sh` after every upstream merge.

## Upstream-friendly structure

- The upstream `apps/codex-plus-manager/src/App.tsx` contains only audited
  per-model catalog integration hooks; the shared `styles.css` remains untouched.
- The fixed-provider shell and its responsive styling are isolated under
  `apps/codex-plus-manager/src/uapi/`.
- Tauri commands are isolated in `src-tauri/src/uapi_commands.rs`.
- Native Codex page injection is disabled for this edition; model discovery uses
  Codex's native `model_catalog_json` configuration instead.
