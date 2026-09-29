//! The provider-neutral vector contract.

use core::fmt;

use rahi_types::Error;

/// A configured embedding model, separate from its numeric revision.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelId(String);

impl ModelId {
    /// Parse a non-empty model identifier.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `id` is empty or padded.
    pub fn new(id: impl Into<String>) -> Result<Self, Error> {
        let id = id.into();
        if id.is_empty() || id.trim() != id {
            return Err(Error::Validation(
                "an embedding model id must be non-empty and unpadded".to_owned(),
            ));
        }
        Ok(Self(id))
    }

    /// The stable configured identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One embedding vector in provider order.
#[derive(Clone, Debug, PartialEq)]
pub struct Vector(Vec<f32>);

impl Vector {
    /// Build a non-empty vector containing only finite values.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the vector is empty, too wide for the
    /// schema, or contains a non-finite value.
    pub fn new(values: Vec<f32>) -> Result<Self, Error> {
        if values.is_empty() {
            return Err(Error::Validation(
                "an embedding vector must have at least one dimension".to_owned(),
            ));
        }
        if u16::try_from(values.len()).is_err() {
            return Err(Error::Validation(format!(
                "embedding vector has {} dimensions, above the u16 ceiling",
                values.len()
            )));
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(Error::Validation(
                "an embedding vector must contain only finite values".to_owned(),
            ));
        }
        Ok(Self(values))
    }

    /// Number of scalar values.
    #[must_use]
    pub fn dims(&self) -> u16 {
        u16::try_from(self.0.len()).unwrap_or(u16::MAX)
    }

    /// The scalar values.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.0
    }

