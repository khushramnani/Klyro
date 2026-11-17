#!/bin/sh
# Install klyro on Linux or macOS:
#   curl -fsSL https://raw.githubusercontent.com/khushramnani/Klyro/main/install.sh | sh
# Set KLYRO_INSTALL_DIR to choose the folder (default ~/.local/bin).
set -eu

REPO=khushramnani/Klyro
BIN_DIR=${KLYRO_INSTALL_DIR:-$HOME/.local/bin}
BASE="https://github.com/$REPO/releases/latest/download"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  *) target="" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if [ -n "$target" ] && curl -fsSL -o "$tmp/klyro.tar.gz" "$BASE/klyro-$target.tar.gz" 2>/dev/null; then
  curl -fsSL -o "$tmp/SHA256SUMS" "$BASE/SHA256SUMS"
  expected=$(grep " klyro-$target.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
  actual=$( (sha256sum "$tmp/klyro.tar.gz" 2>/dev/null || shasum -a 256 "$tmp/klyro.tar.gz") | cut -d' ' -f1)
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "klyro: checksum mismatch, not installing" >&2
    exit 1
  fi
  tar -xzf "$tmp/klyro.tar.gz" -C "$tmp"
  mkdir -p "$BIN_DIR"
  mv "$tmp/klyro" "$BIN_DIR/klyro"
  chmod +x "$BIN_DIR/klyro"
elif command -v cargo >/dev/null 2>&1; then
  echo "klyro: no prebuilt binary for this system yet; building from source with cargo (a few minutes)"
  cargo install --locked --git "https://github.com/$REPO" klyro-cli
  BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
else
  echo "klyro: no prebuilt binary for this system yet, and no Rust toolchain to build one." >&2
  echo "       Install Rust (https://rustup.rs) and run this again." >&2
  exit 1
fi

echo "klyro: installed $BIN_DIR/klyro"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "klyro: add $BIN_DIR to your PATH" ;;
esac
echo "klyro: agent skill: https://raw.githubusercontent.com/$REPO/main/skill/SKILL.md (e.g. save as ~/.claude/skills/klyro/SKILL.md)"
"$BIN_DIR/klyro" doctor || true
