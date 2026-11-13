fn main() {
    pkg_config::Config::new().atleast_version("0.24").probe("lilv-0").expect("lilv-0 (lilv development files)");
    println!("cargo:rerun-if-changed=src/log.c");
    cc::Build::new().file("src/log.c").compile("solstice_lv2_log");
}
