# U-API Connect upstream synchronization SOP

## Remotes

```bash
git remote add upstream https://github.com/BigPizzaV3/CodexPlusPlus.git
git remote add origin <your fork>
```

## 同步原则

默认只同步已经发布的稳定 tag，不直接追逐 `upstream/main`。先查看主线新增提交和
最新 release，再决定目标版本。这样 Windows、macOS 安装测试都有一个可复现的
上游基线；确实需要未发布修复时，单独记录目标 commit 和原因。

当前历史已经和上游连通，禁止再使用 `--allow-unrelated-histories`，也不要把旧版
大提交整体重放到新版源码上。

## Sync procedure

```bash
git fetch upstream --tags
git switch main
git pull --ff-only origin main
git switch -c codex/upstream-v<version>
git merge --no-ff v<version>
```

合并前先确认目标 tag 确实位于上游历史，并记录比较范围：

```bash
git merge-base --is-ancestor v<current> v<version>
git log --oneline v<current>..v<version>
```

Resolve conflicts by priority:

1. Preserve upstream launcher, CDP, bridge, SQLite and relay fixes.
2. Reapply only the small distribution hooks listed below.
3. Do not copy the old upstream `App.tsx` into the U-API UI. The U-API entry
   lives in `src/uapi/`. Shared relay fixes may touch the upstream screen only
   where the same catalog transaction must work for every profile. Shared
   styles are reused read-only; edition CSS lives in `src/uapi/uapi.css`.
4. Run the full U-API workflow before merging.
5. 不直接覆盖整个共享文件，不通过删除测试来解决冲突。

## Expected integration surface

- `Cargo.toml` and `crates/codex-plus-core/Cargo.toml` (`keyring` dependency)
- `crates/codex-plus-core/src/lib.rs`
- `crates/codex-plus-core/src/launcher.rs` (one fixed-edition dispatch hook)
- `crates/codex-plus-core/src/{model_suffix,relay_config,relay_switch}.rs`
- `crates/codex-plus-core/src/settings.rs`（私密文件原子写入）
- `crates/codex-plus-core/src/ads.rs`
- `crates/codex-plus-core/src/update.rs`
- `crates/codex-plus-core/src/install/*`
- `apps/codex-plus-launcher/src/main.rs`
- `apps/codex-plus-manager/src/main.tsx`
- `apps/codex-plus-manager/src/uapi/*` and `src/uapi-launch-policy*.ts`
- `apps/codex-plus-manager/src/App.tsx`、`model-windows.ts` 和
  `model-windows.test.ts`（逐模型目录编辑与 active-profile 事务路径）
- `apps/codex-plus-manager/src-tauri/src/{commands,lib,main}.rs`
- `apps/codex-plus-manager/src-tauri/src/uapi_commands.rs` (new distribution module)
- `apps/codex-plus-manager/src-tauri/tauri.conf.json`

U-API-specific product logic must stay in newly added U-API files. The shared
relay files above may only contain generic model-catalog correctness and
transaction fixes that remain valid for every profile. In particular, the
dual-mode and credential logic belongs in `crates/codex-plus-core/src/uapi.rs`
and `crates/codex-plus-core/src/uapi/credentials.rs`; ordinary settings must
not contain the U-API key or an official `auth.json` snapshot. The `keyring`
workspace dependency is part of this distribution boundary.

## Required validation after sync

```bash
npm ci --prefix apps/codex-plus-manager
npm test --prefix apps/codex-plus-manager
npm run check --prefix apps/codex-plus-manager
npm run vite:build --prefix apps/codex-plus-manager
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace --all-targets
cargo build --release
bash scripts/uapi/audit-upstream-surface.sh <pre-merge-sha>
```

Then install the generated package on a clean Mac/Windows machine and test:
key configuration, dynamic model refresh, model switching, Codex file edits,
command execution, second launch, reconfiguration and rollback.

For the dual-mode release also verify:

- Start with an official Codex login, configure U-API Connect, then switch back
  to the official subscription and confirm the login still works.
- Switch back to U-API Connect and confirm the managed key and model catalog are
  restored.
- Inspect the ordinary settings file and confirm it contains neither the U-API
  key nor official access or refresh tokens.
