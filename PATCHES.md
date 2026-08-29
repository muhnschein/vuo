# Forked dependencies

Two crates under `third_party/` are copies of published releases with small
patches applied. They are wired in through `[patch.crates-io]` at the bottom of
the workspace `Cargo.toml`, and excluded from the workspace so they are not
swept into `make msrv`, `scripts/check-lockfile.sh` or this workspace's lints.

Both patches exist for one reason: **keeping `libQt5Widgets.so.5` out of the
shipped binary.** Harbour's `allowed_libraries.conf` does not list it, so
`rpmvalidation.sh` fails twice on an unpatched build:

```
ERROR [/usr/bin/harbour-vuo] Cannot link to shared library: libQt5Widgets.so.5
ERROR [libQt5Widgets.so.5()(64bit)] Cannot require shared library: ...
```

This is not specific to Vuo. Any qmetaobject application hits it, which is
probably why the Rust Sailfish apps that exist ship on OpenRepos and Chum
rather than through the store.

## qttypes 0.2.12

`build.rs` linked Qt5Widgets unconditionally, next to Core and Gui, while every
other module is behind a feature:

```rust
 link_lib("Core");
 link_lib("Gui");
-link_lib("Widgets");
 #[cfg(feature = "qtquick")]
 link_lib("Quick");
```

qttypes' own C++ glue never refers to a QtWidgets symbol -- `nm -C -u` over its
`librust_cpp_generated.a` finds none -- so nothing else needed changing here.

## qmetaobject 0.2.10

`src/qtdeclarative.rs` built the `QmlEngine`'s application object as a
`QApplication`, which is QtWidgets. A QML engine does not need it;
`QGuiApplication` is enough, and is what SailfishOS itself uses.

```cpp
-#include <QtWidgets/QApplication>
+#include <QtGui/QGuiApplication>

-std::unique_ptr<QApplication> app;
+std::unique_ptr<QGuiApplication> app;

-    : app(new QApplication(argc, argv))
+    : app(new QGuiApplication(argc, argv))
```

Three lines, and no call sites change: `self->app->exec()` and
`self->app->quit()` re-resolve through the new member type. That is what
removes the second undefined symbol -- Qt5 declares `QApplication::exec()` as a
static member, so it shows up as a Widgets symbol even though `QGuiApplication`
has its own.

## Why both, and not one

They are not independent.

* Dropping `link_lib("Widgets")` alone **fails to link**: `QApplication::QApplication(int&, char**, int)` and `QApplication::exec()` come out of qmetaobject's glue undefined.
* Patching only qmetaobject leaves `-lQt5Widgets` on the link line. It would probably drop out of `DT_NEEDED` via `--as-needed`, but that is a linker behaviour to rely on, not a guarantee -- and the whole point is a binary that passes validation deterministically.

## Versions

Pinned exactly: `qmetaobject = "=0.2.10"`, and qttypes 0.2.12 is what that
resolves to. Both are what the offline registry cache holds, and
`rust-version = "1.75"` constrains upgrades anyway. On a version bump, re-apply
the diffs above to the new sources rather than carrying these directories
forward.

## Verifying the patches still do their job

```
readelf -d <binary> | grep NEEDED     # no libQt5Widgets.so.5
```

`crates/harbour-vuo/tests/` has no coverage of this because it is a property of
the linked artifact, not of any Rust code. `scripts/check-harbour.sh` checks it
against a built binary.

## What is not vendored

`tests/`, `README.md` and `Cargo.toml.orig` are stripped from both copies. They
are not built by a path dependency, and carrying a few hundred KB of upstream
test suites into this repository buys nothing -- the crates' own CI covers
them, and the patched behaviour is verified here by
`scripts/check-harbour.sh` against the linked artifact.
