fn main() {
    let path = owlgood_core::config_path();
    let result =
        owlgood_core::load_config(&path).and_then(|config| owlgood_macos::run(config, path));
    if let Err(error) = result {
        eprintln!("OwlGood: {error}");
        std::process::exit(1);
    }
}
