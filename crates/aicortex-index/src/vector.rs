//! The stored vector and the metric (spec 016 B-1, B-3).
//!
//! A vector is the `f32` little-endian BLOB spec 015 writes, with its width
//! recorded beside it in the `dims` column (015 B-10). Decoding checks the
//! byte length against that recorded width and refuses a mismatch with a
//! typed error; it never infers a width from a byte count.
//!
//! Both sides of a comparison are L2-normalized, so cosine similarity is the
//! dot product. [`dot`] is written so the compiler can vectorize it: eight
//! independent accumulators over fixed-width chunks, reduced in a fixed
//! order. [`dot_scalar`] is the plain loop, used for the tail and as the
//! reference the B-3 test compares the lane kernel against.

use std::fmt;

use rahi_types::Error;

/// The accumulator width of [`dot`]: eight `f32` lanes, one 256-bit register.
pub const LANES: usize = 8;

/// The ways a stored or query vector can be unusable (B-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorError {
    /// The BLOB's length is not four bytes per recorded dimension.
    Length {
        /// The width the row records.
        dims: u16,
        /// The bytes the row holds.
        bytes: usize,
    },
    /// A vector's width differs from the width it is compared at.
    Width {
        /// The width the comparison is made at.
        expected: u16,
        /// The width the vector has.
        actual: usize,
    },
    /// A value is NaN or infinite.
    NonFinite,
    /// The vector has no direction, so it cannot be normalized.
    ZeroNorm,
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { dims, bytes } => write!(
                f,
                "vector records {dims} dimensions but holds {bytes} bytes, not {}",
                usize::from(*dims).saturating_mul(4)
            ),
            Self::Width { expected, actual } => write!(
                f,
                "vector has {actual} dimensions where {expected} are compared"
            ),
            Self::NonFinite => f.write_str("vector holds a value that is not finite"),
            Self::ZeroNorm => f.write_str("vector has zero length and no direction"),
        }
    }
}

impl std::error::Error for VectorError {}

impl From<VectorError> for Error {
    fn from(error: VectorError) -> Self {
        match error {
            VectorError::Width { .. } | VectorError::ZeroNorm => {
                Self::Validation(error.to_string())
            }
            VectorError::Length { .. } | VectorError::NonFinite => {
                Self::Integrity(error.to_string())
            }
        }
    }
}

/// Decode `bytes` as exactly `dims` little-endian `f32` values into `out`.
///
/// `out` is cleared first and reused, so a scan decodes every row into one
/// buffer rather than allocating per row.
///
/// # Errors
///
/// [`VectorError::Length`] when the length is not `4 * dims`, and
/// [`VectorError::NonFinite`] for a NaN or infinite value.
pub fn decode_into(bytes: &[u8], dims: u16, out: &mut Vec<f32>) -> Result<(), VectorError> {
    if bytes.len() != usize::from(dims).saturating_mul(4) {
        return Err(VectorError::Length {
            dims,
            bytes: bytes.len(),
        });
    }
    out.clear();
    out.reserve(usize::from(dims));
    // The length check above leaves no remainder.
    let (words, _) = bytes.as_chunks::<4>();
    for word in words {
        let value = f32::from_le_bytes(*word);
        if !value.is_finite() {
            return Err(VectorError::NonFinite);
        }
        out.push(value);
    }
    Ok(())
}

/// Decode `bytes` as exactly `dims` little-endian `f32` values.
///
/// # Errors
///
/// As [`decode_into`].
pub fn decode(bytes: &[u8], dims: u16) -> Result<Vec<f32>, VectorError> {
    let mut out = Vec::new();
    decode_into(bytes, dims, &mut out)?;
    Ok(out)
}

