# AGENTS.md

How to work on Vuo. What it is: `README.md`, `docs/`. Read `docs/scope.md`, `docs/architecture.md`, `docs/testing.md` before first change. User's words in chat beat this file.

## Ground rules

- Don't know how to proceed → say "I don't know", stop, ask. Mark guesses as guesses. Unknown beats confident wrong.
- Bugs: find root cause, state it. Symptom fix ≠ fix.
- Good return on cost. No gold-plating, no unasked refactors.
- Options → lay them out, recommend one with reasons. Implement nothing until user clearly picks.
- Vuo = UI for Miniflux. Fetching, parsing, full-text extraction, scheduling stay on server. Non-goals: `docs/scope.md` §3.
- SQLite mirror = UI's single source of truth; UI never waits on network. Mutations go through outbox, replay idempotent. Change that breaks either → stop, ask.

## Done means

In this order; later step only once earlier ones hold:

1. Change implemented.
2. Tests added for it (regression test per bug).
3. Tests proven to bite: each fails with change reverted or mutated. Report how (e.g. "22/22 mutations caught"); name what headless tests can't prove.
4. `make check` green locally.
5. PR CI green (`ci.yml`). Sonar (`sonar.yml`) = report, not gate (`docs/testing.md`): still read it, fix new issues and new-code coverage gaps or say why not.
6. Device: dispatch `.github/workflows/rpm.yml` on PR branch; run RPM on test phone ("Device tests"); give user artifact link and evidence.
7. With RPM: outline briefly what merging this PR risks and how you mitigated it.

Feature reachable from several places (feed list, article list, article page, search, cover) → all of them.

User tests RPM on phone; that test judges feel, lag, transitions. Say exactly what to check on device.

## Device tests

- Test phone: OnePlus 6, SFOS 5.1, via `muhnschein-sfos-harness` workflow `do`, shared with other apps' agents. Its `AGENTS.md` rules every phone run (calling `do`, lease, no blind taps, costly ops); Vuo specifics: its `apps/vuo.md`. Test accounts only. Never run fork or others' PR code there: any job can root phone.
- Tags and leases start with `vuo-` (e.g. tag `vuo-pr50-$(date +%s)`, lease `vuo-pr50`); `do` refuses others.
- Build in: `install_artifact muhnschein/vuo RUN_ID harbour-vuo` (rpm.yml run on PR branch).
- Every run starts from named state, recorded in `state.txt` with RPM name and sha256: `clean` (`wipe harbour-vuo`, then install), `fixture:NAME`, `upgrade:FROM` (release plus fixture, then build over it; required when change touches stored data, schema, settings).
- Footprint (`fs_mark`, `footprint harbour-vuo`): file outside app dirs → add to reset or fail run.
- Motion or timing change: launch with `FRAMELOG`, report `frames`. Never measure frames while recording.
- Harness proves function and frame timing. Feel, lag, transitions stay user's call on device.

## Local checks

- `make check` = exactly what CI runs: no phone, no server, no network (`docs/testing.md`). Needs Qt (qmake, qmllint); without it fails. `SKIP_QT=1` only when Qt truly unavailable; then shim tests, QML load test, shim clippy didn't run. Say so.
- Opt-in gates, not in `check`: `make vendor-check` (crates.io, CI runs it), `make msrv`, `make live-test` (real Miniflux, `VUO_LIVE_BASE_URL`, `VUO_LIVE_TOKEN`), `make fuzz-quick`, `make rpm` (SDK). Say which ran, which skipped.
- Local failure → rerun on untouched `main` before calling it environment.
- `make check` gates catch device-only breakage (Rust floor, `Cargo.lock` v3, Qt 5.6 QML, packaging, Harbour rules, cargo-deny); reasons in `docs/packaging.md`, `docs/sdk-build.md`. Gate red → fix code, never loosen gate.
- Foreign input (server JSON, article HTML, images, URLs): rules in `docs/scope.md` §9. New parser path → test plus fuzz target or seed.
- Miniflux behaviour Vuo relies on: `docs/api-contract.md`. New endpoint or semantics → write it there.
- Tests wait on signals or conditions, not fixed sleeps or wall-clock asserts (those flake on loaded runners).
- New dependency must earn its keep: well maintained, widely used, more than someone's pet project, builds at `Cargo.toml` `rust-version`, passes `deny.toml`. Say why in PR.
- Committed generated files (`translations/*.qm`, `qml/art/`, `store/cover.png`, icons): regenerate with their tool (`docs/packaging.md` "Generated files that are committed"), never hand-edit.

