//! Privacy-preserving irreversible SHA-256 digests and block sizing (`PLAN.md` §0 #4, §4).

use sha2::{Digest, Sha256};

fn hex_encode_prefix8(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(16);
    for &byte in &digest[..8] {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0F)]));
    }
    out
}

fn write_canonical_json(value: &serde_json::Value, out: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(_) | serde_json::Value::String(_) => {
            serde_json::to_writer(&mut *out, value).expect("in-memory JSON primitive write");
        }
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out);
            }
            out.push(b']');
        }
        serde_json::Value::Object(map) => {
            out.push(b'{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (idx, key) in keys.into_iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, key).expect("in-memory JSON key write");
                out.push(b':');
                write_canonical_json(&map[key], out);
            }
            out.push(b'}');
        }
    }
}

/// Computes a 16-hex-char SHA-256 prefix over `"cwd\0" + cwd` so raw working
/// directory paths are dropped immediately at parse time.
#[must_use]
pub fn hash_cwd(cwd: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cwd\0");
    hasher.update(cwd.as_bytes());
    let digest = hasher.finalize();
    hex_encode_prefix8(&digest)
}

/// Serialises `input` as canonical compact JSON with recursively sorted object
/// keys, returning `(utf8_byte_len, key_hash)` where `key_hash` is a 16-hex-char
/// SHA-256 prefix over `"tool\0" + tool_name + "\0" + canonical_json`.
///
/// The temporary JSON buffer is dropped before returning.
#[must_use]
pub fn measure_and_hash_tool_input(tool_name: &str, input: &serde_json::Value) -> (u64, String) {
    let mut canonical_buf = Vec::with_capacity(128);
    write_canonical_json(input, &mut canonical_buf);
    let byte_len = canonical_buf.len() as u64;

    let mut hasher = Sha256::new();
    hasher.update(b"tool\0");
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(&canonical_buf);
    let digest = hasher.finalize();

    // Explicitly clear before drop so temporary tool argument bytes do not linger.
    canonical_buf.clear();
    (byte_len, hex_encode_prefix8(&digest))
}

/// Computes an 8-byte SHA-256 content fingerprint used exclusively in memory to
/// deduplicate repeated streaming content blocks without retaining raw text.
#[must_use]
pub fn hash_block_content(tag: &str, content: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(tag.as_bytes());
    hasher.update(b"\0");
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Provisional byte-to-token block estimator prior to T06's Estimator ADR.
#[must_use]
pub const fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}