/// Scale `values` to unit L2 length in place.
///
/// # Errors
///
/// [`VectorError::NonFinite`] for a NaN or infinite value, and
/// [`VectorError::ZeroNorm`] when every value is zero.
pub fn normalize(values: &mut [f32]) -> Result<(), VectorError> {
    if values.iter().any(|value| !value.is_finite()) {
        return Err(VectorError::NonFinite);
    }
    // Squares are summed in `f64`, where no finite `f32` input can overflow,
    // so the only way to fail here is a vector with no length at all.
    let norm = values
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if norm == 0.0 {
        return Err(VectorError::ZeroNorm);
    }
    for value in values.iter_mut() {
        *value = (f64::from(*value) / norm) as f32;
    }
    Ok(())
}

/// The dot product, in a shape the compiler vectorizes (B-3).
///
/// Eight lanes accumulate independently over each full chunk of
/// [`LANES`] values; the lanes are then summed in index order and the tail
/// is added through [`dot_scalar`]. The order is fixed, so the same inputs
/// always give the same bits (B-4). Both slices must have the same length;
/// extra values in the longer one are ignored.
#[must_use]
pub fn dot(left: &[f32], right: &[f32]) -> f32 {
    let mut lanes = [0.0_f32; LANES];
    let left_chunks = left.chunks_exact(LANES);
    let right_chunks = right.chunks_exact(LANES);
    let tail = dot_scalar(left_chunks.remainder(), right_chunks.remainder());
    for (a, b) in left_chunks.zip(right_chunks) {
        for ((lane, x), y) in lanes.iter_mut().zip(a).zip(b) {
            *lane += x * y;
        }
    }
    lanes.iter().sum::<f32>() + tail
}

/// The dot product as one sequential loop: the scalar fallback and the
/// reference [`dot`] is tested against (B-3).
#[must_use]
pub fn dot_scalar(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    /// A fixed pseudo-random sequence in `[-1, 1)`.
    fn fixture(seed: u64, len: usize) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let unit = u32::try_from(state >> 40).unwrap_or(0);
                (f64::from(unit) / f64::from(1_u32 << 24)).mul_add(2.0, -1.0) as f32
            })
            .collect()
    }

    #[test]
    fn the_lane_kernel_agrees_with_the_scalar_loop() {
        // B-3: within 1e-6 over a fixture of normalized vectors, at widths
        // that exercise both full chunks and every tail length.
        for dims in [1, 7, 8, 9, 64, 383, 384, 768, 1024] {
            for seed in 0..16 {
                let mut a = fixture(seed, dims);
                let mut b = fixture(seed + 1000, dims);
                normalize(&mut a).unwrap();
                normalize(&mut b).unwrap();
                let lane = dot(&a, &b);
                let scalar = dot_scalar(&a, &b);
                assert!(
                    (lane - scalar).abs() <= 1e-6,
                    "dims {dims} seed {seed}: lane {lane} scalar {scalar}"
                );
            }
        }
    }

    #[test]
    fn decoding_refuses_a_length_that_disagrees_with_dims() {
        let bytes = [0_u8; 12];
        assert_eq!(
            decode(&bytes, 4),
            Err(VectorError::Length { dims: 4, bytes: 12 })
        );
        assert_eq!(decode(&bytes, 3).unwrap(), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn decoding_reads_little_endian_and_refuses_non_finite_values() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.5_f32.to_le_bytes());
        bytes.extend_from_slice(&(-2.0_f32).to_le_bytes());
        assert_eq!(decode(&bytes, 2).unwrap(), vec![1.5, -2.0]);
        bytes.extend_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(decode(&bytes, 3), Err(VectorError::NonFinite));
    }

    #[test]
    fn normalizing_refuses_a_zero_vector_and_survives_huge_values() {
        assert_eq!(normalize(&mut [0.0, 0.0]), Err(VectorError::ZeroNorm));
        let mut huge = [f32::MAX, f32::MAX];
        normalize(&mut huge).unwrap();
        assert!((huge[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        let mut v = [3.0, 4.0];
        normalize(&mut v).unwrap();
        assert!((v[0] - 0.6).abs() < 1e-7 && (v[1] - 0.8).abs() < 1e-7);
    }
}