    /// Encode each scalar as IEEE-754 little-endian bytes (B-10).
    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.0.len().saturating_mul(size_of::<f32>()));
        for value in &self.0 {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// Decode exactly `dims` little-endian `f32` values.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the byte length disagrees with `dims`, or
    /// when a stored scalar is not finite.
    pub fn from_le_bytes(bytes: &[u8], dims: u16) -> Result<Self, Error> {
        let expected = usize::from(dims).saturating_mul(size_of::<f32>());
        if bytes.len() != expected {
            return Err(Error::Integrity(format!(
                "vector declares {dims} dimensions but has {} bytes, expected {expected}",
                bytes.len()
            )));
        }
        let values = bytes
            .chunks_exact(size_of::<f32>())
            .map(|chunk| {
                let encoded: [u8; size_of::<f32>()] = chunk
                    .try_into()
                    .map_err(|_| Error::Integrity("vector scalar is not four bytes".to_owned()))?;
                Ok(f32::from_le_bytes(encoded))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Self::new(values).map_err(|error| Error::Integrity(error.message().to_owned()))
    }
}

/// A provider resolved once at boot.
#[allow(async_fn_in_trait)]
pub trait EmbeddingProvider: Send + Sync {
    /// The model identity configured for this provider.
    fn id(&self) -> ModelId;

    /// The exact number of values every returned vector carries.
    fn dims(&self) -> u16;

    /// Whether returned vectors are L2-normalized.
    fn normalized(&self) -> bool;

    /// Embed one batch in the same order as the input.
    ///
    /// # Errors
    ///
    /// Provider or validation error. A successful result must contain one
    /// vector per input and each vector must have [`Self::dims`] values.
    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error>;
}

/// Validate the common result invariants at the provider boundary.
pub(crate) fn validate_batch(
    provider: &impl EmbeddingProvider,
    input_len: usize,
    vectors: &[Vector],
) -> Result<(), Error> {
    if vectors.len() != input_len {
        return Err(Error::Integrity(format!(
            "provider {} returned {} vectors for {input_len} inputs",
            provider.id(),
            vectors.len()
        )));
    }
    if let Some(vector) = vectors
        .iter()
        .find(|vector| vector.dims() != provider.dims())
    {
        return Err(Error::Integrity(format!(
            "provider {} declares {} dimensions but returned {}",
            provider.id(),
            provider.dims(),
            vector.dims()
        )));
    }
    for vector in vectors {
        if vector
            .values()
            .iter()
            .all(|value| value.to_bits() & 0x7fff_ffff == 0)
        {
            return Err(Error::Integrity(format!(
                "provider {} returned a zero vector",
                provider.id()
            )));
        }
        if provider.normalized() && !has_normalized_norm(vector) {
            return Err(Error::Integrity(format!(
                "provider {} declares normalized vectors but returned a vector outside tolerance",
                provider.id()
            )));
        }
    }
    Ok(())
}

/// Check a squared norm against `(0.999)..=(1.001)` without violating the
/// workspace prohibition on floating-point arithmetic outside index and recall.
/// Each finite `f32` is decoded into a Q48 integer, then the exact integer
/// squares are compared with the rational squared bounds.
fn has_normalized_norm(vector: &Vector) -> bool {
    const FRACTION_MASK: u32 = 0x007f_ffff;
    const EXPONENT_SHIFT: u32 = 23;
    const EXPONENT_MASK: u32 = 0xff;
    const IMPLICIT_BIT: u128 = 1 << 23;
    const Q_BITS: u32 = 48;
    const F32_MANTISSA_SCALE_EXPONENT: i32 = 150;
    const DENOMINATOR: u128 = 1_000_000;
    const LOWER_NUMERATOR: u128 = 998_001;
    const UPPER_NUMERATOR: u128 = 1_002_001;
    const Q_SQUARED: u128 = 1_u128 << (Q_BITS * 2);

    let mut squared_norm = 0_u128;
    for value in vector.values() {
        let bits = value.to_bits() & 0x7fff_ffff;
        let exponent = (bits >> EXPONENT_SHIFT) & EXPONENT_MASK;
        if exponent >= 128 {
            return false;
        }
        let mantissa = if exponent == 0 {
            u128::from(bits & FRACTION_MASK)
        } else {
            IMPLICIT_BIT | u128::from(bits & FRACTION_MASK)
        };
        let shift = i32::try_from(exponent).unwrap_or_default() - F32_MANTISSA_SCALE_EXPONENT
            + i32::try_from(Q_BITS).unwrap_or_default();
        let fixed = if shift >= 0 {
            mantissa
                .checked_shl(shift.unsigned_abs())
                .unwrap_or(u128::MAX)
        } else {
            mantissa
                .checked_shr(shift.unsigned_abs())
                .unwrap_or_default()
        };
        let Some(square) = fixed.checked_mul(fixed) else {
            return false;
        };
        let Some(sum) = squared_norm.checked_add(square) else {
            return false;
        };
        squared_norm = sum;
    }
    let Some(scaled_norm) = squared_norm.checked_mul(DENOMINATOR) else {
        return false;
    };
    let lower = Q_SQUARED.saturating_mul(LOWER_NUMERATOR);
    let upper = Q_SQUARED.saturating_mul(UPPER_NUMERATOR);
    (lower..=upper).contains(&scaled_norm)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    struct DeclaredNormalized;

    impl EmbeddingProvider for DeclaredNormalized {
        fn id(&self) -> ModelId {
            ModelId::new("normalized-test").expect("model id")
        }

        fn dims(&self) -> u16 {
            2
        }

        fn normalized(&self) -> bool {
            true
        }

        async fn embed(&self, _batch: &[&str]) -> Result<Vec<Vector>, Error> {
            unreachable!()
        }
    }

    #[test]
    fn declared_normalized_vectors_are_verified() {
        let provider = DeclaredNormalized;
        let vector = Vector::new(vec![3.0, 4.0]).expect("finite vector");
        assert!(validate_batch(&provider, 1, &[vector]).is_err());
    }

    #[test]
    fn zero_vectors_are_rejected() {
        let provider = DeclaredNormalized;
        let vector = Vector::new(vec![0.0, 0.0]).expect("finite vector");
        assert!(validate_batch(&provider, 1, &[vector]).is_err());
    }
}
