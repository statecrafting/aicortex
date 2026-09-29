#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use aicortex_embed::{ChunkConfig, Chunker};

#[test]
fn short_body_is_one_exact_chunk() {
    let chunker = Chunker::new(ChunkConfig {
        threshold_bytes: 100,
        target_bytes: 80,
        overlap_bytes: 20,
        max_chunks_per_memory: 512,
    })
    .expect("valid chunk configuration");
    let chunks = chunker
        .split("First sentence. Second sentence.")
        .expect("body chunks");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].ordinal, 0);
    assert_eq!(chunks[0].byte_start, 0);
    assert_eq!(chunks[0].byte_end, 32);
    assert_eq!(chunks[0].text, "First sentence. Second sentence.");
}

#[test]
fn long_body_is_covered_with_overlap_and_exact_ranges() {
    let sentence = "A bounded sentence carries enough text for chunking. ";
    let body = sentence.repeat(900);
    assert!(body.len() > 40 * 1024);
    let chunker = Chunker::new(ChunkConfig {
        threshold_bytes: 1024,
        target_bytes: 2048,
        overlap_bytes: 256,
        max_chunks_per_memory: 512,
    })
    .expect("valid chunk configuration");
    let chunks = chunker.split(&body).expect("body chunks");

    assert!(chunks.len() > 2);
    assert_eq!(chunks.first().map(|chunk| chunk.byte_start), Some(0));
    assert_eq!(chunks.last().map(|chunk| chunk.byte_end), Some(body.len()));
    for (ordinal, chunk) in chunks.iter().enumerate() {
        assert_eq!(usize::try_from(chunk.ordinal).ok(), Some(ordinal));
        assert_eq!(
            body.get(chunk.byte_start..chunk.byte_end),
            Some(chunk.text.as_str())
        );
    }
    for pair in chunks.windows(2) {
        let first = &pair[0];
        let second = &pair[1];
        assert!(second.byte_start < first.byte_end);
        assert!(second.byte_start > first.byte_start);
        assert!(first.byte_end - second.byte_start >= 256);
        assert_eq!(
            body.get(second.byte_start..first.byte_end),
            second.text.get(..first.byte_end - second.byte_start)
        );
    }

    let mut rebuilt = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let start = if index == 0 {
            0
        } else {
            chunks[index - 1].byte_end - chunk.byte_start
        };
        rebuilt.push_str(chunk.text.get(start..).expect("overlap is a byte range"));
    }
    assert_eq!(rebuilt, body);
}

#[test]
fn overlap_is_preserved_after_a_short_leading_sentence() {
    let body = format!("Short. {}", "x".repeat(400));
    let chunks = Chunker::new(ChunkConfig {
        threshold_bytes: 16,
        target_bytes: 100,
        overlap_bytes: 30,
        max_chunks_per_memory: 32,
    })
    .expect("valid chunk configuration")
    .split(&body)
    .expect("bounded body chunks");
    for pair in chunks.windows(2) {
        assert!(pair[0].byte_end - pair[1].byte_start >= 30);
    }
}

#[test]
fn amplification_is_bounded_by_configuration() {
    let chunker = Chunker::new(ChunkConfig {
        threshold_bytes: 1,
        target_bytes: 10,
        overlap_bytes: 5,
        max_chunks_per_memory: 2,
    })
    .expect("valid chunk configuration");
    assert!(chunker.split(&"x".repeat(100)).is_err());
}

#[test]
fn multibyte_sentence_never_splits_inside_a_scalar() {
    let body = "Crème brûlée is good. 旅行も好きです。 Another sentence. ".repeat(40);
    let chunks = Chunker::new(ChunkConfig {
        threshold_bytes: 32,
        target_bytes: 97,
        overlap_bytes: 19,
        max_chunks_per_memory: 512,
    })
    .expect("valid chunk configuration")
    .split(&body)
    .expect("body chunks");
    assert!(chunks.iter().all(|chunk| {
        body.is_char_boundary(chunk.byte_start)
            && body.is_char_boundary(chunk.byte_end)
            && body.get(chunk.byte_start..chunk.byte_end) == Some(chunk.text.as_str())
    }));
}
