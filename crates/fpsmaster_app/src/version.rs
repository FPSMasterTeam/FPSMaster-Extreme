//! The single source of truth for the product name and version shown anywhere
//! in the UI (title screen, window title, F3 overlay). The version tracks the
//! `fpsmaster_app` crate version — a semantic `major.minor.patch` string — via
//! `CARGO_PKG_VERSION`, so a release only has to bump `Cargo.toml`.

/// Product / brand name.
pub const PRODUCT_NAME: &str = "FPSMaster Extreme";

/// Semantic product version (`major.minor.patch`), sourced from the crate
/// version in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The brand + version label shown to the player, e.g. `FPSMaster Extreme 1.0.0`.
pub fn title() -> String {
    format!("{PRODUCT_NAME} {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semantic_major_minor_patch() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "version must be major.minor.patch: {VERSION}");
        for p in parts {
            assert!(
                p.parse::<u32>().is_ok(),
                "version component {p:?} is not numeric in {VERSION}"
            );
        }
    }

    #[test]
    fn title_combines_name_and_version() {
        assert_eq!(title(), format!("{PRODUCT_NAME} {VERSION}"));
        assert!(title().starts_with("FPSMaster Extreme "));
    }
}
