#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOG_ROOT="$ROOT/local-build-logs"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG_DIR="$LOG_ROOT/$STAMP"
mkdir -p "$LOG_DIR"
exec > >(tee "$LOG_DIR/build.log") 2>&1

fail() {
  echo
  echo "构建失败：$*" >&2
  echo "日志：$LOG_DIR/build.log" >&2
  exit 1
}

step() {
  echo
  echo "============================================================"
  echo "$*"
  echo "============================================================"
}

if [ "$(uname -s)" != "Darwin" ]; then
  fail "该脚本只能在 macOS 上运行。"
fi

step "1/9 检查 Xcode Command Line Tools"
if ! xcode-select -p >/dev/null 2>&1; then
  echo "未安装 Xcode Command Line Tools，正在打开系统安装窗口。"
  xcode-select --install || true
  fail "安装完成后重新双击本脚本。"
fi

step "2/9 检查 Node.js 22"
if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
  if command -v brew >/dev/null 2>&1; then
    echo "未检测到 Node.js，将通过 Homebrew 安装 node@22。"
    brew install node@22
    export PATH="$(brew --prefix node@22)/bin:$PATH"
  else
    open "https://nodejs.org/zh-cn/download" || true
    fail "未检测到 Node.js/npm，已打开下载页。安装 Node.js 22 后重新运行。"
  fi
fi
NODE_MAJOR="$(node -p 'process.versions.node.split(".")[0]')"
if [ "$NODE_MAJOR" -lt 22 ]; then
  echo "当前 Node.js：$(node -v)"
  if command -v brew >/dev/null 2>&1; then
    echo "正在通过 Homebrew 安装 node@22。"
    brew install node@22
    export PATH="$(brew --prefix node@22)/bin:$PATH"
  else
    fail "需要 Node.js 22。"
  fi
fi
node -v
npm -v

step "3/9 检查 Rust stable"
if ! command -v cargo >/dev/null 2>&1; then
  echo "未检测到 Rust。"
  read -r -p "是否通过 rustup 官方脚本安装 Rust？[Y/n] " answer
  answer="${answer:-Y}"
  case "$answer" in
    Y|y|yes|YES)
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
      # shellcheck disable=SC1090
      source "$HOME/.cargo/env"
      ;;
    *) fail "缺少 Rust/cargo。" ;;
  esac
fi
if command -v rustup >/dev/null 2>&1; then
  rustup toolchain install stable --profile minimal --component rustfmt
  rustup default stable
fi
cargo --version
rustc --version

step "4/9 安装前端依赖"
cd "$ROOT/apps/codex-plus-manager"
rm -rf node_modules
npm ci

step "5/9 运行前端与发行规则测试"
node --experimental-strip-types --test src/*.test.ts
cd "$ROOT"
node --test scripts/uapi/tests/distribution.test.mjs
bash scripts/uapi/audit-upstream-surface.sh

step "6/9 TypeScript 与前端构建"
cd "$ROOT/apps/codex-plus-manager"
npm run check
npm run vite:build

step "7/9 Rust 格式与测试"
cd "$ROOT"
cargo fmt --all -- --check
cargo test --workspace

step "8/9 构建 Release 二进制"
cargo build --release

test -x "$ROOT/target/release/codex-plus-plus" || fail "缺少启动器二进制。"
test -x "$ROOT/target/release/codex-plus-plus-manager" || fail "缺少设置程序二进制。"

step "9/9 生成 macOS DMG"
case "$(uname -m)" in
  arm64) ARCH="arm64" ;;
  x86_64) ARCH="x64" ;;
  *) fail "不支持的芯片架构：$(uname -m)" ;;
esac
BASE_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
[ -n "$BASE_VERSION" ] || fail "无法从 Cargo.toml 读取版本。"
VERSION="${BASE_VERSION}-uapi.local.$STAMP"
BINARY_DIR="$ROOT/target/release" bash "$ROOT/scripts/uapi/package-macos-dmg.sh" "$VERSION" "$ARCH"
DMG="$ROOT/dist/uapi/macos/UAPIConnect-${VERSION}-macos-${ARCH}.dmg"
test -f "$DMG" || fail "DMG 未生成。"

cat > "$LOG_DIR/result.txt" <<RESULT
构建成功
版本：$VERSION
架构：$ARCH
DMG：$DMG
日志：$LOG_DIR/build.log
RESULT

cat "$LOG_DIR/result.txt"
open "$DMG"
