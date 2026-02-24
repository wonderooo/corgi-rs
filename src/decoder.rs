use std::collections::HashMap;

#[cfg(feature = "parallel")]
use crate::RAYON_CHUNK_SIZE;
#[cfg(feature = "parallel")]
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

pub use crate::decoder::extractors::{BodyStyle, VehicleInfo};
use crate::{
    CorgiError, Make, VIN,
    decoder::{
        extractors::{
            ModelYearErrorCode, WmiErrorCode, extract_model_year, extract_vds_vis,
            extract_vehicle_info, extract_wmi,
        },
        validators::{
            CheckDigitErrorCode, StructureErrorCode, validate_check_digit, validate_vin_structure,
        },
    },
    maps::FstRkyvMap,
    pattern::{MatchQuery, PatternMatcher},
};

#[derive(Debug)]
pub enum VinDecoderError {
    InvalidStructure {
        message: String,
        code: StructureErrorCode,
    },
    InvalidCheckDigit {
        message: String,
        code: CheckDigitErrorCode,
    },
    UnreadableModelYear {
        message: String,
        code: ModelYearErrorCode,
    },
    UnreadableWmi {
        message: String,
        code: WmiErrorCode,
    },
    Unexpected {
        message: String,
    },
}

pub struct VinDecoder {
    pattern_matcher: PatternMatcher,
    wmi_make_map: FstRkyvMap<Make>,
}

impl VinDecoder {
    pub fn new() -> Self {
        Self {
            pattern_matcher: PatternMatcher::new(),
            wmi_make_map: FstRkyvMap::new(),
        }
    }

    pub fn decode(&self, vin: &VIN) -> Result<VehicleInfo, CorgiError> {
        validate_vin_structure(&vin)?;
        validate_check_digit(&vin)?;

        let model_year = extract_model_year(&vin)?;
        let wmi = extract_wmi(&vin)?;
        let make = self
            .wmi_make_map
            .get(&wmi)
            .map(|mut ma| ma.remove(0).make)
            .unwrap_or("".to_string());
        let (vds, vis) = extract_vds_vis(&vin)?;

        let query = MatchQuery {
            wmi: &wmi,
            model_year: model_year as i32,
            vds,
            vis,
        };

        let patterns = self.pattern_matcher.matches(&query);

        let vehicle_info = extract_vehicle_info(make, model_year as i32, patterns);
        Ok(vehicle_info)
    }

    pub fn decode_batch<'inp>(
        &self,
        vins: &'inp Vec<VIN>,
    ) -> HashMap<&'inp VIN, Result<VehicleInfo, CorgiError>> {
        #[cfg(not(feature = "parallel"))]
        let decoded = vins
            .into_iter()
            .map(|vin| (vin, self.decode(&vin)))
            .collect();

        #[cfg(feature = "parallel")]
        let decoded = vins
            .into_par_iter()
            .chunks(RAYON_CHUNK_SIZE)
            .flat_map_iter(|chunk| chunk.into_iter().map(|vin| (vin, self.decode(&vin))))
            .collect();

        decoded
    }

    pub fn decode_batch_owned(
        &self,
        vins: Vec<VIN>,
    ) -> HashMap<VIN, Result<VehicleInfo, CorgiError>> {
        #[cfg(not(feature = "parallel"))]
        let decoded = vins
            .into_iter()
            .map(|vin| {
                let decoded = self.decode(&vin);
                (vin, decoded)
            })
            .collect();

        #[cfg(feature = "parallel")]
        let decoded = vins
            .into_par_iter()
            .chunks(RAYON_CHUNK_SIZE)
            .flat_map_iter(|chunk| {
                chunk.into_iter().map(|vin| {
                    let decoded = self.decode(&vin);
                    (vin, decoded)
                })
            })
            .collect();

        decoded
    }
}

pub mod extractors {
    use std::{borrow::Cow, collections::HashMap, fmt::Display, sync::LazyLock};

    use chrono::Datelike;

    use crate::{CorgiError, decoder::VinDecoderError, pattern::PatternMatch};

    // Canonical VIN character sequence for a 30-year block (1980-2009 or 2010-2039)
    static MODEL_YEAR_CODES: &[char] = &[
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'M', 'N', 'P', 'R', 'S', 'T', 'V',
        'W', 'X', 'Y', '1', '2', '3', '4', '5', '6', '7', '8', '9',
    ];

    #[derive(Debug)]
    pub enum ModelYearErrorCode {
        CharIndexOutOfBounds,
        UnencodedModelYear,
        ModelYearCodeIncompatibile,
    }

    pub fn extract_model_year(vin: &String) -> Result<u32, CorgiError> {
        let year_char =
            vin.chars()
                .nth(9)
                .map(|c| c.to_ascii_uppercase())
                .ok_or(CorgiError::VinDecoder(
                    vin.clone(),
                    VinDecoderError::UnreadableModelYear {
                        message: format!(
                            "Could not read year char at position 9 for VIN number: {vin}"
                        ),
                        code: ModelYearErrorCode::CharIndexOutOfBounds,
                    },
                ))?;
        let decade_block_char = vin.chars().nth(6).ok_or(CorgiError::VinDecoder(
            vin.clone(),
            VinDecoderError::UnreadableModelYear {
                message: format!(
                    "Could not read decade block char at position 6 for VIN number: {vin}"
                ),
                code: ModelYearErrorCode::CharIndexOutOfBounds,
            },
        ))?;

        //
        // Handle case when year char is 0 - some countries do not encode model years inside VIN
        //
        if year_char == '0' {
            return Err(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::UnreadableModelYear {
                    message: format!("Model year is not encoded for VIN number: {vin}"),
                    code: ModelYearErrorCode::UnencodedModelYear,
                },
            ));
        }

