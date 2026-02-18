use std::{collections::HashMap, sync::Arc};

pub use crate::decoder::extractors::{BodyStyle, VehicleInfo};
use crate::{
    CorgiError, VIN,
    db::Db,
    decoder::{
        extractors::{
            ModelYearErrorCode, WmiErrorCode, extract_model_year, extract_vds_vis,
            extract_vehicle_info, extract_wmi,
        },
        validators::{
            CheckDigitErrorCode, StructureErrorCode, validate_check_digit, validate_vin_structure,
        },
    },
    pattern::{PatternDescriptor, PatternMatcher},
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
    db: Arc<Db>,
}

impl VinDecoder {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            pattern_matcher: PatternMatcher::new(Arc::clone(&db)),
            db,
        }
    }

    pub async fn new_with_default_db() -> Self {
        let db = Db::new().await;
        Self::new(Arc::new(db))
    }

    pub async fn decode(&self, vin: String) -> Result<VehicleInfo, CorgiError> {
        validate_vin_structure(&vin)?;
        validate_check_digit(&vin)?;

        let model_year = extract_model_year(&vin)?;
        let wmi = extract_wmi(&vin)?;
        let wmi_info = self.db.get_wmi_infos(&[&wmi]).await?.remove(0);
        let (vds, vis) = extract_vds_vis(&vin)?;

        let pattern_descriptor = PatternDescriptor {
            wmi: wmi.to_string(),
            model_year: model_year as i32,
            vds: vds.to_string(),
            vis: vis.to_string(),
        };

        let patterns = self
            .pattern_matcher
            .matches(vec![pattern_descriptor.clone()])
            .await?
            .remove(&pattern_descriptor)
            .ok_or(CorgiError::VinDecoder(
                vin,
                VinDecoderError::Unexpected {
                    message: "Could not find pattern descriptor in patterns map".to_string(),
                },
            ))?;

        let vehicle_info = extract_vehicle_info(wmi_info, model_year as i32, patterns);
        Ok(vehicle_info)
    }

    pub async fn batch_decode(
        &self,
        vins: Vec<VIN>,
    ) -> HashMap<VIN, Result<VehicleInfo, CorgiError>> {
        let (pattern_descriptors, invalid_structures) = vins
            .into_iter()
            .map(|vin| {
                fn make_pattern_descriptor(vin: &String) -> Result<PatternDescriptor, CorgiError> {
                    validate_vin_structure(&vin)?;
                    validate_check_digit(&vin)?;

                    let model_year = extract_model_year(&vin)?;
                    let wmi = extract_wmi(&vin)?;
                    let (vds, vis) = extract_vds_vis(&vin)?;

                    let pattern_descriptor = PatternDescriptor {
                        wmi: wmi.to_string(),
                        model_year: model_year as i32,
                        vds: vds.to_string(),
                        vis: vis.to_string(),
                    };
                    Ok(pattern_descriptor)
                }

                let pattern_descriptor = make_pattern_descriptor(&vin);
                (vin, pattern_descriptor)
            })
            .fold(
                (HashMap::new(), HashMap::new()),
                |(mut oks, mut errs), (vin, r)| {
                    match r {
                        Ok(v) => {
                            oks.insert(vin, v);
                        }
                        Err(e) => {
                            errs.insert(vin, e);
                        }
                    }
                    (oks, errs)
                },
            );

        let wmis = pattern_descriptors
            .values()
            .map(|v| v.wmi.as_str())
            .collect::<Vec<_>>();
        let wmi_infos = match self.db.get_wmi_infos(&wmis).await {
            Ok(infos) => infos,
            Err(err) => {
                let shared_err = Arc::new(err);

                let mut wmi_errs = pattern_descriptors
                    .into_keys()
                    .map(|vin| (vin, Err(CorgiError::Shared(Arc::clone(&shared_err)))))
                    .collect::<HashMap<_, _>>();

                wmi_errs.extend(
                    invalid_structures
                        .into_iter()
                        .map(|(vin, err)| (vin, Err(err))),
                );
                return wmi_errs;
            }
        };

        let mut patterns = match self
            .pattern_matcher
            .matches(pattern_descriptors.values().cloned().collect())
            .await
        {
            Ok(patterns) => patterns,
            Err(err) => {
                let shared_err = Arc::new(err);

                let mut pattern_errs = pattern_descriptors
                    .into_keys()
                    .map(|vin| (vin, Err(CorgiError::Shared(Arc::clone(&shared_err)))))
                    .collect::<HashMap<_, _>>();

                pattern_errs.extend(
                    invalid_structures
                        .into_iter()
                        .map(|(vin, err)| (vin, Err(err))),
                );
                return pattern_errs;
            }
        };

        let mut vehicle_infos = pattern_descriptors
            .into_iter()
            .map(|(vin, desc)| {
                if let Some(pattern) = patterns.remove(&desc)
                    && let Some(wmi_info) =
                        wmi_infos.iter().find(|wmi| wmi.code == desc.wmi).cloned()
                {
                    let vehicle_info = extract_vehicle_info(wmi_info, desc.model_year, pattern);
                    return (vin, Ok(vehicle_info));
                }

                (
                    vin.clone(),
                    Err(CorgiError::VinDecoder(
                        vin,
                        VinDecoderError::Unexpected { message: "Could not find pattern descriptor in patterns map or wmi code in wmi infos".to_string() },
                    )),
                )
            })
            .collect::<HashMap<_, _>>();

        vehicle_infos.extend(
            invalid_structures
                .into_iter()
                .map(|(vin, err)| (vin, Err(err))),
        );
        vehicle_infos
    }
}

