# Vuo build and check entry points.
#
# Rule, docs/scope.md §8:
#
#     `make check` runs exactly what CI runs, from a clean checkout, with no
#     phone, no server account, and no network.
#
# Anything unverifiable that way is badly layered or behind explicit opt-in
# gate: `make live-test` (real Miniflux), `make rpm` (Sailfish SDK), `make
# vendor-check` (crates.io). None in `check`; CI runs vendor-check as own step.

CARGO ?= cargo
# The Rust floor: the workspace's `rust-version`, read from Cargo.toml as the
# CI msrv job reads it, so the two cannot disagree. `make msrv` re-checks
# against it so a dependency bump cannot silently break the device build (§7).
MSRV ?= $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1)
QMLLINT ?= $(shell command -v qmllint 2>/dev/null || echo /usr/lib/qt5/bin/qmllint)
QMAKE ?= $(shell command -v qmake 2>/dev/null || echo /usr/lib/qt5/bin/qmake)
ARCH ?= aarch64

# Shim links Qt, so checked only where Qt is present.
#
# Missing Qt = HARD FAILURE by default. Silent skip would print "make check
# passed" without shim clippy, shim tests, QML load test (§9.3 textFormat
# defence): green that checked nothing is worse than red.
#
# `make check SKIP_QT=1`: explicit opt-out for machine without Qt; summary says
# so.
#
# Refusal fires only for Qt goals. Qt-free opt-in gates (live-test,
# vendor-check, msrv, fuzz-quick, rest below) must run anywhere, e.g. Miniflux
# integration job without Qt.
SKIP_QT ?=
HAVE_QT := $(shell test -x "$(QMAKE)" && echo yes)
# Goals bearing on Qt: shim and its clippy, QML tests, aggregates pulling them
# in. Rest Qt-free on host.
QT_FREE_GOALS := fmt fmt-check fuzz-check lockfile deny workflow-lint vendor-check \
                 msrv fuzz-quick live-test icons rpm vendor clean help
QT_GOALS := $(filter-out $(QT_FREE_GOALS),$(if $(MAKECMDGOALS),$(MAKECMDGOALS),all))
ifneq ($(HAVE_QT),yes)
ifneq ($(QT_GOALS),)
ifndef SKIP_QT
$(error qmake not found at $(QMAKE), so the shim, the QML load test and shim clippy \
cannot run. Install qtbase5-dev qtdeclarative5-dev qtdeclarative5-dev-tools \
qml-module-qtquick2, or run `make $(MAKECMDGOALS) SKIP_QT=1` to skip them knowingly)
endif
endif
endif

.PHONY: all check fmt fmt-check clippy test qmllint qml-load shim deny \
        workflow-lint fuzz-check packaging harbour msrv fuzz-quick live-test \
        vendor-check leak-check sonar-reports textart icons rpm vendor clean help

all: check

## check: everything CI runs. No phone, no server, no network.
check: fmt-check clippy test qmllint qml-load fuzz-check packaging lockfile deny \
       workflow-lint
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
	# `harbour-vuo` not in default-members; without this nothing in `make check`
	# compiles app entry point (type errors, §9.5-denied `unimplemented!`). Default
	# features build without SDK; `sailfishapp` gate is for SDK.
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

## qml-load: compile every QML file in real engine against Silica stubs. Much
## stronger than qmllint (syntax only).
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
## Fuzz crate is SEPARATE workspace (cargo-fuzz flags); nothing else in `make
## check` compiles it. Plain `cargo check`, no nightly or sanitizer: runs
## anywhere.
fuzz-check:
	@echo "== fuzz targets type-check =="
	cd crates/vuo-core/fuzz && $(CARGO) check --all-targets

## lockfile: Cargo.lock format and dependency editions the SDK's cargo must read
lockfile:
	@echo "== lockfile (SailfishOS SDK constraints) =="
	scripts/check-lockfile.sh

## packaging: spec, desktop entry and installed-file checks (no SDK needed)
packaging: harbour
	scripts/check-packaging.sh

## harbour: the Harbour intake rules that need no device build
harbour:
	scripts/check-harbour.sh
ifeq ($(HAVE_QT),yes)
	@echo "== linked libraries (host build) =="
	# Allowed-libraries rule decided by link, so link here. Host Qt isn't device's,
	# but failure same: qttypes passes `-lQt5Widgets` unconditionally; stays unless
	# nothing refers to QtWidgets. Host build STRICTER: constructs qmetaobject's
	# app object; device entry point uses SailfishApp's.
	$(CARGO) build -p harbour-vuo --bin harbour-vuo
	scripts/check-linked-libs.sh target/debug/harbour-vuo
