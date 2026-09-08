#!/usr/bin/env bash
# The Harbour intake rules that can be checked without building for a device.
#
# Harbour runs `rpmvalidation.sh` from sailfishos/sdk-harbour-rpmvalidator over
# the submitted RPM. Most of what it looks at is decided in this repository
# rather than by the compiler -- which paths the spec installs to, what the
# .desktop file declares, which QML modules the pages import -- so those can be
# held here, on every `make check`, instead of being discovered by a rejected
# submission.
#
# What it CANNOT check is anything that needs the device link: the shared
# libraries the binary ends up needing, its glibc symbol versions, whether it
# is stripped. Those are checked by scripts/cross-build.sh, on the one machine
# that has the cross toolchain.
#
# The rules below are transcribed from the validator's own configuration as of
# 2026-09-06 (rpmvalidation.conf, allowed_sailjailkeys.conf,
# allowed_permissions.conf, disallowed_orgnames.conf, allowed_qmlimports.conf).
# Transcribed, not fetched: `make check` runs with no network (docs/scope.md
# §8). Re-read them against upstream when a submission is being prepared.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
NAME=harbour-vuo
fail=0
note() { echo "  $*"; }
bad() { echo "FAIL: $*" >&2; fail=1; }

echo "== Harbour rules =="

# ---------------------------------------------------------------- 1. paths
#
# rpmvalidation.sh:447-456 rejects every packaged file that is not one of
# these four, with "Installation not allowed in this location". That includes
# %{_defaultlicensedir}, which is where rpm's own `%license` puts a file --
# the reason the licence text is installed into the app's datadir instead.
path_allowed() {
    local path=$1
    case "$path" in
        "usr/bin/$NAME") return 0 ;;
        "usr/share/$NAME"|"usr/share/$NAME"/*) return 0 ;;
        "usr/share/applications/$NAME.desktop") return 0 ;;
        usr/share/icons/hicolor/*/apps/"$NAME.png") return 0 ;;
        *) return 1 ;;
    esac
}

# Every destination either spec writes to, and every path either %files
# section claims. Derived from the specs, so a new install line is checked
# without anyone remembering to add it here.
checked=0
for spec in rpm/harbour-vuo.spec rpm/harbour-vuo-cross.spec; do
    while IFS= read -r raw; do
        [[ -n "$raw" ]] || continue
        # Expand the macros the specs actually use. `${RES}` is the icon
        # loop's variable; any size satisfies the icon rule, so one stands in.
        p=$raw
        p=${p//%\{buildroot\}/}
        p=${p//%\{_bindir\}/\/usr\/bin}
        p=${p//%\{_datadir\}/\/usr\/share}
        p=${p//%\{name\}/$NAME}
        p=${p//\$\{RES\}/86x86}
        p=${p//\{\}/x}          # find -exec placeholder: any file under ./qml
        p=${p//.\/qml/qml}
        p=${p#/}
        p=${p%/}
        # `desktop-file-install --dir <dir>` names the DIRECTORY; the file it
        # writes there is the one named on the same line.
        if [[ "$p" = "usr/share/applications" ]]; then
            p="$p/$(sed 's/#.*//' "$spec" | grep -oE 'desktop-file-install .*' \
                | grep -oE '[A-Za-z0-9._-]+\.desktop$' | head -1)"
        fi
        # A trailing directory (translations/, icons/) is fine if the
        # directory itself is allowed.
        checked=$((checked + 1))
        path_allowed "$p" \
            || bad "$spec would package /$p, which Harbour does not allow"
    done < <(
        sed 's/#.*//' "$spec" \
            | sed -e :a -e '/\\$/N; s/\\\n/ /; ta' \
            | grep -oE '(%\{buildroot\}[^ "]+|^%\{_[a-z]+\}[^ ]*|^/usr/[^ ]*)' \
            | sort -u
    )
done
[[ "$checked" -ge 8 ]] || bad "the path check only examined $checked entries; the spec parse must have failed"
note "every path the specs install to is one Harbour allows ($checked checked)"

# ------------------------------------------------------------ 2. .desktop
D=$NAME.desktop
grep -q "^Type=Application[[:space:]]*$" "$D" || bad "$D needs Type=Application"
grep -q "^Icon=$NAME[[:space:]]*$" "$D" || bad "$D needs Icon=$NAME"
grep -qE "^Exec=$NAME([[:space:]]|$)" "$D" || bad "$D needs Exec=$NAME"
grep -q "^\[Sailjail\]$" "$D" && bad "$D must use [X-Sailjail], not [Sailjail]"
grep -q "^\[X-Sailjail\]$" "$D" || bad "$D has no [X-Sailjail] section"

# The section's own body: keys, then the two values with a shape.
sailjail=$(sed '1,/^\[X-Sailjail\]/d;/^\[/,$d' "$D" | grep -E '^[A-Za-z]+=' || true)
[[ -n "$sailjail" ]] || bad "$D has an empty [X-Sailjail] section"
while IFS='=' read -r key value; do
    case "$key" in
        Permissions)
            # allowed_permissions.conf.
            for perm in ${value//;/ }; do
                case "$perm" in
                    Audio|Bluetooth|Camera|Internet|Location|MediaIndexing|Microphone|NFC|\
RemovableMedia|UserDirs|WebView|Documents|Downloads|Music|Pictures|PublicDir|Videos|\
Compatibility|Secrets|Contacts|Accounts) ;;
                    *) bad "$D declares the Sailjail permission '$perm', which is not allowed" ;;
                esac
            done
            ;;
        OrganizationName)
            [[ $value =~ ^[0-9a-z._-]+$ ]] \
                || bad "$D: OrganizationName '$value' has characters Harbour rejects"
            [[ $value =~ (^|[.])[0-9] ]] \
                && bad "$D: no OrganizationName component may start with a digit"
            case "$value" in
                com.jolla|org.sailfishos) bad "$D: OrganizationName '$value' is reserved" ;;
                # Every other organisation name is the applicant's to choose.
                *) ;;
            esac
            ;;
        ApplicationName)
            [[ $value =~ ^[A-Za-z_-][A-Z0-9a-z_-]*$ ]] \
                || bad "$D: ApplicationName '$value' has characters Harbour rejects"
            ;;
        ExecDBus) ;;
        *) bad "$D: '$key' is not an allowed key in [X-Sailjail]" ;;
    esac
