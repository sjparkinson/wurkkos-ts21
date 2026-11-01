use std::env;

fn main() {
    println!("cargo::rerun-if-changed=link.x");

    // The host build is only there for tests, so it links normally.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("avr") {
        return;
    }

    let dir = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-search={dir}");
    println!("cargo::rustc-link-arg-bins=-Tlink.x");
}
