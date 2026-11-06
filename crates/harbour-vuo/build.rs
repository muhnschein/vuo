//! Build script for app binary: compiles C++ glue in `src/main.rs` against
//! `sailfishapp` and Qt when `sailfishapp` feature on; links `--as-needed` so
//! unused Qt libs aren't recorded as needed.
// Build script talks to cargo via `cargo:` lines on stdout.
#![allow(clippy::print_stdout)]

fn main() {
    // C++ glue only for device build. Host has no sailfishapp headers;
    // requiring them would make workspace unbuildable off-device (§5 layering).
    #[cfg(feature = "sailfishapp")]
    {
        // Inside SDK: absolute rootfs paths, sb2 maps them; default prefix
        // empty. Cross-build from OUTSIDE sb2 (how RPM gets built, SDK cargo
        // 1.75 can't parse lockfile, docs/sdk-build.md) sees rootfs as plain
        // dir: rewrite every include under it.
        let sysroot = std::env::var("VUO_SYSROOT").unwrap_or_default();

        let mut config = cpp_build::Config::new();
        config.include(format!("{sysroot}/usr/include/sailfishapp"));
        // `main.rs` cpp! block includes `<QtQuick>`, `<QGuiApplication>`,
        // `<QQuickView>`. qmetaobject finds Qt via QT_INCLUDE_PATH, but that
        // doesn't reach this Config.
        let qt = std::env::var("QT_INCLUDE_PATH")
            .unwrap_or_else(|_| format!("{sysroot}/usr/include/qt5"));
        for module in ["", "/QtCore", "/QtGui", "/QtQml", "/QtQuick"] {
            config.include(format!("{qt}{module}"));
        }
        config.build("src/main.rs");

        println!("cargo:rustc-link-lib=sailfishapp");
        if !sysroot.is_empty() {
            println!("cargo:rustc-link-search=native={sysroot}/usr/lib64");
        }
    }
    // Drop libraries no symbol in binary refers to.
    //
    // `qttypes` passes `-lQt5Widgets` unconditionally (its build.rs). Vendored
    // qmetaobject doesn't use `QApplication`, so nothing refers to QtWidgets;
    // this drops DT_NEEDED entry that fails Harbour allowed-libraries check
    // (see docs/packaging.md). Order matters: `rustc-link-arg-bins` lands ahead
    // of build scripts' `-l` flags, only position where `--as-needed` affects
    // them.
    //
    // Prunes ONLY unneeded libs; can't hide a real dependency.
    println!("cargo:rustc-link-arg-bins=-Wl,--as-needed");

    // Export `main` in .dynsym. Booster (mapplauncherd, `silica-qt5` in .desktop)
    // dlopen()s binary, dlsym()s `main`; Harbour rejects binary without it
    // (rpmvalidation.sh "must export main() symbol"). Rust bin keeps `main` only
    // in .symtab, which `strip = true` drops. `--dynamic-list`: any GNU ld and
    // lld take it; `--export-dynamic-symbol` needs binutils >= 2.35. Not
    // `-rdynamic`: exports every symbol. scripts/check-main-export.sh guards.
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    println!("cargo:rustc-link-arg-bins=-Wl,--dynamic-list={dir}/main.dynlist");
    println!("cargo:rerun-if-changed=main.dynlist");

    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-env-changed=VUO_SYSROOT");
    println!("cargo:rerun-if-env-changed=QT_INCLUDE_PATH");
}