else
	@echo "== linked libraries SKIPPED: no qmake found at $(QMAKE) =="
endif

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

## workflow-lint: .github/ workflows through zizmor (`${{ }}` pasted into
## script, token left in .git/config, cache a release build trusts). Offline;
## scheduled.yml asks GitHub the rest weekly.
workflow-lint:
	@echo "== zizmor =="
	@if command -v zizmor >/dev/null 2>&1; then \
		zizmor --offline .github/workflows/; \
	else \
		echo "zizmor not installed. Install it with:" >&2; \
		echo "    cargo install --locked zizmor    (or: pipx install zizmor)" >&2; \
		echo "CI installs it, so a green local run without it is not a green CI run." >&2; \
		exit 1; \
	fi

## leak-check: every test under AddressSanitizer + LeakSanitizer, pinned
## nightly. Slow, not in `check`; scheduled.yml runs weekly. See
## scripts/leak-check.sh.
leak-check:
	scripts/leak-check.sh

## sonar-reports: the two files SonarQube Cloud imports (clippy diagnostics,
## coverage) into target/sonar/. Needs cargo-llvm-cov: opt-in, not in `check`.
## See sonar-project.properties.
sonar-reports:
	@echo "== reports for SonarQube Cloud =="
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { \
		echo "cargo-llvm-cov is not installed. Install it with:" >&2; \
		echo "    rustup component add llvm-tools-preview" >&2; \
		echo "    cargo install --locked cargo-llvm-cov" >&2; \
		exit 1; \
	}
	@mkdir -p target/sonar
	# cargo prints each diagnostic ONCE, then caches: warm target/ = EMPTY report,
	# imported as "clippy found nothing". Clean three workspace crates; deps stay
	# built.
	$(CARGO) clean -p vuo-core -p vuo-shim -p harbour-vuo
	@: > target/sonar/clippy.json
	@echo "-- clippy (core) --"
	# No `-- -D warnings` (unlike `clippy`): this reports, `make check` refuses.
	# Newline-delimited JSON, so three runs append.
	$(CARGO) clippy --workspace --exclude vuo-shim --exclude harbour-vuo \
		--all-targets --message-format=json >> target/sonar/clippy.json
ifeq ($(HAVE_QT),yes)
	@echo "-- clippy (shim) --"
	$(CARGO) clippy -p vuo-shim --all-targets --message-format=json \
		>> target/sonar/clippy.json
	@echo "-- clippy (app binary) --"
	$(CARGO) clippy -p harbour-vuo --all-targets --message-format=json \
		>> target/sonar/clippy.json
	@echo "-- coverage (core and shim) --"
	# harbour-vuo excluded: entry point, no tests, only adds uncovered main().
	# third_party/qmetaobject excluded: upstream's code (sonar-project.properties).
	QT_QPA_PLATFORM=offscreen $(CARGO) llvm-cov --workspace \
		--exclude harbour-vuo \
		--ignore-filename-regex '(^|/)third_party/' \
		--lcov --output-path target/sonar/lcov.info
else
	@echo "-- shim and app clippy SKIPPED: no qmake found at $(QMAKE) --"
	@echo "-- coverage (core only) --"
	$(CARGO) llvm-cov --workspace --exclude vuo-shim --exclude harbour-vuo \
		--ignore-filename-regex '(^|/)third_party/' \
		--lcov --output-path target/sonar/lcov.info
endif
	@echo "== wrote target/sonar/clippy.json and target/sonar/lcov.info =="

## vendor-check: prove third_party/qmetaobject = upstream + its one patch. Needs
## crates.io: opt-in gate, not in `check`.
vendor-check:
	scripts/check-vendored.sh

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

## live-test: opt-in integration test against real Miniflux. Needs
## VUO_LIVE_BASE_URL and VUO_LIVE_TOKEN.
live-test:
	@test -n "$$VUO_LIVE_BASE_URL" || { echo "set VUO_LIVE_BASE_URL" >&2; exit 1; }
	@test -n "$$VUO_LIVE_TOKEN"    || { echo "set VUO_LIVE_TOKEN" >&2; exit 1; }
	$(CARGO) test -p vuo-core --features live-integration-tests -- --ignored --nocapture

## textart: regenerate texture masks in qml/art/ from tools/textart/. Needs QML
## runtime and display (or xvfb); results committed.
textart:
	scripts/render-textart.sh

## icons: re-render four Harbour icon sizes from icons/harbour-vuo.svg. Needs
## rsvg-convert; results committed (packaging path has no SVG renderer).
icons:
	scripts/render-icons.sh

## rpm: build device RPM. Needs SailfishOS SDK (Docker build engine).
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
