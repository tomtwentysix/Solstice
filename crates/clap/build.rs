fn main() {
    println!("cargo:rerun-if-changed=guard.cpp");
    cc::Build::new().cpp(true).file("guard.cpp").compile("solstice_clap_guard");
}
