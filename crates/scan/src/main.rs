//! Loads one VST3 module and prints its classes as a JSON array, so that a plug-in that
//! crashes while loading only takes this process down.
fn main() {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: solstice-scan <module.vst3>");
        std::process::exit(2);
    };
    match solstice_vst3::scan(path.as_ref()) {
        Ok(classes) => println!("{}", serde_json::to_string(&classes).unwrap()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
