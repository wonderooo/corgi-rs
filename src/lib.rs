//! Decode VINs into structured vehicle metadata using NHTSA's vPIC database.
//!
//! ```no_run
//! use corgi_rs::VinDecoder;
//!
//! let decoder = VinDecoder::new();
//! let info = decoder.decode("1C6RR7LT2JS179571").expect("valid VIN");
//!
//! assert_eq!(info.make, "Ram");
//! assert_eq!(info.model.as_deref(), Some("1500"));
//! assert_eq!(info.year, 2018);
//! ```
//!
//! # What gets decoded
//!
//! [`VehicleInfo`] names the attributes most callers want, and carries every
//! other element vPIC resolved in [`VehicleInfo::attributes`] — engine output,
//! seat count, wheelbase, airbag placement, the driver-assist systems, plant of
//! assembly, and so on. [`element::all`] lists the full catalogue.
//!
//! # Scope
//!
//! The shipped tables cover passenger cars, trucks (which is where every pickup
//! lives), multipurpose passenger vehicles and incomplete vehicles. Motorcycles,
//! buses, trailers, low-speed and off-road vehicles are excluded, which is what
//! keeps the tables to a few megabytes; decoding one of those VINs returns
//! [`decoder::WmiErrorCode::UnknownWmi`].
//!
//! # Data
//!
//! `assets/` holds a compressed export of the vPIC database, regenerated with
//! `tools/extract_assets.sql` (see `tools/README.md`). `build.rs` turns it into
//! memory-mapped `fst` tables under `$HOME/.corgi-rs-cache`, or under `MAPS_DIR`
//! if that is set.

// Keep the README honest: its code blocks run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct Readme;

pub use crate::build_shared::*;
pub use crate::decoder::{VinDecoder, VinDecoderError};
pub use crate::maps::MapError;
pub use crate::vehicle::{BodyStyle, Generation, VehicleInfo, Warning, extract_body_style};

pub mod build_shared;
pub mod decoder;
pub mod element;
pub mod maps;
pub mod pattern;
pub mod vehicle;
pub mod vin;

#[cfg(feature = "parallel")]
pub const RAYON_CHUNK_SIZE: usize = 48;

/// A VIN is represented as a string so callers can pass owned or borrowed values.
pub type VIN = String;

/// Global error type returned by VIN decoding operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorgiError {
    /// Wraps all errors emitted by [`decoder::VinDecoder`], alongside the VIN
    /// that produced them.
    VinDecoder(VIN, VinDecoderError),
}

impl CorgiError {
    /// The VIN that failed to decode.
    pub fn vin(&self) -> &str {
        match self {
            Self::VinDecoder(vin, _) => vin,
        }
    }
}

impl std::fmt::Display for CorgiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VinDecoder(_, err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CorgiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::VinDecoder(_, err) => Some(err),
        }
    }
}
