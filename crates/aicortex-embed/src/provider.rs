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
#[allow(clippy::float_arithmetic)]
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
    const NORM_TOLERANCE: f32 = 1.0e-3;
    for vector in vectors {
        let squared_norm = vector
            .values()
            .iter()
            .map(|value| value * value)
            .sum::<f32>();
        if squared_norm == 0.0 {
            return Err(Error::Integrity(format!(
                "provider {} returned a zero vector",
                provider.id()
            )));
        }
        if provider.normalized() {
            let norm = squared_norm.sqrt();
            if (norm - 1.0).abs() > NORM_TOLERANCE {
                return Err(Error::Integrity(format!(
                    "provider {} declares normalized vectors but returned norm {norm}",
                    provider.id()
                )));
            }
        }
    }
    Ok(())
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
