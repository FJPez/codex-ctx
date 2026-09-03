//! Byte-to-token estimation for items no measured total has priced yet.
//!
//! Calibrated against the Spike C captures, where the bytes are the serialised JSON sizes the
//! profiler itself measures and the tokens are provider-reported:
//!
//! | what | bytes | tokens | bytes/token |
//! |---|---|---|---|
//! | tool outputs | 4,792 / 15,876 / 14,152 / 24,567 | 1,040 / 3,373 / 2,219 / 5,043 | 4.61 / 4.71 / 6.38 / 4.87 |
//! | whole first request | 116,100 | 25,230 | 4.60 |
//! | hidden residual | 53,951 | ~11,700 | 4.61 |
//! | live-trace big read | 41,448 | 8,942 | 4.64 |
//! | one Reasoning item | 1,593 | 14 | 113.8 |
//!
//! Text sits in a narrow band, so one global constant covers it; reasoning is two orders of
//! magnitude off it and needs its own rule.

use crate::item::Category;
use crate::item::ContentPart;

/// Serialized size of a value, or `None` if it cannot be serialized.
pub fn serialized_size<T: serde::Serialize>(value: &T) -> Option<usize> {
    serde_json::to_vec(value).ok().map(|json| json.len())
}

/// Bytes per 100 tokens: the median of the seven non-reasoning densities above, 4.64, held as an
/// integer so the estimate is exact arithmetic rather than a float.
const BYTES_PER_HUNDRED_TOKENS: i128 = 464;

/// Flat cost of one image input, mirroring core's `RESIZED_IMAGE_BYTES_ESTIMATE` of 7,373 bytes at
/// its 4-bytes/token heuristic (`core/src/context_manager/history.rs:734`).
///
/// Core prices `detail: "original"` images from their decoded dimensions instead, counting 32px
/// patches up to a 10,000 cap; the profiler never decodes an image, so it uses this figure for all
/// of them. An unusually large `original` image is therefore under-counted, and lands in drift.
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

/// The initial cost of an item, before any anchor prices it.
///
/// Messages are summed per content entry so a mixed one - text alongside an image - is not priced
/// as if the image's base64 were prose.
pub(crate) fn item_tokens(category: Category, parts: &[ContentPart], bytes: usize) -> i64 {
    if category == Category::Reasoning {
        return reasoning_tokens(bytes);
    }
    if parts.is_empty() {
        return text_tokens(bytes);
    }
    parts
        .iter()
        .map(|part| {
            if part.is_image {
                IMAGE_TOKENS
            } else {
                text_tokens(part.bytes)
            }
        })
        .sum()
}

#[cfg(test)]
#[path = "estimate_tests.rs"]
mod tests;
