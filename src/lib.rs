use std::sync::Arc;

use crate::decoder::VinDecoderError;
pub use build_shared::*;

pub mod build_shared;
pub mod decoder;
pub mod maps;
pub mod pattern;

#[cfg(feature = "parallel")]
pub const RAYON_CHUNK_SIZE: usize = 48;

pub type VIN = String;

#[derive(Debug)]
pub enum CorgiError {
    Shared(Arc<Self>),
    VinDecoder(VIN, VinDecoderError),
}
