#!/bin/sh
# Rebuilds a release from its source, checks it against published RPM.
#
#     scripts/verify-release.sh [--podman | --docker] [--rpm <file>]
#                               [--no-download] [--ref <ref>] [<version>]
#
#   <version>       release to check, e.g. 1.2.0; default: latest
#   --rpm <file>    check this file too (e.g. one copied off a phone); version
#                   from its name unless given
#   --no-download   don't check GitHub's copy; only --rpm's
#   --ref <ref>     build this tag, branch or commit, not v<version> (rpm.yml
#                   checks a release before it is published)
#   --podman, --docker  engine to use; default: Docker if running, else Podman
#
# Needs git, curl, sha256sum, Podman or Docker. First run: ~5 GB pull, ~30 min
# build. Builds in fresh clone under VUO_WORKDIR, never in this tree; needs
# ~4 GB free there (checks).
#
# Exit, worst of copies checked: 0 same file, byte for byte; 1 same files
# inside, different packaging; 2 files differ; 3 couldn't check.
#
# Environment, all optional:
#   VUO_REPO     where to clone from; default GitHub
#   VUO_KEEP     1: keep clone and build
#   VUO_WORKDIR  where to clone and build; default ~/.cache/vuo-verify (not
#                $TMPDIR: /tmp often RAM disk, small and shared)
#   build-in-container.sh's VUO_* pass through, except VUO_RELEASE (always 1).

set -eu

prog=verify-release
github=muhnschein/vuo
repo=${VUO_REPO:-https://github.com/$github.git}

usage() {
    sed -n '4,13p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 3
}

rpm=""
download=1
ref=""
version=""
while [ $# -gt 0 ]; do
    case "$1" in
        --podman | --docker) VUO_ENGINE=${1#--}; export VUO_ENGINE ;;
        --rpm) [ $# -ge 2 ] || usage; rpm=$2; shift ;;
        --no-download) download=0 ;;
        --ref) [ $# -ge 2 ] || usage; ref=$2; shift ;;
        -h | --help) usage ;;
        -*) echo "$prog: unknown option $1" >&2; usage ;;
        *) [ -z "$version" ] || usage; version=$1 ;;
    esac
    shift
done

here=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/container-engine.sh
. "$here/scripts/container-engine.sh"
pick_engine

fail() { echo "$prog: $*" >&2; exit 3; }

# Before 30 min of build, not during.
need_gb=4
parent=${VUO_WORKDIR:-${XDG_CACHE_HOME:-$HOME/.cache}/vuo-verify}
mkdir -p "$parent" || fail "can't create $parent"
free_gb=$(df -Pk "$parent" | awk 'NR == 2 { print int($4 / 1048576) }')
[ "${free_gb:-0}" -ge "$need_gb" ] ||
    fail "$parent: ${free_gb:-?} GB free, build needs $need_gb GB; set VUO_WORKDIR to a roomier disk"
work=$(mktemp -d "$parent/run.XXXXXX")
# shellcheck disable=SC2317,SC2329 # run by the trap.
cleanup() {
    if [ "${VUO_KEEP:-0}" = 1 ]; then
        echo "kept: $work"
    else
        rm -rf "$work" 2>/dev/null || echo "$prog: could not remove all of $work" >&2
    fi
}
trap cleanup EXIT
mkdir -p "$work/cmp"

# What to check against: GitHub's copy, --rpm's, or both.
[ "$download" = 1 ] || [ -n "$rpm" ] || fail "--no-download: nothing to check; give --rpm"
if [ -n "$rpm" ]; then
    [ -f "$rpm" ] || fail "no such file: $rpm"
    cp "$rpm" "$work/cmp/local.rpm"
    if [ -z "$version" ]; then
        version=$(basename "$rpm" | sed -n 's/^harbour-vuo-\(.*\)-[^-]*\.aarch64\.rpm$/\1/p')
        [ -n "$version" ] || [ -n "$ref" ] ||
            fail "can't read a version from '$(basename "$rpm")'; give one"
    fi
