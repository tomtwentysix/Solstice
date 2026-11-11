fn main() {
    let shim = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vst3shim");
    for f in ["shim.cpp", "x11window.cpp", "solstice_vst3.h", "CMakeLists.txt"] {
        println!("cargo:rerun-if-changed={}", shim.join(f).display());
    }
    let dst = cmake::Config::new(&shim)
        .profile("Release")
        .define("SOLSTICE_SDK_EXAMPLES", "OFF")
        .build_target("solstice_vst3")
        .build();
    let build = dst.join("build");
    println!("cargo:rustc-link-search=native={}", build.display());
    println!("cargo:rustc-link-search=native={}", build.join("lib/Release").display());
    for lib in ["solstice_vst3", "sdk_hosting", "sdk_common", "base", "pluginterfaces"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    for lib in ["X11", "stdc++", "dl", "pthread"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
