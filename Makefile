# Vuo's build and check entry points.
#
# The governing rule, from docs/scope.md §8:
#
#     `make check` runs exactly what CI runs, from a clean checkout, with no
#     phone, no server account, and no network.
#
# Anything that cannot be verified under those conditions is either badly
# layered or belongs behind an explicit opt-in gate. The opt-in gates are
# `make live-test` (needs a real Miniflux), `make rpm` (needs the Sailfish SDK)
# and `make vendor-check` (needs crates.io); none is part of `check`. CI runs
# the last of those as a step of its own.

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
        fuzz-check packaging harbour msrv fuzz-quick live-test vendor-check \
        sonar-reports textart icons rpm vendor clean help

all: check

## check: everything CI runs. No phone, no server, no network.
check: fmt-check clippy test qmllint qml-load fuzz-check packaging lockfile deny
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
packaging: harbour
	scripts/check-packaging.sh

## harbour: the Harbour intake rules that need no device build
harbour:
	scripts/check-harbour.sh
ifeq ($(HAVE_QT),yes)
	@echo "== linked libraries (host build) =="
	# The allowed-libraries rule is decided by the link, so the only way to
	# hold it here is to link. The host's Qt is not the device's, but the
	# thing that goes wrong is the same on both: qttypes passes
	# `-lQt5Widgets` unconditionally, and it stays unless nothing refers to
	# QtWidgets. The host build is the STRICTER of the two -- it is the one
	# that actually constructs qmetaobject's application object, where the
	# device entry point uses SailfishApp's instead.
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

## sonar-reports: the two files SonarQube Cloud imports -- clippy diagnostics
## and coverage -- written to target/sonar/. Needs cargo-llvm-cov, so it is an
## opt-in target rather than part of `check`. See sonar-project.properties.
sonar-reports:
	@echo "== reports for SonarQube Cloud =="
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { \
		echo "cargo-llvm-cov is not installed. Install it with:" >&2; \
		echo "    rustup component add llvm-tools-preview" >&2; \
		echo "    cargo install --locked cargo-llvm-cov" >&2; \
		exit 1; \
	}
	@mkdir -p target/sonar
	# cargo prints each diagnostic ONCE and caches it afterwards, so on a warm
	# target/ this writes an EMPTY report -- which SonarQube imports without
	# complaint as "clippy found nothing". Dropping the three workspace crates
	# costs a recompile of Vuo's own code and keeps the dependency build.
	$(CARGO) clean -p vuo-core -p vuo-shim -p harbour-vuo
	@: > target/sonar/clippy.json
	@echo "-- clippy (core) --"
	# Deliberately without `-- -D warnings`, unlike the `clippy` target: this
	# one reports, and `make check` is the one that refuses. The report is
	# newline-delimited JSON, so the three runs simply append.
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
	# harbour-vuo is excluded: it is an entry point with no tests of its own,
	# and instrumenting it only adds an uncovered main() to the report.
	# third_party/qmetaobject is excluded for the reason given in
	# sonar-project.properties -- it is upstream's code, not ours to cover.
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

## vendor-check: prove third_party/qmetaobject is upstream plus its one patch.
## Needs crates.io, so it is an opt-in gate rather than part of `check`.
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

## live-test: opt-in integration test against a real Miniflux instance.
## Requires VUO_LIVE_BASE_URL and VUO_LIVE_TOKEN.
live-test:
	@test -n "$$VUO_LIVE_BASE_URL" || { echo "set VUO_LIVE_BASE_URL" >&2; exit 1; }
	@test -n "$$VUO_LIVE_TOKEN"    || { echo "set VUO_LIVE_TOKEN" >&2; exit 1; }
	$(CARGO) test -p vuo-core --features live-integration-tests -- --ignored --nocapture

## textart: regenerate the texture masks in qml/art/ from tools/textart/.
## Needs a QML runtime and a display (or xvfb); the results are committed.
textart:
	scripts/render-textart.sh

## icons: re-render the four Harbour icon sizes from icons/harbour-vuo.svg.
## Needs rsvg-convert; the results are committed, because the packaging path
## has no SVG renderer.
icons:
	scripts/render-icons.sh

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
