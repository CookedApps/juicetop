#!/usr/bin/env sh
# Installs juicetop and starts the background collector.
#
#   curl -fsSL https://raw.githubusercontent.com/CookedApps/juicetop/main/install.sh | sh
#
# Environment:
#   JUICETOP_VERSION=v0.1.0   install a specific release instead of the latest
#   JUICETOP_SERVICE=user     run the collector as your user (no root, less precise CPU data)
#   JUICETOP_SERVICE=none     only install the binary
set -eu

repo="CookedApps/juicetop"
version="${JUICETOP_VERSION:-latest}"
service="${JUICETOP_SERVICE:-system}"

say() { printf '\033[1;33m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

[ "$(uname -s)" = "Linux" ] || die "juicetop runs on Linux only (for now)"
case "$(uname -m)" in
  x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
  aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
  *) die "no prebuilt binary for $(uname -m); build from source: cargo install --locked --git https://github.com/$repo" ;;
esac

if [ "$(id -u)" = "0" ]; then
  sudo=""
elif has sudo; then
  sudo="sudo"
else
  sudo=""
  [ "$service" = "user" ] || die "need root or sudo for the system service; use JUICETOP_SERVICE=user"
fi

fetch() {
  if has curl; then curl -fsSL "$1" -o "$2"
  elif has wget; then wget -qO "$2" "$1"
  else die "need curl or wget"
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ "$version" = "latest" ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi
archive="juicetop-$target.tar.gz"

say "downloading $archive ($version)"
if fetch "$base/$archive" "$tmp/$archive" && fetch "$base/$archive.sha256" "$tmp/$archive.sha256"; then
  (cd "$tmp" && sha256sum -c "$archive.sha256" >/dev/null) || die "checksum mismatch"
  tar -xzf "$tmp/$archive" -C "$tmp"
  bin="$tmp/juicetop"
elif has cargo; then
  say "no prebuilt binary available, building from source with cargo"
  cargo install --locked --git "https://github.com/$repo" juicetop
  bin="${CARGO_HOME:-$HOME/.cargo}/bin/juicetop"
else
  die "download failed and cargo is not installed (https://rustup.rs)"
fi

case "$service" in
  system)
    say "installing /usr/local/bin/juicetop and the system service"
    $sudo "$bin" service install
    ;;
  user)
    mkdir -p "$HOME/.local/bin"
    install -m 755 "$bin" "$HOME/.local/bin/juicetop"
    say "installed ~/.local/bin/juicetop"
    "$HOME/.local/bin/juicetop" service install --user
    ;;
  none)
    $sudo install -m 755 "$bin" /usr/local/bin/juicetop
    say "installed /usr/local/bin/juicetop (no service; start one with: sudo juicetop service install)"
    ;;
  *) die "JUICETOP_SERVICE must be system, user or none" ;;
esac

say "done. Open the dashboard with: juicetop"
