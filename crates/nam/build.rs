fn main() {
    let shim = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../namshim");
    for f in [
        "bridge.cpp",
        "solstice_nam.h",
        "CMakeLists.txt",
        "patches/convnet-no-alloc.patch",
    ] {
        println!("cargo:rerun-if-changed={}", shim.join(f).display());
    }
    let dst = cmake::Config::new(&shim)
        .profile("Release")
        .build_target("solstice_nam")
        .build();
    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("build").display()
    );
    // Architectures register themselves from static initialisers that nothing else references.
    println!("cargo:rustc-link-lib=static:+whole-archive=solstice_nam");
    println!("cargo:rustc-link-lib=dylib=stdc++");
}
