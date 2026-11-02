//! Build script: exports the pinned UI-framework version from the
//! workspace manifest so `doctor` can print it without a runtime lookup.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../../Cargo.toml");
    let text = fs::read_to_string(manifest)?;
    let pin = find_gpui_pin(&text).ok_or("the workspace manifest carries no gpui pin")?;
    println!("cargo:rustc-env=YAGNA_GPUI_PIN={pin}");
    Ok(())
}

/// Finds the `gpui = "…"` entry in the manifest and returns the version
/// without its leading `=`.
fn find_gpui_pin(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("gpui") {
            let value = trimmed.split('"').nth(1)?;
            return Some(value.trim_start_matches('=').to_string());
        }
    }
    None
}
