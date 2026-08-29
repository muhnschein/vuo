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
    println!("cargo:rerun-if-changed=src/main.rs");
}
