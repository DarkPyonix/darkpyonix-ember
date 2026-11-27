#!/usr/bin/env bash
# Installs the DarkPyonix OSE release that matches this Ember build (SPEC FR-W4 acceptance step 6,
# #83). Ships next to ember-server and ember-node in their release archives, with an OSE_VERSION
# file naming the OSE release (ose-v<Code-OSS tag>); in a checkout it reads .github/scripts/ose/VERSION.
#
#   ./install-ose.sh            download, verify, unpack; prints the launcher path
#
# It downloads dpx-ose-<tag>-<target>.tar.gz and SHA256SUMS from the GitHub release with curl (no
# gh, no login), refuses an archive SHA256SUMS does not list or match, and unpacks it into
# ~/.local/share, where ember-server and ember-node look when EMBER_OSE_SERVER is unset
# (ember/node/src/ose.rs, server_binary). The launcher path is the only line on stdout.
#
# Environment (all optional):
#   OSE_RELEASE      the release tag (default: OSE_VERSION beside this script, else ose-v<.github/scripts/ose/VERSION>)
#   OSE_TARGET       linux-x64 | linux-arm64 | darwin-arm64 | darwin-x64 (default: this machine)
#   OSE_INSTALL_DIR  where to unpack (default: ~/.local/share)
#   OSE_BASE_URL     release download base (default: https://github.com/DarkPyonix/darkpyonix-ember/releases/download)
#   OSE_FORCE=1      download again even when this release is already unpacked
#
# Windows has no server release yet; there, unpack the windows-x64 archive into %LOCALAPPDATA% by
# hand (.github/scripts/ose/README.md).
set -euo pipefail

log() { echo "install-ose: $*" >&2; }
die() { log "error: $*"; exit 1; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
command -v curl >/dev/null || die "curl not found"
command -v tar >/dev/null || die "tar not found"

release="${OSE_RELEASE:-}"
if [ -z "$release" ]; then
  if [ -f "$here/OSE_VERSION" ]; then
    release="$(tr -d '[:space:]' < "$here/OSE_VERSION")"
  elif [ -f "$here/../../../.github/scripts/ose/VERSION" ]; then
    release="ose-v$(tr -d '[:space:]' < "$here/../../../.github/scripts/ose/VERSION")"
  else
    die "no OSE_VERSION beside this script and no .github/scripts/ose/VERSION; set OSE_RELEASE=ose-v<tag>"
  fi
fi
case "$release" in
  ose-v[0-9]*) ;;
  *) die "$release is not an OSE release tag (ose-v<code-oss tag>, for example ose-v1.139.1)" ;;
esac

if [ -z "${OSE_TARGET:-}" ]; then
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) die "unsupported OS $(uname -s); unpack the archive by hand (.github/scripts/ose/README.md)" ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=x64 ;;
    *) die "unsupported CPU $(uname -m)" ;;
  esac
  OSE_TARGET="$os-$arch"
fi
case "$OSE_TARGET" in
  linux-x64 | linux-arm64 | darwin-arm64 | darwin-x64) ;;
  *) die "unsupported OSE_TARGET $OSE_TARGET" ;;
esac

# The archive name carries the Code-OSS tag; a rebuild tag (ose-v1.139.1-dpx.2) still names
# dpx-ose-1.139.1-<target>.tar.gz, so the tag's suffix is dropped.
code_oss="${release#ose-v}"
code_oss="${code_oss%%-*}"
name="dpx-ose-$code_oss-$OSE_TARGET"
dest="${OSE_INSTALL_DIR:-${HOME:?HOME is not set}/.local/share}"
launcher="$dest/$name/bin/dpx-ose-server"

if [ "${OSE_FORCE:-0}" != 1 ] && [ -x "$launcher" ]; then
  log "$release already installed in $dest/$name"
  echo "$launcher"
  exit 0
fi

base="${OSE_BASE_URL:-https://github.com/DarkPyonix/darkpyonix-ember/releases/download}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
log "downloading $name.tar.gz from release $release"
curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$base/$release/SHA256SUMS" \
  || die "release $release has no SHA256SUMS at $base/$release (is the OSE release published?)"
curl -fsSL --retry 3 -o "$work/$name.tar.gz" "$base/$release/$name.tar.gz" \
  || die "release $release has no $name.tar.gz"

awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f' "$work/SHA256SUMS" > "$work/this.sha256"
[ -s "$work/this.sha256" ] || die "SHA256SUMS of $release does not list $name.tar.gz"
if command -v sha256sum >/dev/null; then
  (cd "$work" && sha256sum -c this.sha256) >&2 || die "$name.tar.gz does not match SHA256SUMS of $release"
else
  (cd "$work" && shasum -a 256 -c this.sha256) >&2 || die "$name.tar.gz does not match SHA256SUMS of $release"
fi

mkdir -p "$dest"
rm -rf "${dest:?}/$name"
tar -xzf "$work/$name.tar.gz" -C "$dest"
[ -x "$launcher" ] || die "unpacked, but $launcher is missing or not executable"
log "installed $release into $dest/$name"
echo "$launcher"
