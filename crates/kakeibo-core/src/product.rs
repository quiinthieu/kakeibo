//! Single source of truth for Kakeibo product names and identifiers.
//!
//! Mirrored in `macos/Config/Product.xcconfig` (added in T15).
//! Do not hardcode these product strings anywhere else in the workspace.

/// Human-facing product display name.
pub const DISPLAY_NAME: &str = "Kakeibo";

/// CLI binary executable name.
pub const BINARY_NAME: &str = "kakeibo";

/// Primary CLI Cargo crate name.
pub const CRATE_NAME: &str = "kakeibo";

/// Homebrew formula name.
pub const HOMEBREW_FORMULA: &str = "kakeibo";

/// Optional macOS menu bar Homebrew cask name.
pub const MENUBAR_CASK: &str = "kakeibo-bar";

/// macOS bundle identifier for the menu bar application.
pub const BUNDLE_ID: &str = "io.github.quiinthieu.kakeibo";

/// Package version injected at compile time from workspace Cargo metadata.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Standard `<binary> <version>` string matching `--version` CLI output.
#[must_use]
pub fn version_string() -> String {
    format!("{BINARY_NAME} {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_constants_match_specification() {
        assert_eq!(DISPLAY_NAME, "Kakeibo");
        assert_eq!(BINARY_NAME, "kakeibo");
        assert_eq!(CRATE_NAME, "kakeibo");
        assert_eq!(HOMEBREW_FORMULA, "kakeibo");
        assert_eq!(MENUBAR_CASK, "kakeibo-bar");
        assert_eq!(BUNDLE_ID, "io.github.quiinthieu.kakeibo");
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert!(VERSION.split('.').count() >= 3);
        assert_eq!(version_string(), format!("{BINARY_NAME} {VERSION}"));
    }
}
