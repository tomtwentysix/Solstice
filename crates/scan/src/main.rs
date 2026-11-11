//! Loads one VST3 or CLAP module and prints its classes as a JSON array, so that a plug-in
//! that crashes while loading only takes this process down.
fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: solstice-scan <module.vst3|module.clap>");
        std::process::exit(2);
    };
    let path: &std::path::Path = path.as_ref();
    let result = if path.extension().is_some_and(|e| e == "clap") { solstice_clap::scan(path) } else { solstice_vst3::scan(path) };
    match result {
        Ok(classes) => println!("{}", serde_json::to_string(&classes).unwrap()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
