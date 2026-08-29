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
    }
    println!("cargo:rerun-if-changed=src/main.rs");
    println!("cargo:rerun-if-env-changed=VUO_SYSROOT");
    println!("cargo:rerun-if-env-changed=QT_INCLUDE_PATH");
}
