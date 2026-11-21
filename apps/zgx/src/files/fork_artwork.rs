//! Fork: a project's artwork from the engine's own search
//! (crates/engine/src/project_icon.rs, `ReadProjectIcon`), the one the
//! iPhone uses too. It knows about fifty places projects keep an icon,
//! SVG and ICO among them, and the device holding the checkout renders what
//! it finds as a small PNG, so a remote project costs one call.
//!
//! `Files::artwork` (files.rs) asks here first and falls back to zgx's own
//! `ARTWORK_PATHS` lookup when this finds nothing, so upstream's behaviour is
//! the floor. Kept in its own file to hold the upstream diff to two lines.

use base64::Engine as _;
use sha2::{Digest, Sha256};
use zgx_script::Value;

use super::Files;

impl Files {
    /// `{ src }`, a PNG in the picture cache, when the engine found artwork;
    /// None for zgx's own lookup: nothing found, the device unreachable, or
    /// an engine from before `ReadProjectIcon`.
    pub(super) async fn engine_artwork(&self, target: &Value) -> Option<Value> {
        let params = self.params(Some(target)).await.ok()?;
        let icon = self.call("ReadProjectIcon", params).await.ok()?;
        let png = base64::engine::general_purpose::STANDARD
            .decode(icon["data"].as_str().filter(|data| !data.is_empty())?)
            .ok()?;
        let file = super::keep_image(&png_name(&png), "image/png", &png).ok()?;
        Some(Value::record([("src", Value::from(file.to_string_lossy().as_ref()))]))
    }
}

/// The cache name for `png`: its SHA-256, so the same picture is kept once
/// and a changed one never shows the old file.
fn png_name(png: &[u8]) -> String {
    Sha256::digest(png).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_picture_is_kept_by_its_hash() {
        let name = super::png_name(b"\x89PNG");
        assert_eq!(name.len(), 64);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(name, super::png_name(b"\x89PNH"));
    }
}
