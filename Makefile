# Vuo's build and check entry points.
#
# The governing rule, from docs/scope.md §8:
#
#     `make check` runs exactly what CI runs, from a clean checkout, with no
#     phone, no server account, and no network.
#
# Anything that cannot be verified under those conditions is either badly
# layered or belongs behind an explicit opt-in gate. The two opt-in gates are
# `make live-test` (needs a real Miniflux) and `make rpm` (needs the Sailfish
# SDK); neither is part of `check`.

CARGO ?= cargo
# The Rust floor the SailfishOS SDK ships. `make msrv` re-checks against it so
# a dependency bump cannot silently break the device build (§7).
MSRV ?= 1.75.0
QMLLINT ?= $(shell command -v qmllint 2>/dev/null || echo /usr/lib/qt5/bin/qmllint)
QMAKE ?= $(shell command -v qmake 2>/dev/null || echo /usr/lib/qt5/bin/qmake)
ARCH ?= aarch64

# The shim links against Qt, so it is checked only where Qt is present.
#
# Missing Qt is a HARD FAILURE by default. It used to be a silent skip, so on a
# runner without qmake three of `check`'s eight subjects -- clippy on vuo-shim,
# the offscreen shim tests, and the QML load test -- printed SKIPPED and the
# target still printed "make check passed" and exited 0. A green line that means
# "I did not check the QML, the shim, or §9.3's textFormat defence" is worse
# than a red one.
#
# `make check SKIP_QT=1` is the explicit opt-out for a machine that genuinely
# has no Qt, and it says so in the summary.
SKIP_QT ?=
HAVE_QT := $(shell test -x "$(QMAKE)" && echo yes)
ifneq ($(HAVE_QT),yes)
ifndef SKIP_QT
$(error qmake not found at $(QMAKE), so the shim, the QML load test and shim clippy \
cannot run. Install qtbase5-dev qtdeclarative5-dev qtdeclarative5-dev-tools \
qml-module-qtquick2, or run `make $(MAKECMDGOALS) SKIP_QT=1` to skip them knowingly)
endif
endif

.PHONY: all check fmt fmt-check clippy test qmllint qml-load shim deny \
        fuzz-check packaging harbour patch-deps msrv fuzz-quick live-test rpm vendor clean help

all: check

## check: everything CI runs. No phone, no server, no network.
check: patch-deps fmt-check clippy test qmllint qml-load fuzz-check packaging harbour lockfile deny
ifeq ($(HAVE_QT),yes)
	@echo "== make check passed =="
else
	@echo "== make check passed, WITHOUT Qt: the shim, the QML load test and shim clippy did NOT run =="
endif

## fmt: format the workspace
fmt:
	$(CARGO) fmt --all

fmt-check:
	@echo "== rustfmt =="
	$(CARGO) fmt --all -- --check

clippy:
	@echo "== clippy (core) =="
	$(CARGO) clippy --workspace --exclude vuo-shim --exclude harbour-vuo --all-targets -- -D warnings
ifeq ($(HAVE_QT),yes)
	@echo "== clippy (shim) =="
	$(CARGO) clippy -p vuo-shim --all-targets -- -D warnings
	@echo "== clippy (app binary) =="
	# `harbour-vuo` is not in default-members and was excluded from every
	# target here, so NOTHING in `make check` compiled the application entry
	# point: main.rs could fail to type-check, or contain an `unimplemented!`
	# that §9.5 denies, and the full gate stayed green. The only thing that
	# built it was a 40-minute SDK build -- the exact failure mode the
	# packaging checks exist to pre-empt. Default features build without the
	# SDK; that is what the `sailfishapp` gate is for.
	$(CARGO) clippy -p harbour-vuo --all-targets -- -D warnings
else
	@echo "== clippy (shim) SKIPPED: no qmake found at $(QMAKE) =="
endif

test:
	@echo "== tests (core) =="
	$(CARGO) test --workspace --exclude vuo-shim --exclude harbour-vuo
ifeq ($(HAVE_QT),yes)
	@echo "== tests (shim, offscreen Qt) =="
	QT_QPA_PLATFORM=offscreen $(CARGO) test -p vuo-shim
else
	@echo "== shim tests SKIPPED: no qmake found at $(QMAKE) =="
endif

