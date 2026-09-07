#!/usr/bin/env bash
# Packaging sanity checks that need no SDK, so they can run in `make check`.
#
# These catch the packaging mistakes that are otherwise only discovered by a
# 40-minute SDK build: a file referenced by the spec that does not exist, a
# desktop entry that fails validation, a version that has drifted between
# Cargo.toml and the spec.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { echo "  $*"; }
bad() { echo "FAIL: $*" >&2; fail=1; }

echo "== packaging checks =="

SPEC=rpm/harbour-vuo.spec
[ -f "$SPEC" ] || { bad "$SPEC is missing"; exit 1; }

# 1. The spec's version must match the workspace's.
spec_version=$(sed -n 's/^Version:[[:space:]]*//p' "$SPEC" | head -1)
cargo_version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "$spec_version" != "$cargo_version" ]; then
    bad "version drift: spec says $spec_version, Cargo.toml says $cargo_version"
else
    note "version $spec_version matches Cargo.toml"
fi

# 2. Every file the spec installs must exist -- DERIVED FROM THE SPEC, not from
#    a list restated here. A hardcoded list only ever re-checks the files
#    someone remembered to add to it: adding an `install -D` line for a file
#    that does not exist passed, which is precisely the 40-minute SDK failure
#    this check exists to pre-empt.
#
#    The source-side operand of each `install -D...` line, with the spec's own
#    `for RES in ...` loop expanded. Lines whose source is a build artefact
#    (%{targetdir}/...) or a glob that may legitimately match nothing
#    (translations/*.qm, guarded by `if ls` in the spec) are skipped.
installed=$(
    sed 's/#.*//' "$SPEC" \
        | grep -oE 'install -D[m0-9 ]* +[^ ]+' \
        | awk '{print $NF}' \
        | grep -v '^%' \
        | sort -u
)
[ -n "$installed" ] || bad "no install lines found in $SPEC; check #2 would be vacuous"

resolutions=$(sed -n 's/^for RES in \(.*\); do/\1/p' "$SPEC" | head -1)
checked=0
while IFS= read -r f; do
    [ -n "$f" ] || continue
    case "$f" in
        *'*'*) continue ;;                  # a guarded glob, e.g. translations/*.qm
        *'{}'*) continue ;;                 # find -exec placeholder; the sweep below covers qml/
        *'${RES}'*)
            for res in $resolutions; do
                target=${f//\$\{RES\}/$res}
                checked=$((checked + 1))
                [ -e "$target" ] || bad "the spec installs $target, which does not exist"
            done
            continue
            ;;
    esac
    checked=$((checked + 1))
    [ -e "$f" ] || bad "the spec installs $f, which does not exist"
done <<< "$installed"

# desktop-file-install and the `find ./qml` sweep are not `install -D` lines.
for f in harbour-vuo.desktop qml/harbour-vuo.qml LICENSE; do
    checked=$((checked + 1))
    [ -e "$f" ] || bad "the spec installs $f, which does not exist"
done

[ "$checked" -ge 7 ] || bad "check #2 only examined $checked files; the spec parse must have failed"
note "every file the spec installs is present ($checked checked, derived from the spec)"

# 3. The desktop entry must validate.
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate harbour-vuo.desktop || bad "harbour-vuo.desktop failed validation"
    note "desktop entry validates"
else
    note "desktop-file-validate not installed; skipping (CI installs it)"
fi

# 4. The spec must build the binary explicitly. The workspace's
#    default-members are the Qt-free set, so a bare `cargo build --release`
#    would silently produce no installable binary.
#    Matched on the BUILD LINE, not anywhere in the file. A whole-file grep is
#    satisfied by a comment, a %description or a changelog entry mentioning the
#    flag -- so the regression it is named for (a bare `cargo build --release`
#    that produces no installable binary) could ship with the check green.
build_line=$(
    sed 's/#.*//' "$SPEC" \
        | sed -e :a -e '/\\$/N; s/\\\n//; ta' \
        | grep -E '(^|[^[:alnum:]_])cargo build' \
        | head -1
)
if [ -z "$build_line" ]; then
    bad "the spec has no cargo build line"
else
    case "$build_line" in
        *"--bin harbour-vuo"*) : ;;
        *) bad "the cargo build line must pass --bin harbour-vuo; default-members would otherwise build nothing" ;;
    esac
    case "$build_line" in
        *"--features sailfishapp"*) : ;;
        *) bad "the cargo build line must pass --features sailfishapp for the device entry point" ;;
    esac
    note "the spec builds the right binary with the right features"
fi

# 5. Every language ships, and none of them has drifted.
#
#    A .ts without a .qm is a language that is IN the tree and not on the
#    phone: the specs install `translations/*.qm` and nothing warns when one is
#    absent. And a source string added to the QML reaches only the catalogue
#    someone remembered to run `lupdate` over, so the message counts are
#    compared against English -- 40 catalogues is far past the number anyone
#    keeps in their head.
#
#    Untranslated entries are NOT an error. Qt falls back to the source string,
#    which is English and correct; a check that failed on them would mean no UI
#    string could be added without 39 translations in the same commit.
missing_qm=""
drifted=""
reference=$(grep -c "<message" translations/harbour-vuo-en.ts 2>/dev/null || echo 0)
if [ "$reference" -eq 0 ]; then
    bad "translations/harbour-vuo-en.ts is missing; it is the reference catalogue"
else
    for ts in translations/*.ts; do
        [ -f "${ts%.ts}.qm" ] || missing_qm="$missing_qm ${ts##*/}"
        n=$(grep -c "<message" "$ts")
        [ "$n" -eq "$reference" ] || drifted="$drifted ${ts##*/}($n)"
    done
    [ -z "$missing_qm" ] || bad "no compiled .qm for:$missing_qm -- run lrelease translations/*.ts"
    [ -z "$drifted" ] || bad "these catalogues disagree with English ($reference messages):$drifted -- run lupdate over all of them"
    if [ -z "$missing_qm" ] && [ -z "$drifted" ]; then
        note "$(ls translations/*.ts | wc -l | tr -d ' ') translations, all compiled and all carrying $reference messages"
    fi
fi

# 6. Cargo.lock must be committed: OBS builds --locked and offline.
[ -f Cargo.lock ] || bad "Cargo.lock must be committed for reproducible offline builds"
note "Cargo.lock is present"

if [ "$fail" -ne 0 ]; then
    echo "packaging checks FAILED" >&2
    exit 1
fi
echo "  packaging checks passed"
