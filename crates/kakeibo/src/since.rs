//! Relative time-window (`--since 7d`, `24h`, `30m`) and RFC 3339 cutoff parsing.

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use kakeibo_core::Timestamp;

/// Resolves the reference UTC wall-clock instant (`KAKEIBO_TEST_NOW` override or `SystemTime::now()`).
pub fn resolve_now() -> Result<Timestamp, String> {
    if let Ok(override_raw) = env::var("KAKEIBO_TEST_NOW") {
        let trimmed = override_raw.trim();
        if !trimmed.is_empty() {
            return Timestamp::parse_rfc3339(trimmed)
                .map_err(|e| format!("invalid KAKEIBO_TEST_NOW: {e}"));
        }
    }

    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock precedes UNIX_EPOCH: {e}"))?;
    let secs = i64::try_from(elapsed.as_secs())
        .map_err(|_| "system clock seconds overflowed i64".to_string())?;
    Timestamp::from_unix_timestamp(secs).map_err(|e| e.to_string())
}

/// Parses `--since <WINDOW>` as either an explicit RFC 3339 timestamp or a relative
/// duration (`<N>s`, `<N>m`, `<N>h`, `<N>d`, `<N>w`) subtracted from `now`.
pub fn parse_since_cutoff(spec: &str, now: Timestamp) -> Result<Timestamp, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("`--since` value cannot be empty".to_string());
    }

    if let Ok(explicit_ts) = Timestamp::parse_rfc3339(trimmed) {
        return Ok(explicit_ts);
    }

    let Some(unit) = trimmed.chars().last() else {
        return Err(format!("invalid `--since` window `{trimmed}`"));
    };
    let number_part = &trimmed[..trimmed.len() - unit.len_utf8()];
    let count: u64 = number_part.parse().map_err(|_| {
        format!(
            "invalid `--since` window `{trimmed}` (expected `<N>s|m|h|d|w` or RFC 3339 timestamp)"
        )
    })?;

    let multiplier: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 7 * 86_400,
        _ => {
            return Err(format!(
                "unsupported `--since` unit `{unit}` in `{trimmed}` (expected s, m, h, d, or w)"
            ));
        }
    };

    let delta_secs = count
        .checked_mul(multiplier)
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| format!("`--since` duration `{trimmed}` overflowed"))?;
    let cutoff_secs = now.unix_timestamp().saturating_sub(delta_secs);
    Timestamp::from_unix_timestamp(cutoff_secs).map_err(|e| e.to_string())
}
