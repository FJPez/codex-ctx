//! Byte-to-token estimation for items no anchor has priced.
//! Calibrated per the design spec's Estimator section.

use codex_protocol::models::ResponseItem;

use crate::item::ContentPart;
use crate::item::PartMedia;

/// Counts serialized bytes without keeping them.
#[derive(Default)]
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serialized size of a value, or `None` if it cannot be serialized.
pub fn serialized_size<T: serde::Serialize>(value: &T) -> Option<usize> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.0)
}

/// Bytes per 100 tokens: the median of the seven non-reasoning densities above, 4.64, held as an
/// integer so the estimate is exact arithmetic rather than a float.
const BYTES_PER_HUNDRED_TOKENS: i128 = 464;

/// Mirrors core's `RESIZED_IMAGE_BYTES_ESTIMATE` (`core/src/context_manager/history.rs:734`).
/// Flat for every image, so `detail: "original"` images are under-counted.
pub(crate) const IMAGE_TOKENS: i64 = 1_844;

/// 4.64 bytes/token, saturating rather than wrapping on absurd inputs.
pub(crate) fn text_tokens(bytes: usize) -> i64 {
    let tokens = (bytes as i128) * 100 / BYTES_PER_HUNDRED_TOKENS;
    i64::try_from(tokens).unwrap_or(i64::MAX)
}

/// Encrypted reasoning content costs about 1% of its byte size: the payload is an opaque blob the
/// provider stores rather than text the model reads. Single data point, 1,593 bytes -> 14 tokens.
pub(crate) fn reasoning_tokens(bytes: usize) -> i64 {
    i64::try_from(bytes / 100).unwrap_or(i64::MAX)
}

/// Summed per content entry so an image is not priced as prose.
pub(crate) fn item_tokens(item: &ResponseItem, parts: &[ContentPart], bytes: usize) -> i64 {
    if let ResponseItem::Reasoning {
        encrypted_content: Some(_),
        ..
    } = item
    {
        return reasoning_tokens(bytes);
    }
    if parts.is_empty() {
        return text_tokens(bytes);
    }
    parts
        .iter()
        .map(|part| match part.media {
            PartMedia::Image => IMAGE_TOKENS,
            // Core prices audio by decoded duration and falls back to a byte count when it cannot
            // decode; the profiler never decodes, so it always takes that fallback.
            PartMedia::Text | PartMedia::Audio => text_tokens(part.bytes),
        })
        .sum()
}

#[cfg(test)]
#[path = "estimate_tests.rs"]
mod tests;
