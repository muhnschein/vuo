#!/bin/sh
# shellcheck disable=SC2154 # prog: set by caller.
# Sourced, not run: container engine for build-in-container.sh and
# verify-release.sh. Caller sets `prog` (message prefix).
#
# pick_engine sets:
#   engine    docker | podman
#   rootless  1 when container root = calling user, else 0
#
# VUO_ENGINE=docker|podman forces one. Else Docker if daemon answers
# (podman-docker shim counts as Podman), else Podman.

pick_engine() {
    engine=${VUO_ENGINE:-}
    if [ -z "$engine" ]; then
        if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
            engine=docker
            docker --version 2>/dev/null | grep -qi podman && engine=podman
        elif command -v podman >/dev/null 2>&1; then
            engine=podman
        else
            echo "$prog: needs Podman or Docker" >&2
            exit 1
        fi
    fi
    case "$engine" in
        docker | podman) ;;
        *) echo "$prog: VUO_ENGINE is '$engine', not docker or podman" >&2; exit 2 ;;
    esac
    command -v "$engine" >/dev/null 2>&1 || {
        echo "$prog: $engine asked for, not installed" >&2
        exit 1
    }

    rootless=0
    if [ "$engine" = podman ]; then
        [ "$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" = true ] && rootless=1
    else
        docker info --format '{{.SecurityOptions}}' 2>/dev/null | grep -q rootless && rootless=1
    fi
    return 0
}

# `run` with engine's defaults. Podman: SELinux labels off (Fedora otherwise
# denies bind-mounted tree); Docker has no such default.
ctr_run() {
    if [ "$engine" = podman ]; then
        podman run --rm --security-opt label=disable "$@"
    else
        docker run --rm "$@"
    fi
    return
}

# Flags that run container as calling user, so files it writes in mounts are
# ours. Rootless Docker: container root is us. Rootless Podman keeps own uid
# via keep-id.
ctr_as_me() {
    if [ "$rootless" = 0 ]; then
        echo "--user $(id -u):$(id -g)"
    elif [ "$engine" = podman ]; then
        echo "--userns=keep-id --user $(id -u):$(id -g)"
    else
        echo "--user 0"
    fi
    return 0
}

# Pulls unless present. Rootless Podman failing on ids: say what fixes it.
ctr_pull() {
    ref=$1
    "$engine" image inspect "$ref" >/dev/null 2>&1 && return 0
    "$engine" pull "$ref" && return 0
    if [ "$engine" = podman ] && [ "$rootless" = 1 ]; then
        echo "$prog: if Podman said 'insufficient UIDs or GIDs': SDK image's build" \
             "user is uid 100000, past the 65536 subordinate ids a user gets. Once:" >&2
        echo "    sudo usermod --add-subuids 100000-599999 --add-subgids 100000-599999 $(id -un)" >&2
        echo "    podman system migrate" >&2
    fi
    return 1
}
