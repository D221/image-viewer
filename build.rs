fn main() {
    // turbojpeg-sys builds libjpeg-turbo from source via CMake without setting
    // the CRT linkage, so the static turbojpeg lib comes out linked against the
    // static CRT (/MT -> LIBCMT, /MTd -> LIBCMTD). Rust MSVC binaries use the
    // dynamic CRT (/MD -> MSVCRT), and the linker warns LNK4098 about the
    // conflict. Dropping the conflicting defaultlib makes Rust's choice win.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        if std::env::var("DEBUG").as_deref() == Ok("true") {
            println!("cargo:rustc-link-arg=/NODEFAULTLIB:LIBCMTD");
        } else {
            println!("cargo:rustc-link-arg=/NODEFAULTLIB:LIBCMT");
        }
    }
}
