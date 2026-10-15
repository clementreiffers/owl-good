fn main() {
    let path = std::env::var_os("ISDOWN_CONFIG_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(isdown_core::config_path);
    let result = isdown_core::load_config(&path).and_then(|config| isdown_macos::run(config, path));
    if let Err(error) = result {
        eprintln!("isDown: {error}");
        std::process::exit(1);
    }
}
