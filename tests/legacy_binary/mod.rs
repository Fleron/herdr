use std::path::Path;
use std::sync::OnceLock;

pub fn path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let binary = Path::new(env!("CARGO_BIN_EXE_herdr"))
            .parent()
            .expect("Cargo binary directory")
            .join("examples")
            .join(format!("legacy-regression{}", std::env::consts::EXE_SUFFIX));
        assert!(binary.is_file(), "build the legacy regression target first: cargo build --locked --example legacy-regression");
        binary.to_str().expect("test binary path is UTF-8").to_owned()
    })
}