## qmllint: syntax-check every QML file
qmllint:
	@echo "== qmllint =="
	@if [ ! -x "$(QMLLINT)" ]; then \
		echo "qmllint not found at $(QMLLINT); install qtdeclarative5-dev-tools" >&2; exit 1; \
	fi
	@find qml qml-stubs -name '*.qml' -print0 | xargs -0 -n1 $(QMLLINT)

## qml-load: compile every QML file in a real engine against the Silica stubs.
## Much stronger than qmllint, which only checks syntax.
qml-load:
ifeq ($(HAVE_QT),yes)
	@echo "== QML load test =="
	QT_QPA_PLATFORM=offscreen $(CARGO) test -p vuo-shim --test qml_loads
else
	@echo "== QML load test SKIPPED: no qmake found at $(QMAKE) =="
endif

## shim: build the Qt-linked shim explicitly
shim:
	$(CARGO) build -p vuo-shim

## fuzz-check: type-check the fuzz targets.
##
## The fuzz crate is a SEPARATE workspace (cargo-fuzz needs its own flags), so
## nothing else in `make check` compiles it -- which meant adding a field to a
## struct a fuzz target constructs broke only in CI. This is a plain
## `cargo check`, no nightly and no sanitizer, so it runs anywhere.
fuzz-check:
	@echo "== fuzz targets type-check =="
	cd crates/vuo-core/fuzz && $(CARGO) check --all-targets

## lockfile: Cargo.lock format and dependency editions the SDK's cargo must read
lockfile:
	@echo "== lockfile (SailfishOS SDK constraints) =="
	scripts/check-lockfile.sh

## packaging: spec, desktop entry and installed-file checks (no SDK needed)
packaging:
	scripts/check-packaging.sh

## patch-deps: materialise third_party/ from patches/ (needed before any build)
##
## `cargo` fails immediately if the paths in [patch.crates-io] are missing, so
## this has to run first. Idempotent, so every target that builds can depend on
## it without cost.
patch-deps:
	scripts/patch-deps.sh

## harbour: the Harbour rules checkable from a built binary (libs, exported main)
##
## The real gate is sdk-harbour-rpmvalidator, which needs an RPM and the SDK.
## This is its fast half, so a regression is caught before a twenty-minute cross
## build rather than after one. Skips cleanly where the binary is not built.
harbour:
	@echo "== harbour binary rules =="
	scripts/check-harbour.sh

## deny: advisories, licences, banned and duplicated crates
deny:
	@echo "== cargo-deny =="
	@if command -v cargo-deny >/dev/null 2>&1; then \
		$(CARGO) deny check; \
	else \
		echo "cargo-deny not installed. Install it with:" >&2; \
		echo "    cargo install --locked cargo-deny" >&2; \
		echo "CI installs it, so a green local run without it is not a green CI run." >&2; \
		exit 1; \
	fi

## msrv: re-check against the SailfishOS Rust floor
msrv:
	@echo "== MSRV check ($(MSRV)) =="
	@rustup toolchain list | grep -q '$(MSRV)' || rustup toolchain install $(MSRV) --profile minimal
	$(CARGO) +$(MSRV) check --workspace --exclude vuo-shim --exclude harbour-vuo --locked

## fuzz-quick: short fuzz run over the two parsers, as PR CI does
fuzz-quick:
	@echo "== fuzz (60s per target) =="
	scripts/fuzz-seed.sh content_transform entry_deserialise
	cd crates/vuo-core/fuzz && \
		for target in content_transform entry_deserialise; do \
			$(CARGO) +nightly fuzz run $$target \
				-- -max_total_time=60 -dict=$$target.dict || exit 1; \
		done

## live-test: opt-in integration test against a real Miniflux instance.
## Requires VUO_LIVE_BASE_URL and VUO_LIVE_TOKEN.
live-test:
	@test -n "$$VUO_LIVE_BASE_URL" || { echo "set VUO_LIVE_BASE_URL" >&2; exit 1; }
	@test -n "$$VUO_LIVE_TOKEN"    || { echo "set VUO_LIVE_TOKEN" >&2; exit 1; }
	$(CARGO) test -p vuo-core --features live-integration-tests -- --ignored --nocapture

## rpm: build a device RPM. Needs the SailfishOS SDK (Docker build engine).
rpm:
	scripts/build-rpm.sh $(ARCH)

## vendor: produce the offline crate bundle OBS builds need
vendor:
	scripts/vendor-crates.sh

clean:
	$(CARGO) clean
	rm -rf rpm/vendor.tar.xz rpm/vendor.toml vendor/

help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
