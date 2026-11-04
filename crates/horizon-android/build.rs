fn main() {
    // Android tombstones can match a native frame to the exact private symbols.
    // This affects the final ELF only, not code generation or release optimization.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg=-Wl,--build-id=sha1");
    }
    // These values are read with option_env! by the native Android runtime. Make Cargo
    // rebuild the cdylib when a different ABI or Horizon revision is selected.
    for name in [
        "HORIZON_ANDROID_BUILD_VERSION",
        "HORIZON_ANDROID_BUILD_COMMIT",
        "HORIZON_ANDROID_BUILD_DIRTY",
        "HORIZON_ANDROID_ABI",
        "HORIZON_ANDROID_PUBLIC",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
}
