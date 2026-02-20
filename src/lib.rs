use std::sync::Arc;

use crate::decoder::VinDecoderError;

pub mod db;
pub mod decoder;
pub mod pattern;
pub mod types;

include!(concat!(env!("OUT_DIR"), "/gen.rs"));
include!(concat!(env!("OUT_DIR"), "/gen_wmi_schema_id.rs"));

pub type VIN = String;

#[derive(Debug)]
pub enum CorgiError {
    Shared(Arc<Self>),
    VinDecoder(VIN, VinDecoderError),
    Sqlite(sqlx::Error),
}

impl From<sqlx::Error> for CorgiError {
    fn from(value: sqlx::Error) -> Self {
        Self::Sqlite(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phf_wmi_make() {
        let v = WMI_MAKE_MAP.get("WA1").expect("is not some");
        assert_eq!(*v, "Audi")
    }

    #[test]
    fn test_phf_wmi_schema_id() {
        let v = WMI_SCHEMA_ID_MAP.get("WA1").expect("is not some");
        assert_eq!(
            *v,
            &[
                "3288", "3927", "3931", "3963", "3998", "4092", "4106", "4158", "4201", "4244",
                "4265", "8341", "8389", "8471", "8483", "10930", "15507", "20427", "21297",
                "22045", "23318", "24457", "24458", "24459", "24545", "24546", "24547", "24588",
                "24767", "25233", "25830", "25831", "26029", "26031", "26032", "26642", "27226",
                "27302", "27303", "27340", "27362", "27532", "28366", "28367", "28368", "28802"
            ]
        )
    }
}
