use std::path::Path;

fn main() {
    let libs_dir = std::env::var("PRPR_AVC_LIBS").unwrap_or_else(|_| format!("{}/static-lib", std::env::var("CARGO_MANIFEST_DIR").unwrap()));
    let target = std::env::var("TARGET").unwrap();
    let libs_path = Path::new(&libs_dir).join(&target);
    let libs_path = libs_path.display();
    println!("cargo:rustc-link-search={libs_path}");
    if target.ends_with("apple-darwin") || target.ends_with("unknown-linux-gnu") {
        for dir in ["/opt/homebrew/lib", "/usr/local/lib", "/usr/lib/x86_64-linux-gnu"] {
            if Path::new(dir).exists() {
                println!("cargo:rustc-link-search=native={dir}");
            }
        }
        if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX") {
            println!("cargo:rustc-link-search=native={prefix}/lib");
        }
        println!("cargo:rustc-link-lib=static=vorbis");
        println!("cargo:rustc-link-lib=static=ogg");
    }
    if !target.ends_with("windows-msvc") {
        println!("cargo:rustc-link-lib=z");
    }
    println!("cargo:rerun-if-changed={libs_path}");
}