        let code_index = MODEL_YEAR_CODES
            .iter()
            .position(|c| *c == year_char)
            .ok_or(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::UnreadableModelYear {
                    message: format!(
                        "Year char `{year_char}` is not found in model year codes for VIN number: {vin}"
                    ),
                    code: ModelYearErrorCode::ModelYearCodeIncompatibile,
                },
            ))? as u32;

        let base_year = if decade_block_char as usize >= 48 && decade_block_char as usize <= 57 {
            1980
        } else {
            2010
        };
        let mut adjusted_year = base_year + code_index;
        let next_year = chrono::Utc::now().year() + 1;
        if adjusted_year as i32 > next_year {
            adjusted_year -= 30
        }

        Ok(adjusted_year)
    }

    #[derive(Debug)]
    pub enum WmiErrorCode {
        CharIndexOutOfBounds,
        InvalidVinLength,
    }

    pub fn extract_wmi(vin: &String) -> Result<Cow<'_, str>, CorgiError> {
        if vin.len() < 3 {
            return Err(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::UnreadableWmi {
                    message: format!(
                        "Could not read base WMI, invalid VIN length for VIN number: {vin}"
                    ),
                    code: WmiErrorCode::InvalidVinLength,
                },
            ));
        }

        let base_wmi = &vin[0..3];
        let extended_wmi_char = base_wmi.chars().nth(2).ok_or(CorgiError::VinDecoder(
            vin.clone(),
            VinDecoderError::UnreadableWmi {
                message: format!(
                    "Could not check extended WMI at position 2 for VIN number: {vin}"
                ),
                code: WmiErrorCode::CharIndexOutOfBounds,
            },
        ))?;

        if extended_wmi_char == '9' && vin.len() >= 14 {
            let extended_wmi = &vin[11..14];
            let joined = [base_wmi, extended_wmi].concat();
            return Ok(Cow::Owned(joined));
        }

        Ok(Cow::Borrowed(base_wmi))
    }

    pub type Vds<'a> = &'a str;
    pub type Vis<'a> = &'a str;
    pub fn extract_vds_vis(vin: &'_ String) -> Result<(Vds<'_>, Vis<'_>), CorgiError> {
        let vds = &vin[3..9];
        let vis = &vin[9..17];
        Ok((vds, vis))
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
    pub struct VehicleInfo {
        pub make: String,
        pub model: Option<String>,
        pub year: i32,
        pub series: Option<String>,
        pub trim: Option<String>,
        pub body_style: Option<BodyStyle>,
        pub drive_type: Option<String>,
        pub engine_type: Option<String>,
        pub fuel_type: Option<String>,
        pub transmission: Option<String>,
        pub doors: Option<i32>,
        pub gvwr: Option<String>,
        pub manufacturer: Option<String>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum BodyStyle {
        Sedan,
        Coupe,
        Convertible,
        Hatchback,
        Wagon,
        Suv,
        Van,
        Minivan,
        Pickup,
        Truck,
        Trailer,
        Tractor,
        Bus,
        Motorcycle,
        Other,
    }

    pub fn extract_vehicle_info(
        make: String,
        model_year: i32,
        patterns: Vec<PatternMatch>,
    ) -> VehicleInfo {
        let mut vehicle_info = VehicleInfo {
            make,
            model: None,
            year: model_year,
            series: None,
            trim: None,
            body_style: None,
            drive_type: None,
            engine_type: None,
            fuel_type: None,
            transmission: None,
            doors: None,
            gvwr: None,
            manufacturer: None,
        };

        //
        // Keep track of both fuel types to determine if vehicle is hybrid
        //
        let mut primary_fuel_type = None;
        let mut secondary_fuel_type = None;
        patterns
            .into_iter()
            .for_each(|pattern| match pattern.lookup.element_code.as_str() {
                "Make" => vehicle_info.make = pattern.lookup.resolved,
                "Model" => vehicle_info.model = Some(pattern.lookup.resolved),
                "Series" => vehicle_info.series = Some(pattern.lookup.resolved),
                "Trim" | "TrimLevel" => vehicle_info.trim = Some(pattern.lookup.resolved),
                "BodyClass" | "BodyStyle" => {
                    vehicle_info.body_style = Some(extract_body_style(&pattern.lookup.resolved))
                }
                "DriveType" => vehicle_info.drive_type = Some(pattern.lookup.resolved),
                "FuelTypePrimary" => primary_fuel_type = Some(pattern.lookup.resolved),
                "FuelTypeSecondary" => secondary_fuel_type = Some(pattern.lookup.resolved),
                "Transmission" => vehicle_info.transmission = Some(pattern.lookup.resolved),
                "Doors" => vehicle_info.doors = pattern.lookup.resolved.parse::<i32>().ok(),
                "GVWR" => vehicle_info.gvwr = Some(pattern.lookup.resolved),
                _ => {}
            });

        //
        // Set fuelType to Hybrid only if both fuel types are present and one is electric
        //
        if let Some(ref primary_fuel) = primary_fuel_type
            && let Some(ref secondary_fuel) = secondary_fuel_type
            && (primary_fuel.to_lowercase().contains("electric")
                || secondary_fuel.to_lowercase().contains("electric"))
        {
            vehicle_info.fuel_type = Some("Hybrid".to_string())
        } else {
            vehicle_info.fuel_type = primary_fuel_type
        }

        vehicle_info
    }

    impl From<&str> for BodyStyle {
        fn from(value: &str) -> Self {
            match value {
                // Sedans and coupes
                "Sedan/Saloon" | "Sedan" | "4-Door Sedan" | "2-Door Sedan" | "4-Door Saloon" => {
                    BodyStyle::Sedan
                }
                "Coupe" | "2-Door Coupe" => BodyStyle::Coupe,
                "Convertible" | "2-Door Convertible" | "4-Door Convertible" => {
                    BodyStyle::Convertible
                }

                // Hatchbacks and wagons
                "Hatchback" | "3-Door Hatchback" | "5-Door Hatchback" => BodyStyle::Hatchback,
                "Station Wagon" | "Wagon" => BodyStyle::Wagon,

                // SUVs and crossovers
                "Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)"
                | "Sport Utility Vehicle (SUV)"
                | "SUV"
                | "Crossover Utility Vehicle (CUV)"
                | "Crossover" => BodyStyle::Suv,

                // Vans and minivans
                "Van" | "Cargo Van" | "Passenger Van" => BodyStyle::Van,
                "Minivan" => BodyStyle::Minivan,

                // Trucks, pickups, trailers
                "Pickup"
                | "Pickup Truck"
                | "Standard Pickup Truck"
                | "Extended Cab Pickup"
                | "Crew Cab Pickup" => BodyStyle::Pickup,
                "Truck" => BodyStyle::Truck,
                "Trailer" => BodyStyle::Trailer,
                "Tractor" => BodyStyle::Tractor,

                // Bus
                "Bus" | "School Bus" => BodyStyle::Bus,

                // Motorcycle
                "Motorcycle" => BodyStyle::Motorcycle,

                // Catch-all
                "Incomplete Vehicle" | "Other" => BodyStyle::Other,

                // Default for unknown strings
                _ => BodyStyle::Other,
            }
        }
    }

    impl Display for BodyStyle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let text = match self {
                Self::Sedan => "Sedan",
                Self::Coupe => "Coupe",
                Self::Convertible => "Convertible",
                Self::Hatchback => "Hatchback",
                Self::Wagon => "Wagon",
                Self::Suv => "Suv",
                Self::Van => "Van",
                Self::Minivan => "Minivan",
                Self::Pickup => "Pickup",
                Self::Truck => "Truck",
                Self::Trailer => "Trailer",
                Self::Tractor => "Tractor",
                Self::Bus => "Bus",
                Self::Motorcycle => "Motorcycle",
                Self::Other => "Other",
            };

            write!(f, "{text}")
        }
    }

    pub static BODY_STYLE_MAP: LazyLock<HashMap<&'static str, BodyStyle>> = LazyLock::new(|| {
        let mut m = HashMap::with_capacity(64);
        // Sedans and coupes
        m.insert("Sedan/Saloon", BodyStyle::Sedan);
        m.insert("Sedan", BodyStyle::Sedan);
        m.insert("4-Door Sedan", BodyStyle::Sedan);
        m.insert("2-Door Sedan", BodyStyle::Sedan);
        m.insert("4-Door Saloon", BodyStyle::Sedan);

        m.insert("Coupe", BodyStyle::Coupe);
        m.insert("2-Door Coupe", BodyStyle::Coupe);
        m.insert("Convertible", BodyStyle::Convertible);
        m.insert("2-Door Convertible", BodyStyle::Convertible);
        m.insert("4-Door Convertible", BodyStyle::Convertible);

        // Hatchbacks and wagons
        m.insert("Hatchback", BodyStyle::Hatchback);
        m.insert("3-Door Hatchback", BodyStyle::Hatchback);
        m.insert("5-Door Hatchback", BodyStyle::Hatchback);
        m.insert("Station Wagon", BodyStyle::Wagon);
        m.insert("Wagon", BodyStyle::Wagon);

        // SUVs
        m.insert(
            "Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)",
            BodyStyle::Suv,
        );
        m.insert("Sport Utility Vehicle (SUV)", BodyStyle::Suv);
        m.insert("SUV", BodyStyle::Suv);
        m.insert("Crossover Utility Vehicle (CUV)", BodyStyle::Suv);
        m.insert("Crossover", BodyStyle::Suv);

        // Vans
        m.insert("Van", BodyStyle::Van);
        m.insert("Cargo Van", BodyStyle::Van);
        m.insert("Passenger Van", BodyStyle::Van);
        m.insert("Minivan", BodyStyle::Minivan);

        // Trucks
        m.insert("Pickup", BodyStyle::Pickup);
        m.insert("Pickup Truck", BodyStyle::Pickup);
        m.insert("Standard Pickup Truck", BodyStyle::Pickup);
        m.insert("Extended Cab Pickup", BodyStyle::Pickup);
        m.insert("Crew Cab Pickup", BodyStyle::Pickup);
        m.insert("Truck", BodyStyle::Truck);
        m.insert("Trailer", BodyStyle::Trailer);
        m.insert("Tractor", BodyStyle::Tractor);

        // Bus
        m.insert("Bus", BodyStyle::Bus);
        m.insert("School Bus", BodyStyle::Bus);

        // Motorcycle
        m.insert("Motorcycle", BodyStyle::Motorcycle);

        // Catch-all
        m.insert("Incomplete Vehicle", BodyStyle::Other);
        m.insert("Other", BodyStyle::Other);

        m
    });

    pub fn extract_body_style(raw_body_style: &String) -> BodyStyle {
        //
        // Match exact entries
        //
        if let Some(body_style) = BODY_STYLE_MAP.get(raw_body_style.as_str()) {
            return *body_style;
        }

        //
        // Fuzzy match based on substring
        //
        if let Some(body_style) = BODY_STYLE_MAP
            .iter()
            .filter_map(|(bs_key, bs_value)| {
                if raw_body_style
                    .to_lowercase()
                    .contains(&bs_key.to_lowercase())
                    || bs_key
                        .to_lowercase()
                        .contains(&raw_body_style.to_lowercase())
                {
                    return Some(*bs_value);
                }
                None
            })
            .collect::<Vec<_>>()
            .first()
        {
            return *body_style;
        }

        //
        // Handle common keywords
        //
        if raw_body_style.to_lowercase().contains("truck")
            || raw_body_style.to_lowercase().contains("pickup")
        {
            return BodyStyle::Pickup;
        }

        BodyStyle::Other
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_extractors_model_year() {
            let vin = "1HGCP26739A060971".to_string();
            let y = extract_model_year(&vin).unwrap();
            assert_eq!(y, 2009)
        }
    }
}

