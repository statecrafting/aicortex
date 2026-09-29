//! Sentence-boundary chunking with byte-precise overlap.

use rahi_types::Error;

/// Chunking thresholds, measured in UTF-8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkConfig {
    /// Bodies at or below this size remain one chunk.
    pub threshold_bytes: usize,
    /// Preferred maximum chunk size.
    pub target_bytes: usize,
    /// Minimum byte overlap requested between adjacent chunks.
    pub overlap_bytes: usize,
    /// Hard ceiling on chunks emitted for one memory.
    pub max_chunks_per_memory: usize,
}

impl ChunkConfig {
    /// Validate a configuration.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when a bound is zero or overlap exceeds half the
    /// target.
    pub fn validate(self) -> Result<Self, Error> {
        if self.threshold_bytes == 0 || self.target_bytes == 0 || self.max_chunks_per_memory == 0 {
            return Err(Error::Config(
                "chunk threshold, target, and chunk ceiling must be non-zero".to_owned(),
            ));
        }
        if self.overlap_bytes > self.target_bytes / 2 {
            return Err(Error::Config(format!(
                "chunk overlap {} must not exceed half the target {}",
                self.overlap_bytes, self.target_bytes
            )));
        }
        Ok(self)
    }
}

/// One slice of the original body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Stable zero-based order within the memory.
    pub ordinal: u32,
    /// Inclusive UTF-8 byte offset.
    pub byte_start: usize,
    /// Exclusive UTF-8 byte offset.
    pub byte_end: usize,
    /// Exact text in `byte_start..byte_end`.
    pub text: String,
}

/// A validated chunking policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunker {
    config: ChunkConfig,
}

impl Chunker {
    /// Build a chunker.
    ///
    /// # Errors
    ///
    /// The validation errors from [`ChunkConfig::validate`].
    pub fn new(config: ChunkConfig) -> Result<Self, Error> {
        Ok(Self {
            config: config.validate()?,
        })
    }

    /// Split `body` without changing a byte.
    ///
    /// Sentence boundaries are preferred. A sentence longer than the target
    /// is split at the nearest UTF-8 boundary so progress stays bounded.
    pub fn split(&self, body: &str) -> Result<Vec<Chunk>, Error> {
        if body.is_empty() {
            return Ok(Vec::new());
        }
        if body.len() <= self.config.threshold_bytes {
            return Ok(vec![chunk(body, 0, 0, body.len())]);
        }

        let boundaries = sentence_boundaries(body);
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < body.len() {
            let target = start
                .saturating_add(self.config.target_bytes)
                .min(body.len());
            let mut end = boundaries
                .iter()
                .copied()
                .rfind(|boundary| *boundary > start && *boundary <= target)
                .unwrap_or_else(|| utf8_floor(body, target));
            if end < body.len() && end.saturating_sub(start) <= self.config.overlap_bytes {
                end = utf8_floor(body, target);
            }
            if end <= start {
                end = body[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(body.len(), |(offset, _)| start.saturating_add(offset));
            }
            let ordinal = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
            chunks.push(chunk(body, ordinal, start, end));
            if chunks.len() > self.config.max_chunks_per_memory {
                return Err(Error::Validation(format!(
                    "memory requires more than {} chunks",
                    self.config.max_chunks_per_memory
                )));
            }
            if end == body.len() {
                break;
            }

            let desired = end.saturating_sub(self.config.overlap_bytes);
            let sentence_start = boundaries
                .iter()
                .copied()
                .rfind(|boundary| *boundary > start && *boundary <= desired);
            let next = sentence_start.unwrap_or_else(|| utf8_floor(body, desired));
            if next > start {
                start = next;
            } else {
                start = body[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(end, |(offset, _)| start.saturating_add(offset));
            }
        }
        Ok(chunks)
    }
}

fn chunk(body: &str, ordinal: u32, start: usize, end: usize) -> Chunk {
    Chunk {
        ordinal,
        byte_start: start,
        byte_end: end,
        text: body.get(start..end).unwrap_or_default().to_owned(),
    }
}

fn utf8_floor(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset = offset.saturating_sub(1);
    }
    offset
}

fn sentence_boundaries(text: &str) -> Vec<usize> {
    let mut boundaries = vec![0];
    let mut sentence_end = false;
    for (offset, character) in text.char_indices() {
        let after = offset.saturating_add(character.len_utf8());
        if character == '\n' {
            boundaries.push(after);
            sentence_end = false;
        } else if matches!(character, '.' | '!' | '?') {
            sentence_end = true;
        } else if sentence_end && character.is_whitespace() {
            boundaries.push(after);
            sentence_end = false;
        } else if !character.is_whitespace() {
            sentence_end = false;
        }
    }
    if boundaries.last().copied() != Some(text.len()) {
        boundaries.push(text.len());
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}
