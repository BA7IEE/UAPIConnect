# U-API Connect upstream synchronization SOP

## Remotes

```bash
git remote add upstream https://github.com/BigPizzaV3/CodexPlusPlus.git
git remote add origin <your fork>
```

## Sync procedure

```bash
git fetch upstream --tags
git switch -c upstream-sync/<date> main
git merge --no-ff upstream/main
```

Resolve conflicts by priority:

1. Preserve upstream launcher, CDP, bridge, SQLite and relay fixes.
2. Reapply only the small distribution hooks listed below.
3. Do not copy the old upstream `App.tsx` into the U-API UI.  The U-API entry
   lives in `src/uapi/` and the upstream file should remain untouched. Shared
   styles are reused read-only; edition CSS lives in `src/uapi/uapi.css`.
4. Run the full U-API workflow before merging.

## Expected integration surface

- `crates/codex-plus-core/src/lib.rs`
- `crates/codex-plus-core/src/ads.rs`
- `crates/codex-plus-core/src/update.rs`
- `crates/codex-plus-core/src/install/*`
- `apps/codex-plus-launcher/src/main.rs`
- `apps/codex-plus-manager/src/main.tsx`
- `apps/codex-plus-manager/src-tauri/src/{lib,main}.rs`
- `apps/codex-plus-manager/src-tauri/src/uapi_commands.rs` (new distribution module)
- `apps/codex-plus-manager/src-tauri/tauri.conf.json`

All other product logic must stay in newly added U-API files.

## Required validation after sync

```bash
npm ci --prefix apps/codex-plus-manager
npm test --prefix apps/codex-plus-manager
npm run check --prefix apps/codex-plus-manager
npm run vite:build --prefix apps/codex-plus-manager
cargo fmt --all -- --check
cargo test --workspace
cargo build --release
bash scripts/uapi/audit-upstream-surface.sh <pre-merge-sha>
```

Then install the generated package on a clean Mac/Windows machine and test:
key configuration, dynamic model refresh, model switching, Codex file edits,
command execution, second launch, reconfiguration and rollback.
