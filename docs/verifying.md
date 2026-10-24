# Verifying a release

Each release RPM rebuilds from its tag to same file, byte for byte. Check one:

```
$ git clone https://github.com/muhnschein/vuo.git
$ cd vuo
$ scripts/verify-release.sh          # latest release
$ scripts/verify-release.sh 1.2.0    # or given one
```

Needs Linux on x86_64, git, curl, Podman (rootless fine) or Docker. Both
installed: Docker if running; `--podman` or `--docker` picks. First run: ~5 GB
pull (Sailfish SDK image, dropped again after its compiler and sysroot are
lifted into `~/.cache/vuo-sdk`, ~2 GB), ~30 min build. Builds in fresh clone of
tag under `~/.cache/vuo-verify`, never in your tree; checks for 4 GB free.
Elsewhere: `VUO_WORKDIR=/some/big/disk scripts/verify-release.sh`. Not `/tmp`:
often RAM disk, small and shared.

| Result | Exit | Meaning |
|---|---|---|
| `VERIFIED` | 0 | Same file as published. |
| `SAME FILES` | 1 | Every file inside matches; packaging (build time, file dates) differs. A bug: please open an issue with the output. |
| `DIFFERENT` | 2 | Files differ. Please open an issue with the output. |

Releases up to 1.1.1 predate reproducible builds: script refuses them (no
`scripts/build-in-container.sh` at their tag).

Rootless Podman: SDK image's build user is uid 100000, past the 65536
subordinate ids a user gets by default. Pull fails with "insufficient UIDs or
GIDs" until, once, a second range clear of the one in `/etc/subuid`
(overlap: "write to uid_map failed"):

```
$ sudo usermod --add-subuids 1000000-1999999 --add-subgids 1000000-1999999 $USER
$ podman system migrate
```

## An RPM from elsewhere

E.g. one copied off a phone. Checked with GitHub's in one run: one build, each
copy compared with it, a line per copy; exit is worst.

```
$ scripts/verify-release.sh --rpm harbour-vuo-1.2.0-1.aarch64.rpm
github: VERIFIED, same file as the rebuild, byte for byte.
local: VERIFIED, same file as the rebuild, byte for byte.
```

`--no-download`: that copy only.

## What makes it reproducible

`scripts/build-in-container.sh` is the one recipe: local builds, `rpm.yml`,
this check.

- Package build time and file dates: commit time (`SOURCE_DATE_EPOCH`, set by
  script); build host fixed (`rpm/harbour-vuo-cross.spec`).
- Build environment `rpm/Containerfile`: Ubuntu base image by digest, apt
  from `snapshot.ubuntu.com` frozen at one instant, rustup by version and
  sha256, Rust release from `rust-toolchain.toml`. Same paths inside on every
  machine: tree `/build/vuo`, crates `/cargo`, SDK `/sdk`, `-B` dir
  `target/cross-bin`.
- Sailfish SDK image (cross compiler, sysroot) by digest: `rpm/sdk-images`.
- Inputs pinned: `Cargo.lock`, `--locked`; `third_party/` in tree.

Releases pass this same check in CI before publishing (`reproduce` in
`.github/workflows/rpm.yml`: fresh clone, Podman, other runner, other time zone
and umask). `main` weekly (`repro-build` in `.github/workflows/scheduled.yml`).

## What it doesn't cover

- Sailfish SDK's compilers and libraries themselves: you trust Jolla's SDK, as
  every Sailfish app does, and coderus' image of it.
- Ubuntu's and Rust's binaries: pinned and checked by signature or hash, not
  rebuilt.
- Test packages (`Release: 1.<run number>`): Release differs per run by design.
  Same recipe otherwise.
- macOS, Windows, ARM hosts: untested. Images x86_64.