pub mod validators {
    use crate::{CorgiError, decoder::VinDecoderError};

    #[derive(Debug)]
    pub enum StructureErrorCode {
        InvalidLength,
        InvalidCharacters,
    }

    pub fn validate_vin_structure(vin: &String) -> Result<(), CorgiError> {
        if vin.len() != 17 {
            return Err(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::InvalidStructure {
                    message: format!("Invalid length of VIN number: {vin}"),
                    code: StructureErrorCode::InvalidLength,
                },
            ));
        }

        //
        // Character must be 0-9 or A-Z (except I, O, Q)
        //
        let general_char_test = |c: char| {
            c.is_ascii_digit() || matches!(c, 'A'..='H' | 'J'..='N' | 'P'..='R' | 'S'..='Z')
        };

        struct InvalidCharacter {
            character: char,
            position: usize,
        }
        let invalid_chars =
            vin.chars()
                .into_iter()
                .enumerate()
                .fold(Vec::new(), |mut accu, (idx, c)| {
                    match idx {
                        8 if !(c.is_ascii_digit() || c == 'X') => accu.push(InvalidCharacter {
                            character: c,
                            position: idx,
                        }),
                        _ if !general_char_test(c) => accu.push(InvalidCharacter {
                            character: c,
                            position: idx,
                        }),
                        _ => {}
                    }
                    accu
                });

        if invalid_chars.len() > 0 {
            return Err(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::InvalidStructure {
                    message: format!(
                        "Invalid characters for VIN number: {vin}, characters: {}",
                        invalid_chars
                            .iter()
                            .map(|ic| format!("{} at position {}", ic.character, ic.position))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    code: StructureErrorCode::InvalidCharacters,
                },
            ));
        }

