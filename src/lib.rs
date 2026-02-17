use crate::decoder::VinDecoderError;

pub mod db;
pub mod decoder;
pub mod pattern;
pub mod types;

pub type VIN = String;

#[derive(Debug)]
pub enum CorgiError {
    VinDecoder(VIN, VinDecoderError),
    Sqlite(sqlx::Error),
}

impl From<sqlx::Error> for CorgiError {
    fn from(value: sqlx::Error) -> Self {
        Self::Sqlite(value)
    }
}
