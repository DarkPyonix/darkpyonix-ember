#!/usr/bin/env bash
# Download the newest OSE build for this machine from the `ose` GitHub Actions workflow and
# unpack it. Prints the server launcher path (…/bin/dpx-ose-server) on stdout, nothing else, so:
#
#   export EMBER_OSE_SERVER="$(crates/editor-conn/scripts/fetch-ose-artifact.sh)"
#   cargo test --manifest-path crates/editor-conn/Cargo.toml --test live_ose -- --ignored --nocapture
#
# Picks the most recent *successful* run of .github/workflows/test-ose.yml that still has an
# unexpired artifact named dpx-ose-<target> (pull-request runs and dispatches limited to other
# targets have none and are skipped). The artifact holds dpx-ose-<tag>-<target>.tar.gz and its
# .sha256; the checksum is verified before unpacking. Nothing is built or started.
#
# Needs: gh (authenticated: `gh auth login`), tar, shasum or sha256sum.
#
# Environment (all optional):
#   OSE_REPO        owner/name (default: the repo of the current checkout, via gh)
#   OSE_BRANCH      only runs on this branch (default: any)
#   OSE_RUN_ID      use this run instead of searching
#   OSE_TARGET      linux-x64 | linux-arm64 | darwin-arm64 | darwin-x64 (default: this machine)
#   OSE_CACHE_DIR   where to unpack (default: <repo>/.scratch/ose)
#   OSE_FORCE=1     download again even if this run is already unpacked
set -euo pipefail

log() { echo "fetch-ose-artifact: $*" >&2; }
die() { log "error: $*"; exit 1; }

command -v gh >/dev/null || die "gh (GitHub CLI) not found"

if [ -z "${OSE_TARGET:-}" ]; then
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) die "unsupported OS $(uname -s) (OSE targets: linux-x64, linux-arm64, darwin-arm64, darwin-x64)" ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=x64 ;;
    *) die "unsupported CPU $(uname -m)" ;;
  esac
  OSE_TARGET="$os-$arch"
fi
artifact="dpx-ose-$OSE_TARGET"

repo="${OSE_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}"
[ -n "$repo" ] || die "cannot determine the repository; set OSE_REPO=owner/name"

# ---- pick the run -------------------------------------------------------------------------------
run="${OSE_RUN_ID:-}"
if [ -z "$run" ]; then
  list_args=(run list -R "$repo" --workflow test-ose.yml --status success --limit 30 --json databaseId -q '.[].databaseId')
  [ -n "${OSE_BRANCH:-}" ] && list_args+=(--branch "$OSE_BRANCH")
  for id in $(gh "${list_args[@]}"); do
    has="$(gh api "repos/$repo/actions/runs/$id/artifacts?per_page=100" \
      -q ".artifacts[] | select(.name == \"$artifact\" and .expired == false) | .id" || true)"
    if [ -n "$has" ]; then run="$id"; break; fi
  done
  [ -n "$run" ] || die "no successful ose run in $repo${OSE_BRANCH:+ on $OSE_BRANCH} has an unexpired $artifact artifact (artifacts are kept 14 days; run the workflow: gh workflow run test-ose.yml -R $repo)"
fi
log "repo $repo, run $run, artifact $artifact"

# ---- download, verify, unpack -------------------------------------------------------------------
# Inside the repository, as AGENTS.md asks: .scratch/ is ignored.
repo_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cache="${OSE_CACHE_DIR:-$repo_root/.scratch/ose}"
dest="$cache/$run-$OSE_TARGET"

find_server() { find "$dest" -mindepth 3 -maxdepth 3 -path '*/bin/dpx-ose-server' -type f 2>/dev/null | head -n 1; }

if [ "${OSE_FORCE:-0}" != 1 ] && [ -n "$(find_server)" ]; then
  log "already unpacked in $dest"
else
  rm -rf "$dest"
  mkdir -p "$dest/download"
  gh run download "$run" -R "$repo" -n "$artifact" -D "$dest/download"

  tarball="$(find "$dest/download" -maxdepth 1 -name 'dpx-ose-*.tar.gz' -type f | head -n 1)"
  [ -n "$tarball" ] || die "no dpx-ose-*.tar.gz in the $artifact artifact"
  sums="$tarball.sha256"
  if [ -f "$sums" ]; then
    if command -v sha256sum >/dev/null; then
      (cd "$(dirname "$tarball")" && sha256sum -c "$(basename "$sums")") >&2
    else
      (cd "$(dirname "$tarball")" && shasum -a 256 -c "$(basename "$sums")") >&2
    fi
  else
    log "warning: no $(basename "$sums") in the artifact; checksum not verified"
  fi

  tar -xzf "$tarball" -C "$dest"
  rm -rf "$dest/download"
fi

server="$(find_server)"
[ -n "$server" ] || die "unpacked, but no */bin/dpx-ose-server under $dest"
[ -x "$server" ] || die "$server is not executable"
log "$(cat "$(dirname "$(dirname "$server")")/product.json" 2>/dev/null \
  | sed -n 's/^[[:space:]]*"commit":[[:space:]]*"\([0-9a-f]*\)".*/commit \1/p' | head -n 1)"
echo "$server"