        Ok(())
    }

    #[derive(Debug)]
    pub enum CheckDigitErrorCode {
        NumberIsNotCharacter,
        ActualIsNotExpected,
    }

    pub fn validate_check_digit(vin: &String) -> Result<char, CorgiError> {
        //
        // Check digit weights according to CFR Title 49 § 565.15(c)
        //
        const WEIGHTS: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];

        //
        // Transliterate characters to numerical values according to CFR Title 49 § 565.15(c)
        //
        fn transliterate(c: char) -> u32 {
            match c.to_ascii_uppercase() {
                '0'..='9' => unsafe { c.to_digit(10).unwrap_unchecked() },

                'A' => 1,
                'B' => 2,
                'C' => 3,
                'D' => 4,
                'E' => 5,
                'F' => 6,
                'G' => 7,
                'H' => 8,

                'J' => 1,
                'K' => 2,
                'L' => 3,
                'M' => 4,
                'N' => 5,
                'P' => 7,
                'R' => 9,

                'S' => 2,
                'T' => 3,
                'U' => 4,
                'V' => 5,
                'W' => 6,
                'X' => 7,
                'Y' => 8,
                'Z' => 9,

                _ => 0,
            }
        }

        //
        // Calculate weighted sum
        //
        let sum: u32 = vin
            .chars()
            .zip(WEIGHTS)
            .map(|(c, w)| transliterate(c) * w)
            .sum();

        //
        // Calculate check digit
        //
        let calculated = sum % 11;

        let expected = if calculated == 10 {
            'X'
        } else {
            char::from_digit(calculated, 10).ok_or(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::InvalidCheckDigit {
                    message: format!("Calculated char is not a valid digit: {calculated}"),
                    code: CheckDigitErrorCode::NumberIsNotCharacter,
                },
            ))?
        };

        let actual = vin.chars().nth(8).unwrap_or(' ').to_ascii_uppercase();

        if actual != expected {
            return Err(CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::InvalidCheckDigit {
                    message: format!(
                        "Actual check digit does not match expected, actual: {actual}, expected: {expected}"
                    ),
                    code: CheckDigitErrorCode::ActualIsNotExpected,
                },
            ));
        }

        Ok(actual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoder_simple() -> Result<(), CorgiError> {
        let decoder = VinDecoder::new();
        let info = decoder.decode(&"2FTEF14H8TCA73155".to_string())?;
        assert_eq!(info.make, "Ford".to_string());
        assert_eq!(info.model, Some("F-150".to_string()));
        assert_eq!(info.year, 1996);

        Ok(())
    }

    #[test]
    fn test_decoder_simple_batch() {
        let decoder = VinDecoder::new();
        let vins = vec![
            "KM8K2CAB4PU001140".to_string(),
            "5N1AT2MT9LC784186".to_string(),
            "2FTEF14H8TCA73155".to_string(),
            "1FTFW1ET6DFA4553".to_string(),
        ];

        let start = std::time::Instant::now();
        vins.into_iter().for_each(|v| {
            let _ = decoder.decode(&v);
        });
        let elapsed = start.elapsed();
        println!("{}", elapsed.as_millis())
    }

    #[cfg(feature = "parallel")]
    #[test]
    fn test_decoder_many_batch() {
        let decoder = VinDecoder::new();
        let vins = vec![
            "WA1LAAF77HD021575".to_string(),
            "4T1BF1FK8HU640530".to_string(),
            "1C4RJFBG4KC715074".to_string(),
            "WDC0G4KB8KV146633".to_string(),
            "1C4RJECG1HC752004".to_string(),
            "5NPE34AF0HH479607".to_string(),
            "2HGFC2F7XJH549558".to_string(),
            "1C4RJEAG0KC776806".to_string(),
            "1C6SRFLT5KN851019".to_string(),
            "WDDUG8FB9HA309501".to_string(),
            "KM8SN4HF8HU190561".to_string(),
            "1FTEW1C87GFA06963".to_string(),
            "1N4AL3AP6JC217082".to_string(),
            "5N1AZ2MG1HN139139".to_string(),
            "3N1AB7AP6KY347685".to_string(),
            "1G1BE5SM6H7110182".to_string(),
            "WP0CA2A82KS210214".to_string(),
            "JF1VA1A61K9822032".to_string(),
            "4T1B11HK5KU711718".to_string(),
            "3N1AB7AP7HY409281".to_string(),
            "1C3CDFAA9GD767462".to_string(),
            "4T1BF1FK5GU217984".to_string(),
            "1C4RJFBG9JC426531".to_string(),
            "KNDJN2A28G7314644".to_string(),
            "JN1BJ1CP5KW239521".to_string(),
            "KNDJN2A21G7409188".to_string(),
            "1FMCU0J97HUA59657".to_string(),
            "2GNFLFEK4H6216894".to_string(),
            "1FM5K8D83GGD19954".to_string(),
            "5FNYF5H5XJB008110".to_string(),
            "5FNRL5H93HB006774".to_string(),
            "3VW5T7AU9KM009686".to_string(),
            "2C4RDGEG2KR562615".to_string(),
            "JN1FV7EL1JM630789".to_string(),
            "2C3CDXJG8HH653225".to_string(),
            "1GB0GRFG8K1366438".to_string(),
            "KNAFK4A65G5507517".to_string(),
            "1HGCV1F34JA007665".to_string(),
            "3CZRM3H35GG701063".to_string(),
            "1HGCR3F81HA039270".to_string(),
            "1N4AL3AP5JC234925".to_string(),
            "1HGCV1F19JA206502".to_string(),
            "3GNAXJEV1JS540873".to_string(),
            "1FADP3L94GL207997".to_string(),
            "1GCGTDEN1J1304497".to_string(),
            "3FA6P0H98GR249199".to_string(),
            "5NPD74LF5KH467764".to_string(),
            "KNDCB3LC0H5091283".to_string(),
            "1N4BL4BV2KC169203".to_string(),
            "KM8J3CAL1KU034557".to_string(),
            "3N6CM0KN8JK698083".to_string(),
            "3CZRU5H34JM709224".to_string(),
            "3GNAXUEV4KL295841".to_string(),
            "KNDPM3AC1H7277160".to_string(),
            "ZACCJBDB2JPH36130".to_string(),
            "KNADM4A34H6016357".to_string(),
            "JF2GTAMC8J8257377".to_string(),
            "5J8TB4H3XHL018031".to_string(),
            "5TFUW5F19GX494048".to_string(),
            "1FADP3F22JL231172".to_string(),
            "1GNSKCKC6HR236503".to_string(),
            "5YJ3E1EAXKF410766".to_string(),
            "JN1BJ0RR9HM405174".to_string(),
            "5NPD84LF1HH137450".to_string(),
            "KNDJN2A2XK7014063".to_string(),
            "JTEZU5JR0H5149797".to_string(),
            "1GKKNLLS1JZ205594".to_string(),
            "1HGCR2F12HA300225".to_string(),
            "1FAHP2E88GG142600".to_string(),
            "4T1BF1FK7HU755507".to_string(),
            "1G1FB1RS9H0163653".to_string(),
            "1FTEW1E58KFA27820".to_string(),
            "3CZRU6H30JG727756".to_string(),
            "1FA6P8TH2K5132582".to_string(),
            "SALWR2RK7JA405177".to_string(),
            "1FTEW1E51JFB67593".to_string(),
            "JF2SJGXC0GH540959".to_string(),
            "5N1DR2MN6KC641367".to_string(),
            "5FNYF5H91KB004659".to_string(),
            "55SWF4KBXJU261278".to_string(),
            "1GYKNERS3HZ113616".to_string(),
            "SALRT2RV1K2405678".to_string(),
            "WDDUG8FB2HA335969".to_string(),
            "5FNYF6H33HB063495".to_string(),
            "WBAJE7C34HG887039".to_string(),
            "19UDE2F36JA010223".to_string(),
            "5N1AT2MV0HC820212".to_string(),
            "ZASFAKNN0J7B66669".to_string(),
            "1N4AL3AP6JC168000".to_string(),
            "ML32F3FJ3KHF15870".to_string(),
            "1HGCV1F38JA230842".to_string(),
            "5YJXCBE28GF016969".to_string(),
            "1HGCV2F95JA039888".to_string(),
            "JN1CV7AP1GM202841".to_string(),
            "WBXHU7C30H5H34373".to_string(),
            "19UDE2F78KA005884".to_string(),
            "5YFBURHE2JP827104".to_string(),
            "ZACCJBDT4GPD30917".to_string(),
            "5XYPGDA30GG112327".to_string(),
            "1C6RR6GT1KS671076".to_string(),
            "4T1BF1FK4GU204725".to_string(),
            "2HGFC3B7XHH358485".to_string(),
            "3VW2B7AJ6HM377067".to_string(),
            "JM3KE4BY0G0766624".to_string(),
            "1C6RR6YT2HS591700".to_string(),
            "5TDKZ3DC8HS776292".to_string(),
            "5TFRX5GN5HX086717".to_string(),
            "1GKKVRKDXGJ224643".to_string(),
            "JM1GJ1V52G1439999".to_string(),
            "3GKALMEV9JL360075".to_string(),
            "5NPE34AF1JH647941".to_string(),
            "4T1B11HK8JU144760".to_string(),
            "1C6RR7MTXGS318224".to_string(),
            "4T1BF1FK5HU781281".to_string(),
            "KL4CJGSM0KB932185".to_string(),
            "ZACCJADTXGPC89834".to_string(),
            "1GYS4DKJ2HR124900".to_string(),
            "WBA8E1G5XGNT36809".to_string(),
            "3N1CN7AP7KL876009".to_string(),
            "1C3CDFFA0GD822485".to_string(),
            "1N4AZ1CP9JC300537".to_string(),
            "SHHFK7H49KU424770".to_string(),
            "1FMCU9HD4JUA26619".to_string(),
            "KNAFZ4A83G5575523".to_string(),
            "1FADP3F20JL273176".to_string(),
            "4T1BF1FK4HU429860".to_string(),
            "1HGCV3F9XKA006926".to_string(),
            "19XFC2F51GE228204".to_string(),
            "58ABK1GG6GU026525".to_string(),
            "1FMCU0GD9HUB49856".to_string(),
            "58ABZ1B19KU008401".to_string(),
            "LRBFXBSA6HD216381".to_string(),
            "55SWF4KB6GU163776".to_string(),
            "5N1AT2MMXGC801169".to_string(),
            "1GNKVGKD8HJ107250".to_string(),
            "WMZYS7C30J3E07881".to_string(),
            "5YFBURHE8JP837488".to_string(),
            "YV449MDK3G2879791".to_string(),
            "19UUB2F69JA008260".to_string(),
            "3N1AB7AP5KY443694".to_string(),
            "2GKALNEK7G6334034".to_string(),
            "KNDJP3A59G7386849".to_string(),
            "KM8J3CA21HU367650".to_string(),
            "1FATP8EM5G5326933".to_string(),
            "KMHD74LF4HU141822".to_string(),
            "1FMCU0GD5JUB17458".to_string(),
            "WAUW2AFC1HN019759".to_string(),
            "2FMPK4AP5KBB09238".to_string(),
            "1C4HJXDG1KW691040".to_string(),
            "3N1CN7AP9HL872679".to_string(),
            "3C4PDCABXJT520716".to_string(),
            "ZFBERFAB8J6L20604".to_string(),
            "ZAM57XSA5J1276987".to_string(),
            "1HGCR3F02HA022035".to_string(),
            "1FM5K7D80JGA63421".to_string(),
            "1N4BL4BV0KC118976".to_string(),
            "3N1AB7AP8KY329642".to_string(),
            "1G4ZP5SS2HU179821".to_string(),
            "WA1L2AFP8HA059878".to_string(),
            "ZACCJABT9GPD08957".to_string(),
            "1FMCU0GD6HUD97031".to_string(),
            "1FMCU0GD0HUC93330".to_string(),
            "5FPYK2F23KB006484".to_string(),
            "3N1CN7AP7GL842529".to_string(),
            "3N1AB7AP7KY409689".to_string(),
            "1FTYR1ZM0HKA09138".to_string(),
            "KNAFK4A62G5602620".to_string(),
            "5N1AT2MV6GC823775".to_string(),
            "3FA6P0HD3HR263761".to_string(),
            "JTHC81D24K5040061".to_string(),
            "3GTU2PEC8GG109495".to_string(),
            "5FPYK3F67KB002317".to_string(),
            "1FTEW1C43KFA82346".to_string(),
            "KMHDH4AE0GU581803".to_string(),
            "19XFC2F79JE201986".to_string(),
            "WBXHT3C31H5F86150".to_string(),
            "2GKALPEK4G6278548".to_string(),
            "WBAJA5C56KWW11864".to_string(),
            "WBA5R7C57KFH14445".to_string(),
            "1FTEW1EP2JFC14163".to_string(),
            "3LN6L2G94GR626206".to_string(),
            "JTEBU5JR5K5658731".to_string(),
            "5N1AT2MT7HC825034".to_string(),
            "KNMAT2MT8HP574182".to_string(),
            "3FA6P0G78GR339209".to_string(),
            "4T1B11HKXKU719443".to_string(),
            "1FM5K8GT1HGA08380".to_string(),
            "5YJ3E1EB8JF097840".to_string(),
            "JN8AT2MV1KW377342".to_string(),
            "1C6RR6GG9HS805408".to_string(),
            "3N1CN7AP9HL850827".to_string(),
            "WA1VAAF73JD045726".to_string(),
            "SHHFK7H47JU419646".to_string(),
            "1GKS2BKC0GR225194".to_string(),
            "2C3CDZAG4JH245502".to_string(),
            "JTEBU5JR6J5576330".to_string(),
            "1FTEW1EGXJFE12683".to_string(),
            "1FAHP2D8XKG117529".to_string(),
            "SALVR2BG8GH137953".to_string(),
            "1FMCU0JD4HUE65514".to_string(),
            "1GYKNFRS8JZ236677".to_string(),
            "ZFBCFXBTXGP418825".to_string(),
            "3GCUKREC9JG237442".to_string(),
            "1C4PJMDX2KD465515".to_string(),
            "1HGCR2F34GA000699".to_string(),
            "3GNAXHEV9JL291600".to_string(),
            "3N1AB7AP4KY236312".to_string(),
            "4T1BF1FK4HU701212".to_string(),
            "3C63RRALXGG155507".to_string(),
            "1N6DD0EV8KN756446".to_string(),
            "1C3CCCAB9GN107247".to_string(),
            "5FNYF6H38HB017144".to_string(),
            "ML32A3HJXJH005569".to_string(),
            "KM8J3CA25JU756104".to_string(),
            "2G1105S35H9190879".to_string(),
            "5TDYZ3DC5JS951792".to_string(),
            "JF2GTANCXK8394577".to_string(),
            "4T1BF1FK8HU444118".to_string(),
            "5YJSA1E27JF234225".to_string(),
            "1FTEW1E42KFC20908".to_string(),
            "1FTYR1ZM7HKA88923".to_string(),
            "3GKALMEV7JL140823".to_string(),
            "4T4BF1FK8GR563100".to_string(),
            "1FMCU9GX1GUA51045".to_string(),
            "1C3CCCAB2GN153292".to_string(),
            "1C4RDJEG7GC362522".to_string(),
            "3N1AB7AP6KY454610".to_string(),
            "WP1AB2A52HLB17916".to_string(),
            "1FAHP2J81HG122955".to_string(),
            "WDDWF4KB4HR215845".to_string(),
            "1G1BE5SM6J7159517".to_string(),
            "1N4AL3APXGC150043".to_string(),
            "5NPE24AF7GH325803".to_string(),
            "3N1CE2CP8GL389909".to_string(),
            "5XYPGDA38JG367556".to_string(),
            "1FA6P8CF1G5271804".to_string(),
            "1GTN1LEC8HZ905066".to_string(),
            "1GCHSBEA9K1188637".to_string(),
            "2HGFC2F67JH579365".to_string(),
            "JM3KFBCM4J0352069".to_string(),
            "1FTEW1EG3HFB16656".to_string(),
            "KNDPNCACXH7222090".to_string(),
            "KNDCB3LC2H5062058".to_string(),
            "WBA8E9C54GP847621".to_string(),
            "3GCUKREC1GG366204".to_string(),
            "3VWC57BU1KM036560".to_string(),
            "1FTEW1EF6HFC65215".to_string(),
            "2HKRW2H59KH609255".to_string(),
            "55SWF4JB1JU257329".to_string(),
            "1FM5K7DH1HGA14500".to_string(),
            "5UXTR9C5XJLC80820".to_string(),
            "1N4BL4BV7KC179953".to_string(),
            "1FMCU0HD1JUA17775".to_string(),
            "1GCUKREC1JF200922".to_string(),
            "3GCUKREC5GG167236".to_string(),
            "2C3CDXGJ1HH634759".to_string(),
            "3N1CE2CPXGL370813".to_string(),
            "JM3TCBDY3J0201304".to_string(),
            "4S3GKAB64H3616178".to_string(),
            "2C3CDZAG1HH640716".to_string(),
            "1N4AL3AP8JC224373".to_string(),
            "3GCPCREC9JG461952".to_string(),
            "3CZRU6H18KG708677".to_string(),
            "WVGAV7AX7HK014080".to_string(),
            "3KPFL4A7XJE200476".to_string(),
            "4T1B11HK0KU190343".to_string(),
            "KMHDH4AE8GU490567".to_string(),
            "5N1AZ2MJ8KN130750".to_string(),
            "3LN6L2J97GR624085".to_string(),
            "5N1DR2MM3JC673831".to_string(),
            "WAUKMAF44HN044477".to_string(),
            "4T1B11HK6JU042146".to_string(),
            "SHHFK7H42HU411030".to_string(),
            "5FNRL5H62GB083799".to_string(),
            "WDDZF4KB7JA413983".to_string(),
            "5UXTR7C59KLF33048".to_string(),
            "1G1BE5SM5H7122419".to_string(),
            "1GCRYEED0KZ314299".to_string(),
            "1G1BE5SM3J7128452".to_string(),
            "5FNYF6H9XJB000470".to_string(),
            "3VWYT7AU9GM004427".to_string(),
            "19XFC2F54GE063670".to_string(),
            "2HGFC1F70GH654399".to_string(),
            "SALCR2BGXHH714877".to_string(),
            "2C4RDGEG7JR147000".to_string(),
            "4S4BSANC4K3211940".to_string(),
            "WDDPK3JA2JF149385".to_string(),
            "1FMCU0F7XHUB26307".to_string(),
            "3FA6P0CD5KR219792".to_string(),
            "19XFC2F7XGE067109".to_string(),
            "3GYFNBE30GS562990".to_string(),
            "5NPE24AF0GH360103".to_string(),
            "2C3CCAEG4GH216623".to_string(),
            "5LMJJ2JT0JEL12421".to_string(),
            "5FNYF5H57GB057208".to_string(),
            "1C4PJMLB0KD287481".to_string(),
            "1G1BH5SE5H7263393".to_string(),
            "1C4RJFBG4HC727900".to_string(),
            "KL8CD6SA6HC769082".to_string(),
            "YV140MTL5J2451181".to_string(),
            "3GCPCNEC2GG222745".to_string(),
            "JA4AD3A35JZ044313".to_string(),
            "JTJYARBZ9G2024602".to_string(),
            "1N4AL3AP2GC211935".to_string(),
            "4S4BSENC1H3411660".to_string(),
            "3TMDZ5BN5KM078948".to_string(),
            "3FA6P0HD6GR334272".to_string(),
            "WAUM2AFR9GA011507".to_string(),
            "4JGDA5JB8JB104541".to_string(),
            "1G6AB1RX1G0165072".to_string(),
            "5FRYD4H41GB023156".to_string(),
            "5TDYZ3DC9JS914499".to_string(),
            "2GNALDEKXH6126633".to_string(),
            "1GCGSDE34G1353901".to_string(),
            "1VWAA7A35JC050408".to_string(),
            "5NPD84LF1JH215537".to_string(),
            "3FA6P0T95HR166502".to_string(),
            "5NPD74LF5JH386973".to_string(),
            "1GCVKPEHXHZ195829".to_string(),
            "2T2BZMCA5HC058306".to_string(),
            "JTJYARBZ7J2109350".to_string(),
            "1C4PJLAB4HW644641".to_string(),
            "1C6RR7NT2GS131798".to_string(),
            "JA4AP3AU0HZ067587".to_string(),
            "19XFC2F54GE079660".to_string(),
            "1FMCU0GD9HUE45783".to_string(),
            "3N1AB7AP2GY252533".to_string(),
            "5NPE24AF8HH445529".to_string(),
            "JTJBARBZ4G2083868".to_string(),
            "3N1AB7AP3JY233027".to_string(),
            "5YJSA1E27GF134893".to_string(),
            "JTHBW1GGXG2111667".to_string(),
            "5GAKRBKD7HJ324791".to_string(),
            "1VWBT7A34GC062346".to_string(),
            "3GCUKREC0JG571861".to_string(),
            "5J6RM3H70GL024882".to_string(),
            "1HGCT1B80GA011793".to_string(),
            "4S3BNBE66G3047836".to_string(),
            "2T1BURHE4HC843069".to_string(),
            "2C3CDXBG4HH657013".to_string(),
            "5TDBZRFH9KS936537".to_string(),
            "JM3TCBDY8H0140509".to_string(),
            "1G1BC5SM6J7200167".to_string(),
            "1FTEW1EF7GFD23590".to_string(),
            "KNDJP3A58G7241673".to_string(),
            "5XYZU3LB7GG316784".to_string(),
            "1N6BF0KM9GN810969".to_string(),
            "5NPE34AF5GH423273".to_string(),
            "2T1BURHE3HC790901".to_string(),
            "ZN661YUL7HX212034".to_string(),
            "2T1BURHE1GC621054".to_string(),
            "2HGFC4B07HH306692".to_string(),
            "2HKRW2H50JH681671".to_string(),
            "KNDJN2A27J7522294".to_string(),
            "KM8SR4HF3HU186227".to_string(),
            "1N6AD0ER3GN732570".to_string(),
            "2HKRM4H55GH623968".to_string(),
            "2C3CDXHG9GH222705".to_string(),
            "JF2SJAGC8HH421703".to_string(),
            "19XFC1F30KE018596".to_string(),
            "3FADP4EJ5HM134457".to_string(),
            "4T1BZ1HK5KU023274".to_string(),
            "WDC0G4KB3HV008359".to_string(),
            "5N1AZ2MH6HN137072".to_string(),
            "2T3ZFREV9GW259316".to_string(),
            "3KPFL4A72JE186122".to_string(),
            "3GCUKREC6GG186961".to_string(),
            "1C6RR7LG1GS309254".to_string(),
            "WAU8DAF82KN009571".to_string(),
            "WAU34AFD3GN007682".to_string(),
            "1G1ZD5ST2KF226906".to_string(),
            "3C4NJCCB6JT358399".to_string(),
            "WBXYJ5C36JEF81114".to_string(),
            "4T1BF1FK8HU319832".to_string(),
            "WBAJV6C58JBK07030".to_string(),
            "2T3BFREV0HW604641".to_string(),
            "5UXXW7C54H0U24918".to_string(),
            "1HGCV1F5XJA067922".to_string(),
            "1GKKNULS0JZ221372".to_string(),
            "1N4AL3AP2HN353210".to_string(),
            "2G61P5S3XG9210712".to_string(),
            "KNMAT2MV1JP534503".to_string(),
            "2GNALCEKXG6106455".to_string(),
            "3N1CP5CU2JL546570".to_string(),
            "5N1DR2MN8HC606824".to_string(),
            "2T1BURHE8KC225485".to_string(),
            "1GNERGKW8KJ302131".to_string(),
            "KNDPM3AC8K7539974".to_string(),
            "2HGFC1F35HH656115".to_string(),
            "1C6RR6KT1JS155029".to_string(),
            "1V2GR2CA6JC554551".to_string(),
            "1G4PS5SK2H4101481".to_string(),
            "JM1BM1K79G1285833".to_string(),
            "5NMS33AD1KH037595".to_string(),
            "5TDYK3DC2GS764684".to_string(),
            "JA4AZ3A36JZ056343".to_string(),
            "4YDT28925J5352132".to_string(),
            "19UUB2F67JA001663".to_string(),
            "3GNCJPSBXKL144203".to_string(),
            "JF2SJAEC5JH418265".to_string(),
            "ML32F3FJ3HHF18566".to_string(),
            "3GYFNBE34GS520712".to_string(),
            "4T1BK1EB4GU211235".to_string(),
            "2T3JFREV0GW508907".to_string(),
            "3GCPCNEC6JG573889".to_string(),
            "ZFBERFAB9H6G61152".to_string(),
            "4T1B61HK9KU853183".to_string(),
            "JM3KE2BY8G0814570".to_string(),
            "1N4AL3AP0JC244231".to_string(),
            "1FMCU0JD3HUE29104".to_string(),
            "1N4AA6AP4HC403902".to_string(),
            "1FMCU9JD8HUE61728".to_string(),
            "1HGCR2F5XHA016795".to_string(),
            "2T3C1RFV2KW021108".to_string(),
            "WAUDNAF43HN044931".to_string(),
            "5N1AZ2MH7GN154560".to_string(),
            "1FTMF1CF2GFD11975".to_string(),
            "3VW5T7AU5HM001142".to_string(),
            "5TFBW5F15GX521981".to_string(),
            "3FA6P0T90GR203423".to_string(),
            "5J6RM4H30GL132924".to_string(),
            "5TDJGRFH0JS047920".to_string(),
            "1N4AA6AP1GC414614".to_string(),
            "5FNRL6H79JB060221".to_string(),
            "1N4BL4BV6KC169365".to_string(),
            "1C4PJMCB7HW637041".to_string(),
            "4T1BF1FK8GU221737".to_string(),
            "SAJBN4EV0JCY53845".to_string(),
            "ZACCJABT9GPE31741".to_string(),
            "ZFBHRFBB7K6M28967".to_string(),
            "1GCVKREC1HZ177252".to_string(),
            "2LMPJ6KR1HBL18245".to_string(),
            "2C4RDGCG2KR520464".to_string(),
            "5N1DL0MN9HC510688".to_string(),
            "JM3KFBDM8J0403183".to_string(),
            "SALYL2EX0KA220773".to_string(),
            "1FTEW1CP7GFB41321".to_string(),
            "5NPE34AF4HH463054".to_string(),
            "1FA6P8TH6H5322118".to_string(),
            "1FTEW1EPXKFB43084".to_string(),
            "1FDWS9PM8HKB26765".to_string(),
            "2FMPK4K97HBC05025".to_string(),
            "3N1CE2CP1HL378526".to_string(),
            "KM8K22AA7KU390978".to_string(),
            "3FA6P0H70GR305733".to_string(),
            "1HGCV1F37JA055306".to_string(),
            "1G1BE5SMXJ7175770".to_string(),
            "2G61M5S39J9120354".to_string(),
            "1HGCV1F10JA223351".to_string(),
            "1G1BC5SM2J7173243".to_string(),
            "WAUM2AFR2GA008593".to_string(),
            "1V2CR2CA0JC587057".to_string(),
            "1FM5K8D83HGC50670".to_string(),
            "3N1AB7AP3KY283637".to_string(),
            "1XPCD49X4JD461798".to_string(),
            "2G1105S30K9138468".to_string(),
            "1C4RDHAG5JC226542".to_string(),
            "1G1BE5SM2G7300429".to_string(),
            "2GNALCEK5H6234586".to_string(),
            "5ZT3VG4F1K6502584".to_string(),
            "1G1ZD5ST9KF135678".to_string(),
            "5XYPG4A34GG153267".to_string(),
            "2HKRW2H83KH634632".to_string(),
            "1N4AL3AP4GC234777".to_string(),
            "1N4AL3AP8GC222955".to_string(),
            "5NPD74LF4JH351311".to_string(),
            "2HGFC2F52JH573433".to_string(),
            "JF1GPAB66GH209715".to_string(),
            "3KPF24AD6KE027503".to_string(),
            "1C4HJXEG0KW630373".to_string(),
            "KNDPNCAC3H7254945".to_string(),
            "5FNYF6H0XHB093809".to_string(),
            "1HGCV1F50JA048490".to_string(),
            "1C3CCCAB7GN143762".to_string(),
            "5XYPGDA31GG150987".to_string(),
            "5XXGT4L12GG104709".to_string(),
            "KMHCT4AE7HU298259".to_string(),
            "3C7WRTCL1JG331709".to_string(),
            "19XFC1F99GE038633".to_string(),
            "1FMCU9J99HUE04806".to_string(),
            "1C4BJWEG2GL163823".to_string(),
            "1FMCU9J96HUA74400".to_string(),
            "2C4RDGCG5JR205091".to_string(),
            "2G4GL5EX9H9192900".to_string(),
            "1FMCU9J90GUC64000".to_string(),
            "1FTEX1CFXHFC60356".to_string(),
            "2T1BURHE4HC950462".to_string(),
            "1FTYR2ZM6KKB24274".to_string(),
            "JTMN1RFV9KD518324".to_string(),
            "3N1CN7AP0HL814640".to_string(),
            "1C4RJFAGXGC477952".to_string(),
            "1GAWGFFG9K1220529".to_string(),
            "4T1BZ1FB8KU023045".to_string(),
            "3FA6P0H9XHR198693".to_string(),
            "WA1LAAF78HD050969".to_string(),
            "SALWG2RV0JA699571".to_string(),
            "3N1CE2CP9GL376537".to_string(),
            "1C4NJCBA9HD187088".to_string(),
            "1FMCU9JX3GUB31584".to_string(),
            "1C4RJFAG7JC384247".to_string(),
            "1GC1KUEG7JF146695".to_string(),
            "1FTEW1E5XJFC69152".to_string(),
            "3GTU2PEC6GG313986".to_string(),
            "YV4612HKXG1006284".to_string(),
            "3N1AB7AP6JY271139".to_string(),
            "1GTV2MEC5GZ151531".to_string(),
            "WA1BNAFY9J2196145".to_string(),
            "4T1BF1FK1GU250240".to_string(),
            "1G1ZB5ST4HF193423".to_string(),
            "4S4WMAAD4K3414756".to_string(),
            "3VWC17AUXGM507559".to_string(),
            "1GCUYBEF0KZ232177".to_string(),
            "KM8J3CA26JU724486".to_string(),
            "JM3KFADM5K0623635".to_string(),
            "3N1AB7AP8GY239706".to_string(),
            "58ABK1GG4HU060559".to_string(),
            "KNDPM3AC8J7337862".to_string(),
            "5NPE24AF5HH589748".to_string(),
            "KMHDH4AE6GU584706".to_string(),
            "1FTER4FH0KLA69398".to_string(),
            "3N1AB7AP1HL676025".to_string(),
            "3VW5DAAT8JM505370".to_string(),
            "5TFCZ5AN1HX080499".to_string(),
            "JTHBZ1BL4GA000906".to_string(),
            "3CZRU5H32KG712364".to_string(),
            "1VWAT7A32GC053303".to_string(),
            "1HGCR2F5XGA094332".to_string(),
            "5TDKZRFH0JS528846".to_string(),
            "2T2BZMCAXKC196527".to_string(),
            "1GNERGKW9KJ250542".to_string(),
            "2LMTJ8LR7GBL57719".to_string(),
            "19XFC2F73HE017685".to_string(),
            "1C4RJFBG0KC724497".to_string(),
            "5NMZU3LB3HH031426".to_string(),
            "2C3CDXCT2JH252902".to_string(),
            "WDDWK4KBXHF501020".to_string(),
            "JTJBARBZ5H2120735".to_string(),
            "JN8AZ2NF1K9680108".to_string(),
            "ZACNJABB4KPJ74932".to_string(),
            "1HGCV1F41KA070040".to_string(),
            "3LN6L2LU6GR632096".to_string(),
            "1C4NJDEB0HD205200".to_string(),
            "5FNYF6H51GB053385".to_string(),
            "1FTFW1EG1HFB74401".to_string(),
            "1N4AL3AP9JC154804".to_string(),
            "KM8J33A25JU809271".to_string(),
            "KM8J33A20HU283593".to_string(),
            "JTMK1RFV7KJ003097".to_string(),
            "5XXGU4L37GG072452".to_string(),
            "3MZBN1V74HM112845".to_string(),
            "1N6DD0EV3HN771915".to_string(),
            "JM3KFBDL3H0183646".to_string(),
            "ZACNJBBB4KPK72039".to_string(),
            "2C3CCAGG2GH152451".to_string(),
            "2C4RC1BG7GR230214".to_string(),
            "5NMZUDLB9JH095630".to_string(),
            "1N4AL3AP9JC190878".to_string(),
            "5TFDW5F14HX592518".to_string(),
            "WBXHT3C54K5L91004".to_string(),
            "1HGCR2F80HA049962".to_string(),
            "5YFBURHE4JP795885".to_string(),
            "5FNYF6H52HB033874".to_string(),
            "W04GV8SX8K1013322".to_string(),
            "3VWC57BU4KM138063".to_string(),
            "5NPD84LF9JH392174".to_string(),
            "JN1BJ1CR4KW332385".to_string(),
            "5FRYD4H47GB028202".to_string(),
            "JN1EV7AR8KM552275".to_string(),
            "JTJYARBZ5K2154370".to_string(),
            "5GAKVBKD2GJ329990".to_string(),
            "3C63RRGL4KG533711".to_string(),
            "3N1CN7AP8KL830690".to_string(),
            "3FA6P0G70KR130281".to_string(),
            "2T1BPRHE4GC549290".to_string(),
            "3GNKBGRS8KS703333".to_string(),
            "4S4WMALDXK3466745".to_string(),
            "3MYDLBYV2JY300944".to_string(),
            "4T1BF1FK9GU173732".to_string(),
            "KNAFK4A69G5441957".to_string(),
            "1GCVKREC0GZ394922".to_string(),
            "KNADM4A36G6603920".to_string(),
            "2C4RC1BG2KR650181".to_string(),
            "1N4AL3AP2JC121286".to_string(),
            "1FTFX1E5XKKE74805".to_string(),
            "5YJSA1E22JF276687".to_string(),
            "KL7CJLSB8GB760599".to_string(),
            "3GCUKRECXGG123782".to_string(),
            "1GYKNDRS5HZ129908".to_string(),
            "JTDKARFUXJ3054460".to_string(),
            "2HGFC2F52JH576784".to_string(),
            "2GNALCEK0G6342953".to_string(),
            "1G1JD5SH4H4115724".to_string(),
            "1GKS1CKJ4HR330958".to_string(),
            "2GNALDEK3H1558526".to_string(),
            "JHMGK5H71GX025321".to_string(),
            "KNDJN2A22H7878436".to_string(),
            "5XXGT4L32KG293582".to_string(),
            "1FMCU0G96HUC21995".to_string(),
            "5XYPG4A33HG267214".to_string(),
            "1HGCV1F36KA153308".to_string(),
            "3N1CN7AP1JL859916".to_string(),
            "3FA6P0LU9JR221169".to_string(),
            "5TDJKRFH4GS329999".to_string(),
            "3GNAXJEVXJL307925".to_string(),
            "5N1AT2MT0JC810199".to_string(),
            "2HGFC2F56JH595905".to_string(),
            "JA4AD3A35KZ018733".to_string(),
            "5N1AZ2MH1JN127765".to_string(),
            "19XFC2F53GE041465".to_string(),
            "JM1GL1UM6J1335249".to_string(),
            "5XXGU4L39GG095019".to_string(),
            "2T3WFREV4GW276494".to_string(),
            "1FBZX2CM1KKA98464".to_string(),
            "2C4RDGEGXHR779516".to_string(),
            "5XYPK4A57GG101860".to_string(),
            "1N6AD0EV6GN791321".to_string(),
            "2FMPK4AP9KBC27440".to_string(),
            "5FNYF5H98HB032189".to_string(),
            "5NMS23AD7KH048023".to_string(),
            "1N6AD0EV4JN729410".to_string(),
            "19XFC2F81KE020455".to_string(),
            "1GTG6EEN7K1200837".to_string(),
            "1GCVKRECXGZ243277".to_string(),
            "3G1BE6SM1KS630966".to_string(),
            "2C4RDGCG8HR671561".to_string(),
            "KM8J2CA48KU886383".to_string(),
            "1FMCU9J98HUB51090".to_string(),
            "19XFC2F70HE001380".to_string(),
            "5N1AT2MT2HC755488".to_string(),
            "4T1BF1FK6HU436003".to_string(),
            "2T3YFREV1HW362100".to_string(),
            "JTNB11HK2J3039296".to_string(),
            "1C4RJFBG2KC667039".to_string(),
            "WBA4J1C51JBG76653".to_string(),
            "KL4CJASB8KB704463".to_string(),
            "3FADP4BJXJM126011".to_string(),
            "2HGFC2F54GH553435".to_string(),
            "19XFC2E51GE034175".to_string(),
            "1GNSCHKC5KR187506".to_string(),
            "JTHBA1D21G5024181".to_string(),
            "JTEBU5JR8G5337564".to_string(),
            "1C4RDJDG3KC560914".to_string(),
            "58TBH0BT1K3UM3120".to_string(),
            "1FAHP2E89GG141732".to_string(),
            "KNDJN2A23G7278961".to_string(),
            "KNDJP3A56G7287583".to_string(),
            "2T2BZMCA4KC201401".to_string(),
            "KL4CJASBXKB941570".to_string(),
            "2T2ZZMCA2HC051988".to_string(),
            "1C4RJFBG6HC668624".to_string(),
            "1FMCU0GD6HUE90602".to_string(),
            "7FARW2H50JE073510".to_string(),
            "1GTV2NEC5HZ130752".to_string(),
            "5FNRL5H67GB065752".to_string(),
            "1FM5K7D88HGD42268".to_string(),
            "WBXHT3C31G5E51698".to_string(),
            "LRBFXFSXXHD041080".to_string(),
            "5YJ3E1EB3JF096594".to_string(),
            "5NPD84LF1JH230751".to_string(),
            "1FAHP2E81JG124723".to_string(),
            "1HGCR2F89HA155794".to_string(),
            "3KPF24AD6KE130405".to_string(),
            "SALGS3EF1GA295789".to_string(),
            "1C4RJFBG6HC764463".to_string(),
            "5NPE34AF5KH783216".to_string(),
            "1FM5K8D84HGB02768".to_string(),
            "KL7CJLSB2JB572636".to_string(),
            "55SWF8EB0KU305633".to_string(),
            "5XXGT4L34KG295267".to_string(),
            "1N4AL3AP5GC286936".to_string(),
            "5FNRL6H70JB022604".to_string(),
        ];

        let start = std::time::Instant::now();
        decoder.decode_batch(&vins);
        let elapsed = start.elapsed();
        println!("{}", elapsed.as_millis())
    }
}
