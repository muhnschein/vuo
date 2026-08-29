fn main() {
    // The C++ glue is compiled only for the device build. On a host without
    // the SailfishOS SDK there are no sailfishapp headers, and requiring them
    // unconditionally would make the whole workspace unbuildable off-device --
    // which is exactly what §5 says the layering must avoid.
    #[cfg(feature = "sailfishapp")]
    {
        // Inside the SDK these are absolute paths in the target rootfs, which
        // sb2 maps for us, so the default prefix is empty and behaviour there
        // is unchanged. A cross-build driven from OUTSIDE sb2 -- which is how
        // an RPM gets produced while the SDK tooling's cargo 1.75 cannot parse
        // this lockfile (docs/sdk-build.md) -- sees the same rootfs as a plain
        // directory, and needs every include rewritten under it.
        let sysroot = std::env::var("VUO_SYSROOT").unwrap_or_default();

        let mut config = cpp_build::Config::new();
        config.include(format!("{sysroot}/usr/include/sailfishapp"));
        // `main.rs`'s cpp! block includes <QtQuick>, <QGuiApplication> and
        // <QQuickView>. qmetaobject's own crates find Qt through
        // QT_INCLUDE_PATH, but that does not reach this Config -- and this
        // block has never been compiled anywhere, because the SDK build has
        // never got past the Rust version, so nothing has needed it until now.
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
        // Both of these change what the block above compiles against, and
        // neither is a file cargo watches on its own.
        println!("cargo:rerun-if-env-changed=QT_INCLUDE_PATH");
        println!("cargo:rerun-if-env-changed=VUO_SYSROOT");
    }
    // Harbour's validatesymbols (rpmvalidation.sh:807-812) is a hard ERROR --
    // not a warning -- when the binary does not export `main`, because every
    // QML file imports Sailfish.Silica and that flips the branch. It is also a
    // real launch bug: harbour-vuo.desktop declares
    // `X-Nemo-Application-Type=silica-qt5`, so the app starts through the
    // mdeclarativecache booster, which dlopens the binary and looks up `main`.
    //
    // `[profile.release] strip = true` deletes .symtab, and RPM's own
    // %__os_install_post strips again at package time, so turning strip off is
    // not a fix -- validatelibraries then warns "file is not stripped!". The
    // symbol has to be in .dynsym.
    //
    // `--dynamic-list` rather than `--export-dynamic-symbol`: the latter needs
    // binutils >= 2.35 and an unrecognised linker option would hard-fail the
    // device link, while --dynamic-list has worked since binutils 2.16. And
    // rather than -rdynamic, which would export everything.
    //
    // Outside the sailfishapp cfg on purpose: the export is wanted for every
    // build that produces the shipped binary.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_owned());
    let dynlist = std::path::Path::new(&manifest_dir).join("main.dynlist");
    println!(
        "cargo:rustc-link-arg-bins=-Wl,--dynamic-list={}",
        dynlist.display()
    );
    println!("cargo:rerun-if-changed=main.dynlist");
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-env-changed=VUO_SYSROOT");
    println!("cargo:rerun-if-env-changed=QT_INCLUDE_PATH");
}