## UI and words

- Silica stock components, built like existing pages (e.g. `qml/pages/EntryListPage.qml`). Jolla app has same pattern → match it. Miniflux web UI = reference for features and setting labels.
- "Design first" / "no PR yet" → mockup on canvas, wait for approval, then code.
- Every `qsTr()` string: `docs/architecture.md` "The words on screen". Changed strings → `lupdate` over all `translations/*.ts`, fill all catalogs, `lrelease`, commit `.qm`. `make check` fails on drift.
- QML tests (`crates/vuo-shim/tests`) stub Silica, no layout: looks unproven until device test. Say so.
- Phone = battery: no always-running animations, timers, polling; stop work when app is in background. One process, no background service (Harbour).

## Comments and docs

- Caveman-full = drop articles, filler, pleasantries, hedging. Fragments OK, short words. Identifiers, paths, code, errors exact. E.g. "`Cargo.lock` stays v3: SDK cargo 1.75 can't read v4."
- Code comments, `docs/*.md` edits, this file: caveman-full.
- Comments: terse fragments. What is true now and why; keep constraints. No changelog, bug story, history (git's job): delete comment rather than turn it into history. Identifiers, paths, flags exact. Item doc comments stay (`missing_docs` denied).
- `README.md`, release notes (`%changelog` in both `rpm/*.spec`), forum posts: plain prose for users, as short and to the point as possible.
- No plans, research notes, scratch files in repo.

## Git and PRs

- One branch, one PR per task. Stacked PR only when asked.
- Unrelated finding → tell user; separate PR or issue, not this PR.
- Commit and PR titles: Conventional Commits, plain language, say outcome: `fix(article): scroll position survives the image viewer`. Types `feat` `fix` `perf` `refactor` `test` `docs` `ci` `build` `chore`; scope = app area or subsystem (`sync`, `outbox`, `article`, `cover`, `rpm`, …). Body says why, plain prose; brief, to the point.
- PR body: the problem in a sentence or two, then how you fixed it. End with the model and harness that did the work.
- Final diff: no debug logging, no leftovers.
- Flaky test → find and fix cause. Never add retries. Never re-run CI until green and call it done. Never skip test or loosen assert. One re-run OK only for infra failure before any test ran (runner lost, apt mirror timeout); say so.
- Merge only on user's explicit go. Merge commit, not squash/rebase.

## Releases

- Only on user's go. Latest `main` commit green first: check main, not just PR.
- Strict semver: nothing breaking → minor; fix only → patch.
- Steps: bump `version` in `Cargo.toml` (lockfiles follow, incl. `crates/vuo-core/fuzz/Cargo.lock`), new `%changelog` entry in both `rpm/harbour-vuo.spec` and `rpm/harbour-vuo-cross.spec` (= release notes), push `v*` tag → `rpm.yml` builds release RPM; `reproduce` must rebuild it byte for byte before publish (`docs/verifying.md`). `make check` catches spec/Cargo drift. Harbour upload stays with user.

## Subagents

- Delegate only with tight, written scope.
- Models: only ones on cost/intelligence Pareto frontier ([Artificial Analysis](https://artificialanalysis.ai/), [Slopalytics](https://slopalytics.com/)). Dominated model = don't use. Cheapest frontier model that does the job well. If user names model for task, use that.
- Cheap-model output gets strong-model review: cheap models dropped facts, inverted conditions.

## Reporting

- Replies to user: caveman-full (see Comments and docs).
- Lead with result. Say what was verified, what wasn't, what is guess.
- Non-green step → name it and why. Never hide.
- Forum bug report → search GitHub issues first, link match.