fi
if [ "$download" = 1 ]; then
    # https only, redirects too.
    get() { curl -sSfL --proto '=https' --proto-redir '=https' "$@"; }
    if [ -z "$version" ]; then
        version=$(get "https://api.github.com/repos/$github/releases/latest" |
            sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p' | head -1) ||
            fail "can't ask GitHub for the latest release"
        [ -n "$version" ] || fail "can't find the latest release"
    fi
    name=harbour-vuo-$version-1.aarch64.rpm
    echo ">> downloading release $version"
    get -o "$work/cmp/github.rpm" \
        "https://github.com/$github/releases/download/v$version/$name" ||
        fail "release v$version has no $name"
fi
ref=${ref:-v$version}

# Fresh clone: nothing uncommitted from anywhere gets in.
echo ">> cloning $ref"
src="$work/vuo"
if printf '%s\n' "$ref" | grep -Eq '^[0-9a-f]{7,40}$'; then
    git clone --quiet "$repo" "$src"
    git -C "$src" checkout --quiet --detach "$ref"
else
    git clone --quiet --depth 1 --branch "$ref" "$repo" "$src" 2>/dev/null ||
        git clone --quiet --branch "$ref" "$repo" "$src" ||
        fail "can't clone $ref from $repo"
fi
[ -f "$src/scripts/build-in-container.sh" ] ||
    fail "$ref predates reproducible releases; nothing to rebuild it with"

# Source's own recipe. Commit time from clone, never caller's; crates into
# work dir, not caller's cache.
unset SOURCE_DATE_EPOCH
VUO_RELEASE=1 VUO_CARGO_HOME="$work/cargo" sh "$src/scripts/build-in-container.sh" ||
    fail "the build failed"

built=$(find "$src/dist" -name 'harbour-vuo-*.aarch64.rpm' | head -1)
[ -n "$built" ] || fail "the build made no device RPM"
cp "$built" "$work/cmp/rebuilt.rpm"

sum() { sha256sum < "$work/cmp/$1.rpm" | cut -d' ' -f1; }
ours=$(sum rebuilt)
echo
for c in github local; do
    [ -f "$work/cmp/$c.rpm" ] && printf '%-8s %s\n' "$c" "$(sum "$c")"
done
printf '%-8s %s\n' rebuilt "$ours"
[ -z "$rpm" ] || echo "local    = $rpm"

# Inside view from rpm itself, in clone's build image (has rpm; host may not):
# per file path, size, digest, mode, owner, link target. mtime (field 3) left
# out: that's packaging.
build_image=localhost/vuo-build:$(sha256sum "$src/rpm/Containerfile" | cut -c1-12)-rust$(
    sed -n 's/^channel = "\(.*\)"/\1/p' "$src/rust-toolchain.toml")
chmod 755 "$work/cmp"
chmod 644 "$work/cmp"/*.rpm

# One copy against rebuild: 0 same file, 1 same files inside, 2 differ.
check() {
    copy=$1
    [ "$(sum "$copy")" != "$ours" ] || {
        echo "$copy: VERIFIED, same file as the rebuild, byte for byte."
        return 0
    }
    echo ">> $copy: file differs; comparing what's inside"
    rc=0
    # shellcheck disable=SC2016 # expanded by the container's shell.
    ctr_run -v "$work/cmp:/cmp:ro" "$build_image" sh -c '
        for s in "$1" rebuilt; do
            rpm -qp --dump "/cmp/$s.rpm" | awk "{ \$3 = \"\"; print }" | sort > "/tmp/$s" || exit 3
        done
        echo "$(wc -l < "/tmp/$1") files"
        if ! diff "/tmp/$1" /tmp/rebuilt > /tmp/d; then
            echo "differing (< $1, > rebuilt):"
            grep "^[<>]" /tmp/d | head -50
            exit 10
        fi' sh "$copy" || rc=$?
    case "$rc" in
        0) echo "$copy: SAME FILES: every file, mode and owner inside matches the rebuild;"
           echo "   packaging differs (build time, file dates, host or signature)."
           return 1 ;;
        10) echo "$copy: DIFFERENT: not what $ref rebuilds to. Please report it."
            return 2 ;;
        *) fail "couldn't read $copy and the rebuild to compare them" ;;
    esac
}

worst=0
for c in github local; do
    [ -f "$work/cmp/$c.rpm" ] || continue
    echo
    rc=0
    check "$c" || rc=$?
    [ "$rc" -le "$worst" ] || worst=$rc
done
echo
case "$worst" in
    0) echo "VERIFIED: $ref rebuilds to every copy checked, byte for byte." ;;
    1) echo "SAME FILES: a bug in reproducibility: please report." ;;
    *) echo "DIFFERENT: see above." ;;
esac
exit "$worst"
