//! Decode VINs into structured vehicle metadata using archived patterns.
//! The crate bundles validators, lookup builders, and a decoder powered by
//! `fst` + `rkyv` to keep the runtime footprint predictable.
use crate::decoder::VinDecoderError;
pub use build_shared::*;

pub mod build_shared;
pub mod decoder;
pub mod maps;
pub mod pattern;

#[cfg(feature = "parallel")]
pub const RAYON_CHUNK_SIZE: usize = 48;

/// A VIN is represented as a string so callers can pass owned or borrowed values.
pub type VIN = String;

/// Global error type returned by VIN decoding operations.
#[derive(Debug)]
pub enum CorgiError {
    /// Wraps all errors emitted by [`decoder::VinDecoder`].
    VinDecoder(VIN, VinDecoderError),
}