pub mod extractors {
    use std::{borrow::Cow, collections::HashMap, sync::LazyLock};

    use chrono::Datelike;

    use crate::{CorgiError, decoder::VinDecoderError, pattern::PatternMatch, types::WMIInfo};

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
        wmi_info: WMIInfo,
        model_year: i32,
        patterns: Vec<PatternMatch>,
    ) -> VehicleInfo {
        let mut vehicle_info = VehicleInfo {
            make: wmi_info.make,
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
            manufacturer: Some(wmi_info.manufacturer),
        };

        //
        // Keep track of both fuel types to determine if vehicle is hybrid
        //
        let mut primary_fuel_type = None;
        let mut secondary_fuel_type = None;
        patterns
            .into_iter()
            .for_each(|pattern| match pattern.element.as_str() {
                "Make" => vehicle_info.make = pattern.resolved,
                "Model" => vehicle_info.model = Some(pattern.resolved),
                "Series" => vehicle_info.series = Some(pattern.resolved),
                "Trim" | "Trim Level" => vehicle_info.trim = Some(pattern.resolved),
                "Body Class" | "Body Style" => {
                    vehicle_info.body_style = Some(extract_body_style(&pattern.resolved))
                }
                "Drive Type" => vehicle_info.drive_type = Some(pattern.resolved),
                "Fuel Type - Primary" => primary_fuel_type = Some(pattern.resolved),
                "Fuel Type - Secondary" => secondary_fuel_type = Some(pattern.resolved),
                "Transmission" => vehicle_info.transmission = Some(pattern.resolved),
                "Doors" => vehicle_info.doors = pattern.resolved.parse::<i32>().ok(),
                "Gross Vehicle Weight Rating From" => vehicle_info.gvwr = Some(pattern.resolved),
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

    #[tokio::test]
    async fn test_decoder_simple() -> Result<(), CorgiError> {
        let decoder = VinDecoder::new_with_default_db().await;
        let info = decoder.decode("2FTEF14H8TCA73155".to_string()).await?;
        assert_eq!(info.make, "Ford".to_string());
        assert_eq!(info.model, Some("F-150".to_string()));
        assert_eq!(info.year, 1996);

        Ok(())
    }

    #[tokio::test]
    async fn test_decoder_simple_batch() {
        let decoder = VinDecoder::new_with_default_db().await;
        let vins = vec![
            "KM8K2CAB4PU001140".to_string(),
            "5N1AT2MT9LC784186".to_string(),
            "2FTEF14H8TCA73155".to_string(),
            "1FTFW1ET6DFA4553".to_string(),
        ];

        let start = std::time::Instant::now();
        let _vi = decoder.batch_decode(vins).await;
        let elapsed = start.elapsed();
        println!("{}", elapsed.as_millis())
    }

    #[tokio::test]
    async fn test_decoder_many_batch() {
        let decoder = VinDecoder::new_with_default_db().await;
        let vins = vec![
            "JF2SH6BC0AH793767".to_string(),
            "3MZBN1U7XHM117646".to_string(),
            "1N4AL3AP9JC167911".to_string(),
            "2GKFLREK1C6274551".to_string(),
            "1N4BL4DV3SN327727".to_string(),
            "1D7KS28C06J225332".to_string(),
            "1GTG6CE3XF1188367".to_string(),
            "4S4BSAFC6J3260988".to_string(),
            "3VW167AJXHM200537".to_string(),
            "1N4AA6AP3GC395015".to_string(),
            "3GNAXKEV6NL308789".to_string(),
            "TRUC1AFV0J1007485".to_string(),
            "1B3CC4FB2AN196470".to_string(),
            "1FTEW1EG8GFB86393".to_string(),
            "2T1BURHE1JC988530".to_string(),
            "1FMCU9GD1JUC10420".to_string(),
            "JN8BT3BB1PW210974".to_string(),
            "2GNALAEK1E6353543".to_string(),
            "2HKRW2H27KH152967".to_string(),
            "1C6RR7GT1ES116185".to_string(),
            "KMHTC6ADXCU023903".to_string(),
            "JN8AF5MR0BT021235".to_string(),
            "1B3CC5FV3AN226947".to_string(),
            "4T1BB46K47U028658".to_string(),
            "1FMCU9GXXGUA46720".to_string(),
            "1FMCU9HD7JUB88759".to_string(),
            "2C3CDXMG6PH537452".to_string(),
            "1GKS2KKL8RR203124".to_string(),
            "2GNALCEK4G1159830".to_string(),
            "5FNYF8H64NB029552".to_string(),
            "1G1ZC5EB1AF258296".to_string(),
            "1G2ZG57B794208829".to_string(),
            "2T1BURHE6KC171555".to_string(),
            "4S4BSANC6K3366991".to_string(),
            "2FABP7BV0BX171783".to_string(),
            "1FTZR15E67PA73303".to_string(),
            "WMWSY3C53CT144300".to_string(),
            "JF2GTAEC1KH399666".to_string(),
            "3KPA24AD2ME422520".to_string(),
            "1N6AD0FV9BC419605".to_string(),
            "WVGBV7AX4GW617614".to_string(),
            "2HGFG12847H501740".to_string(),
            "VNKKTUD31FA052307".to_string(),
            "JTMGB3FV6RD171516".to_string(),
            "1FUJCYCY3FHGF8070".to_string(),
            "5TDGSKFCXMS019048".to_string(),
            "1FTFW1ET6EFA96898".to_string(),
            "3VW2K7AJ8EM354921".to_string(),
            "1GNES13H672127302".to_string(),
            "5NMSH73E07H116164".to_string(),
        ];
        let vins_repeated = (0..10).flat_map(|_| vins.iter().cloned()).collect();

        let start = std::time::Instant::now();
        let vi = decoder.batch_decode(vins_repeated).await;
        let elapsed = start.elapsed();
        println!("{vi:#?}");
        println!("{}", elapsed.as_millis())
    }
}
