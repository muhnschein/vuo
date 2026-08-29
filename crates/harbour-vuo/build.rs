fn main() {
    // The C++ glue is compiled only for the device build. On a host without
    // the SailfishOS SDK there are no sailfishapp headers, and requiring them
    // unconditionally would make the whole workspace unbuildable off-device --
    // which is exactly what §5 says the layering must avoid.
    #[cfg(feature = "sailfishapp")]
    {
        let mut config = cpp_build::Config::new();
        config.include("/usr/include/sailfishapp");

        // sailfishapp.h's first line is `#include <QtGlobal>`, so the Qt
        // headers have to be on the include path too. Only
        // /usr/include/sailfishapp was passed, and the device build died on
        // "QtGlobal: No such file or directory" after compiling every Rust
        // crate -- the last step of a twenty-minute cross build.
        //
        // QT_INCLUDE_PATH is what the spec exports for qttypes (it is how that
        // crate is told to skip `qmake -query`, which cannot be exec'd under
        // sb2). Reusing it keeps one source of truth. The per-module
        // subdirectories are needed because Qt headers include each other
        // unqualified.
        let qt_include = std::env::var("QT_INCLUDE_PATH")
            .unwrap_or_else(|_| "/usr/include/qt5".to_owned());
        config.include(&qt_include);
        for module in ["QtCore", "QtGui", "QtQuick", "QtQml"] {
            config.include(format!("{qt_include}/{module}"));
        }

        config.build("src/main.rs");
        println!("cargo:rustc-link-lib=sailfishapp");
        println!("cargo:rerun-if-env-changed=QT_INCLUDE_PATH");
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
}
