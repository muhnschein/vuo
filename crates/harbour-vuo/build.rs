fn main() {
    // The C++ glue is compiled only for the device build. On a host without
    // the SailfishOS SDK there are no sailfishapp headers, and requiring them
    // unconditionally would make the whole workspace unbuildable off-device --
    // which is exactly what §5 says the layering must avoid.
    #[cfg(feature = "sailfishapp")]
    {
        cpp_build::Config::new()
            .include("/usr/include/sailfishapp")
            .build("src/main.rs");
        println!("cargo:rustc-link-lib=sailfishapp");
    }
    println!("cargo:rerun-if-changed=src/main.rs");
}
