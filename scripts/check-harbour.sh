#!/usr/bin/env bash
# Harbour intake rules checkable without device build.
#
# Harbour runs `rpmvalidation.sh` (sailfishos/sdk-harbour-rpmvalidator) on
# submitted RPM. Most of it decided in repo, not compiler (spec install paths,
# .desktop declarations, QML imports); held here on every `make check`, not
# found by rejected submission.
#
# CAN'T check device-link things: needed shared libraries, glibc symbol
# versions, stripping. scripts/cross-build.sh checks those (only machine with
# cross toolchain).
#
# Rules transcribed from validator config as of 2026-09-06 (rpmvalidation.conf,
# allowed_sailjailkeys.conf, allowed_permissions.conf, disallowed_orgnames.conf,
# allowed_qmlimports.conf). Transcribed, not fetched: `make check` runs offline
# (docs/scope.md §8). Re-check against upstream before submission.
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
# rpmvalidation.sh:447-456 rejects every packaged file outside these four:
# "Installation not allowed in this location". Includes %{_defaultlicensedir}
# (rpm `%license` target); why licence text goes in app datadir.
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

# Every destination either spec writes, every path either %files claims. Derived
# from specs: new install line checked automatically.
checked=0
for spec in rpm/harbour-vuo.spec rpm/harbour-vuo-cross.spec; do
    while IFS= read -r raw; do
        [[ -n "$raw" ]] || continue
        # Expand macros specs use. `${RES}` = icon loop variable; any size
        # satisfies icon rule, one stands in.
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
        # `desktop-file-install --dir <dir>` names DIRECTORY; file written = one
        # named on same line.
        if [[ "$p" = "usr/share/applications" ]]; then
            p="$p/$(sed 's/#.*//' "$spec" | grep -oE 'desktop-file-install .*' \
                | grep -oE '[A-Za-z0-9._-]+\.desktop$' | head -1)"
        fi
        # Trailing directory (translations/, icons/) fine if directory allowed.
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

# Section body: keys, then two shaped values.
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
                # Any other organisation name applicant's choice.
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
# Import off Harbour list = unpublishable; nothing else notices (resolves on
# device, Vuo stubs supply it here).
#
# Short allowlist of Vuo's imports, not copy of upstream file: stop NEW import
# going in unexamined.
while IFS= read -r import; do
    case "$import" in
        '"'*) continue ;;                  # a relative path import; checked below
        'QtQuick 2.6'|'Sailfish.Silica 1.0'|'Sailfish.WebView 1.0') ;;
        # New-articles notification. On upstream list under "Nemo QML modules"
        # (allowed_qmlimports.conf, checked 2026-09-22).
        'Nemo.Notifications 1.0') ;;
        'Vuo 1.0') ;;                      # registered by the app itself
        *) bad "qml/ imports '$import', which is not on Harbour's allowed list \
(check allowed_qmlimports.conf before adding it)" ;;
    esac
done < <(grep -rhE '^[[:space:]]*import[[:space:]]' qml/ \
    | sed -e 's/^[[:space:]]*import[[:space:]]*//' -e 's/[[:space:]]\+/ /g' \
          -e 's/ as .*$//' -e 's/;$//' | sort -u)

# Relative imports must stay inside app tree once installed.
while IFS=$'\t' read -r file target; do
    case "$target" in
        /*) bad "$file: absolute path imports are forbidden" ; continue ;;
        # Relative import wanted here; checked below.
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
    # IHDR width and height, big-endian, offsets 16 and 20.
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
    # `%license` and `%doc` in %files name no path: rpm invents one under
    # %{_defaultlicensedir}/%{_defaultdocdir}, both outside Harbour's allowed
    # set. They slip past path check above; caught here.
    while IFS= read -r banned; do
        [[ -n "$banned" ]] && bad "$spec uses '$banned' in %files; rpm would install it \
outside /usr/share/harbour-vuo, which Harbour rejects"
    done < <(grep -oE '^%(license|doc)\b' <<< "$body" || true)
done
note "neither spec uses a scriptlet, a trigger, or a dependency kind Harbour rejects"

# ------------------------------------------------------- 6. sandbox paths
# rpmvalidation.sh:1186 greps every shipped file for hardcoded home dir. Binary
# checked where linked; these = files repo ships verbatim.
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
