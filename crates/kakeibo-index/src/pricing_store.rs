//! Sole-writer persistence and overlay loading for user `pricing.toml` (`PLAN.md` §0 #5, §3.1, §6.2).

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;

use kakeibo_core::pricing::{PricingCatalog, PricingError};

use crate::paths::AppPaths;

/// Errors returned when loading or saving user pricing configuration in `kakeibo-index`.
#[derive(Debug)]
pub enum PricingStoreError {
    /// Neither `KAKEIBO_CONFIG_DIR`, `XDG_CONFIG_HOME`, nor `HOME` was set.
    MissingConfigDirectory,
    /// Filesystem read or atomic write error inside Kakeibo's config directory.
    Io(io::Error),
    /// Pricing TOML validation or serialization error.
    Pricing(PricingError),
}

impl fmt::Display for PricingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingConfigDirectory => {
                f.write_str("unable to resolve config directory (HOME / XDG_CONFIG_HOME unset)")
            }
            Self::Io(err) => write!(f, "pricing store I/O error: {err}"),
            Self::Pricing(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PricingStoreError {}

impl From<io::Error> for PricingStoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<PricingError> for PricingStoreError {
    fn from(value: PricingError) -> Self {
        Self::Pricing(value)
    }
}

/// Effective pricing catalog combining bundled placeholders with any user `pricing.toml` overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectivePricing {
    pub catalog: PricingCatalog,
    pub has_user_overlay: bool,
}

fn read_user_overlay(paths: &AppPaths) -> Result<Option<PricingCatalog>, PricingStoreError> {
    let user_path = paths.user_pricing_path();
    if !user_path.is_file() {
        return Ok(None);
    }
    let mut file = OpenOptions::new().read(true).open(&user_path)?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    let parsed = PricingCatalog::from_toml_str(&raw)?;
    Ok(Some(parsed))
}

/// Loads `PricingCatalog::bundled()` and overlays `<config_dir>/pricing.toml` when present.
pub fn load_effective_pricing(paths: &AppPaths) -> Result<EffectivePricing, PricingStoreError> {
    let mut catalog = PricingCatalog::bundled();
    if let Some(overlay) = read_user_overlay(paths)? {
        catalog.merge_overlay(&overlay);
        Ok(EffectivePricing {
            catalog,
            has_user_overlay: true,
        })
    } else {
        Ok(EffectivePricing {
            catalog,
            has_user_overlay: false,
        })
    }
}

/// Merges `updates` into the user's `<config_dir>/pricing.toml` and writes it atomically.
///
/// Writes strictly inside `paths.config_dir()` (`PLAN.md` §0 #5 & §3.1).
pub fn save_user_pricing(
    paths: &AppPaths,
    updates: &PricingCatalog,
) -> Result<PathBuf, PricingStoreError> {
    let mut user_overlay = read_user_overlay(paths)?.unwrap_or_else(PricingCatalog::empty_overlay);
    user_overlay.merge_overlay(updates);
    let serialized = user_overlay.to_toml_string()?;

    let dir = paths.config_dir();
    fs::create_dir_all(dir)?;

    let target_path = paths.user_pricing_path();
    let tmp_path = dir.join(format!(".pricing.toml.tmp.{}", process::id()));

    {
        let mut tmp_file = File::create(&tmp_path)?;
        tmp_file.write_all(serialized.as_bytes())?;
        tmp_file.sync_all()?;
    }

    if let Err(err) = fs::rename(&tmp_path, &target_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(PricingStoreError::Io(err));
    }

    Ok(target_path)
}