done <<< "$sailjail"
note "the desktop entry's Harbour and Sailjail declarations are allowed"

# --------------------------------------------------------- 3. QML imports
#
# An import that is not on Harbour's list makes the app unpublishable, and
# nothing else in the build would notice: it resolves on the device, where the
# module is present, and Vuo's own stubs supply it here.
#
# A short allowlist of what Vuo uses rather than a copy of the whole upstream
# file: the point is to stop a NEW import going in unexamined.
while IFS= read -r import; do
    case "$import" in
        '"'*) continue ;;                  # a relative path import; checked below
        'QtQuick 2.6'|'Sailfish.Silica 1.0'|'Sailfish.WebView 1.0') ;;
        'Vuo 1.0') ;;                      # registered by the app itself
        *) bad "qml/ imports '$import', which is not on Harbour's allowed list \
(check allowed_qmlimports.conf before adding it)" ;;
    esac
done < <(grep -rhE '^[[:space:]]*import[[:space:]]' qml/ \
    | sed -e 's/^[[:space:]]*import[[:space:]]*//' -e 's/[[:space:]]\+/ /g' \
          -e 's/ as .*$//' -e 's/;$//' | sort -u)

# Relative imports must stay inside the app's own tree once installed.
while IFS=$'\t' read -r file target; do
    case "$target" in
        /*) bad "$file: absolute path imports are forbidden" ; continue ;;
        # A relative import is what this rule wants; it is checked below.
        *) ;;
    esac
    [[ -d "$(dirname "$file")/$target" ]] \
        || bad "$file imports '$target', which is not a directory in qml/"
done < <(grep -rn '^[[:space:]]*import[[:space:]]*"' qml/ \
    | sed -e 's/:[0-9]*:[[:space:]]*import[[:space:]]*"/\t/' -e 's/".*$//')
note "every QML import is one Harbour allows, and every relative one stays inside qml/"

# ---------------------------------------------------------------- 4. icons
# rpmvalidation.sh:667 requires all four sizes, each a PNG of exactly that size.
for size in 86x86 108x108 128x128 172x172; do
    icon="icons/$size/$NAME.png"
    if [[ ! -s "$icon" ]]; then
        bad "$icon is missing; Harbour wants all four sizes"
        continue
    fi
    # The IHDR width and height, big-endian, at offsets 16 and 20.
    dims=$(od -An -tu1 -j16 -N8 "$icon" | tr -s ' ' | sed 's/^ //')
    set -- $dims
    w=$(( ($1<<24) + ($2<<16) + ($3<<8) + $4 ))
    h=$(( ($5<<24) + ($6<<16) + ($7<<8) + $8 ))
    [[ "${w}x${h}" = "$size" ]] || bad "$icon is ${w}x${h}, but must be $size"
done
note "all four icon sizes are present and are PNGs of the right size"

# ----------------------------------------------------- 5. RPM constructs
# Scriptlets and triggers are rejected outright (rpmvalidation.sh:1058-1071),
# as are these dependency kinds (:1106-1114).
for spec in rpm/harbour-vuo.spec rpm/harbour-vuo-cross.spec; do
    body=$(sed 's/#.*//' "$spec")
    while IFS= read -r banned; do
        [[ -n "$banned" ]] && bad "$spec uses '$banned', which Harbour does not allow"
    done < <(grep -oE '^%(pre|post|preun|postun|pretrans|posttrans|triggerin|triggerun|transfiletriggerin)\b' <<< "$body" || true)
    while IFS= read -r banned; do
        [[ -n "$banned" ]] && bad "$spec declares '$banned', which Harbour does not allow"
    done < <(grep -oE '^(Obsoletes|Conflicts|Recommends|Suggests|Supplements|Enhances):' <<< "$body" || true)
    # `%license` and `%doc` in %files do not name a path -- rpm invents one,
    # under %{_defaultlicensedir} or %{_defaultdocdir}, and both are outside
    # what Harbour allows. They therefore slip past the path check above,
    # which is exactly how the licence text came to be packaged there.
    while IFS= read -r banned; do
        [[ -n "$banned" ]] && bad "$spec uses '$banned' in %files; rpm would install it \
outside /usr/share/harbour-vuo, which Harbour rejects"
    done < <(grep -oE '^%(license|doc)\b' <<< "$body" || true)
done
note "neither spec uses a scriptlet, a trigger, or a dependency kind Harbour rejects"

# ------------------------------------------------------- 6. sandbox paths
# rpmvalidation.sh:1186 greps every shipped file for a hardcoded home
# directory. The binary is checked where it is linked; these are the files
# this repository ships verbatim.
if grep -rn "/home/nemo\|/home/defaultuser" qml/ "$D" >/dev/null 2>&1; then
    bad "a shipped file hardcodes a home directory; use QStandardPaths instead"
else
    note "nothing shipped hardcodes /home/nemo or /home/defaultuser"
fi

if [[ "$fail" -ne 0 ]]; then
    echo "Harbour rule checks FAILED" >&2
    exit 1
fi
echo "  Harbour rule checks passed"
