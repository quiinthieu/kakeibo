//! Isolated application configuration and cache directory resolution (`PLAN.md` §0 #5, §3.1).

use std::env;
use std::path::{Path, PathBuf};

use kakeibo_core::product;

use crate::pricing_store::PricingStoreError;

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
}

/// Resolves Kakeibo's own isolated configuration and cache directories.
///
/// Honours `KAKEIBO_CONFIG_DIR` / `KAKEIBO_CACHE_DIR`, then `XDG_CONFIG_HOME` / `XDG_CACHE_HOME`
/// (ensuring `TestEnv` isolation on both macOS and Linux), falling back to `$HOME/.config/<bin>`
/// and `$HOME/.cache/<bin>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    config_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    /// Constructs explicit [`AppPaths`] for testing or custom directory roots.
    #[must_use]
    pub const fn new(config_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            config_dir,
            cache_dir,
        }
    }

    /// Resolves [`AppPaths`] from standard process environment variables.
    pub fn from_env() -> Result<Self, PricingStoreError> {
        let home = non_empty_env_path("HOME").or_else(|| non_empty_env_path("USERPROFILE"));

        let config_dir = if let Some(explicit) = non_empty_env_path("KAKEIBO_CONFIG_DIR") {
            explicit
        } else if let Some(xdg) = non_empty_env_path("XDG_CONFIG_HOME") {
            xdg.join(product::BINARY_NAME)
        } else if let Some(appdata) = non_empty_env_path("APPDATA") {
            appdata.join(product::BINARY_NAME)
        } else if let Some(ref h) = home {
            h.join(".config").join(product::BINARY_NAME)
        } else {
            return Err(PricingStoreError::MissingConfigDirectory);
        };

        let cache_dir = if let Some(explicit) = non_empty_env_path("KAKEIBO_CACHE_DIR") {
            explicit
        } else if let Some(xdg) = non_empty_env_path("XDG_CACHE_HOME") {
            xdg.join(product::BINARY_NAME)
        } else if let Some(local_appdata) = non_empty_env_path("LOCALAPPDATA") {
            local_appdata.join(product::BINARY_NAME)
        } else if let Some(ref h) = home {
            h.join(".cache").join(product::BINARY_NAME)
        } else {
            config_dir.join("cache")
        };

        Ok(Self::new(config_dir, cache_dir))
    }

    /// Kakeibo's own configuration directory (`<config_dir>`).
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Kakeibo's own cache directory (`<cache_dir>`).
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Path to the user-editable pricing override file (`<config_dir>/pricing.toml`).
    #[must_use]
    pub fn user_pricing_path(&self) -> PathBuf {
        self.config_dir.join("pricing.toml")
    }
}
