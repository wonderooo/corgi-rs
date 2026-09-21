//! The VIN decoding pipeline.
//!
//! This follows NHTSA's `spVinDecode` stored procedure step for step, because
//! matching its output is the whole point of decoding against vPIC data:
//!
//! 1. resolve the WMI, and with it the vehicle type and manufacturer;
//! 2. read the model year, which selects *which* VIN schemas apply;
//! 3. match every pattern of every applicable schema against the VIN;
//! 4. rank the matches per element and keep one winner each;
//! 5. derive the make from the winning model, since a WMI alone rarely names it;
//! 6. fill the gaps from the engine-model, vehicle-spec and default tables.
//!
//! Step 2 and step 5 are what this crate previously got wrong. Without the
//! model-year window every schema a WMI ever used competes at once, and without
//! deriving the make from the model, shared WMIs like `1C4` (Jeep, Ram, Dodge,
//! Chrysler and Fiat) all collapse onto whichever make happened to be listed
//! first.

use std::collections::HashMap;

#[cfg(feature = "parallel")]
use crate::RAYON_CHUNK_SIZE;
#[cfg(feature = "parallel")]
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use chrono::Datelike;

use crate::{
    CorgiError, DefaultValueRow, EngineModelRow, GenerationRow, ModelEntry, PatternRow, SchemaRef,
    SpecRow, VIN, WmiEntry,
    element::{self, Element},
    maps::{FstRkyvMap, MapError},
    pattern::{self, keys_match, literal_len, match_key},
    vehicle::{Generation, VehicleInfo, Warning},
    vin::{self, StructureErrorCode},
};

pub use crate::vehicle::extract_body_style;

/// Reasons the model year could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelYearErrorCode {
    CharIndexOutOfBounds,
    /// Position 10 holds a character that is not a model-year code, which some
    /// markets outside the US use to mean "not encoded".
    UnencodedModelYear,
}

/// Reasons the WMI could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WmiErrorCode {
    InvalidVinLength,
    /// The WMI is well-formed but not registered with NHTSA for a car, truck or
    /// MPV. Motorcycles, trailers and equipment are deliberately not shipped.
    UnknownWmi,
}

/// Errors emitted during VIN validation or decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VinDecoderError {
    /// The VIN structure did not meet the CFR requirements (length or character set).
    InvalidStructure {
        message: String,
        code: StructureErrorCode,
    },
    /// Check digit validation failed. Only produced in strict mode; see
    /// [`VinDecoder::require_check_digit`].
    InvalidCheckDigit { message: String, expected: char },
    /// Unable to read the model-year codified character.
    UnreadableModelYear {
        message: String,
        code: ModelYearErrorCode,
    },
    /// Unable to resolve the WMI segment.
    UnreadableWmi { message: String, code: WmiErrorCode },
    /// Any other unexpected failure during decoding.
    Unexpected { message: String },
}

impl std::fmt::Display for VinDecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidStructure { message, .. }
            | Self::InvalidCheckDigit { message, .. }
            | Self::UnreadableModelYear { message, .. }
            | Self::UnreadableWmi { message, .. }
            | Self::Unexpected { message } => message,
        };
        f.write_str(message)
    }
}

impl std::error::Error for VinDecoderError {}

/// One candidate value for an element, before ranking.
///
/// The ordering fields reproduce NHTSA's `RANK() OVER (PARTITION BY ElementId
/// ORDER BY Priority DESC, CreatedOn DESC, LENGTH(REPLACE(Keys, '*', '')), Id)`.
#[derive(Debug, Clone)]
struct Candidate {
    element_id: u16,
    attribute_id: String,
    value: String,
    /// For pattern matches this is the schema's `YearFrom`, so a schema written
    /// for a later model year outranks an older one that also happens to match.
    priority: i32,
    updated_on: i64,
    literal_len: usize,
    tie_break: u32,
}

impl Candidate {
    /// Ranking key. Higher sorts first for the first two components, lower for
    /// the last two, so this is compared as `(priority, updated_on, Reverse(...))`.
    fn outranks(&self, other: &Candidate) -> bool {
        (self.priority, self.updated_on) > (other.priority, other.updated_on)
            || ((self.priority, self.updated_on) == (other.priority, other.updated_on)
                && (self.literal_len, self.tie_break) < (other.literal_len, other.tie_break))
    }
}

/// Displacement conversions vPIC applies when only one unit was decoded, in the
/// order NHTSA's `Conversion` table lists them.
const DISPLACEMENT_CONVERSIONS: [(u16, u16, f64); 6] = [
    (
        element::DISPLACEMENT_CC,
        element::DISPLACEMENT_CI,
        1.0 / 16.387064,
    ),
    (
        element::DISPLACEMENT_CI,
        element::DISPLACEMENT_CC,
        16.387064,
    ),
    (
        element::DISPLACEMENT_CC,
        element::DISPLACEMENT_L,
        1.0 / 1000.0,
    ),
    (element::DISPLACEMENT_L, element::DISPLACEMENT_CC, 1000.0),
    (
        element::DISPLACEMENT_CI,
        element::DISPLACEMENT_L,
        0.016387064,
    ),
    (
        element::DISPLACEMENT_L,
        element::DISPLACEMENT_CI,
        1.0 / 0.016387064,
    ),
];

/// Everything a candidate model year matched, and how strong that evidence is.
///
/// Carrying the candidates rather than just a score means the year can be
/// settled and the vehicle decoded from a single pass over the patterns.
#[derive(Debug, Default)]
struct YearMatch {
    candidates: Vec<Candidate>,
    /// Whether any schema for this year covered the VIN at all.
    had_schemas: bool,
    /// Whether one of the matches was a model, which is the strongest signal a
    /// schema really belongs to this vehicle.
    has_model: bool,
}

impl YearMatch {
    /// Whether this year is supported by the data at all.
    fn is_supported(&self) -> bool {
        !self.candidates.is_empty()
    }

    /// A resolved model outweighs any number of incidental matches.
    fn rank(&self) -> usize {
        self.candidates.len() + if self.has_model { 10_000 } else { 0 }
    }
}

/// Priority NHTSA gives to values that come from an engine-model expansion
/// rather than from the VIN itself. Below any schema `YearFrom`, so a pattern
/// always wins.
const ENGINE_MODEL_PRIORITY: i32 = 50;
/// Priority for keys that read digits straight out of the VIN.
const FORMULA_PRIORITY: i32 = 100;
/// Priority for values taken from the vehicle-spec tables.
const VEHICLE_SPEC_PRIORITY: i32 = -100;
/// Priority for per-vehicle-type defaults.
const DEFAULT_PRIORITY: i32 = -200;

/// VIN decoder backed by the archived vPIC tables.
///
/// Construction memory-maps roughly 55 MB of lookup tables, so build one and
/// share it. Decoding itself allocates only the result.
pub struct VinDecoder {
    wmis: FstRkyvMap<WmiEntry>,
    schemas: FstRkyvMap<SchemaRef>,
    patterns: FstRkyvMap<PatternRow>,
    models: FstRkyvMap<ModelEntry>,
    specs: FstRkyvMap<SpecRow>,
    engine_models: FstRkyvMap<EngineModelRow>,
    defaults: FstRkyvMap<DefaultValueRow>,
    generations: FstRkyvMap<GenerationRow>,
    now_year: i32,
    strict_check_digit: bool,
}

impl VinDecoder {
    /// Construct a decoder, memory-mapping the lookup tables.
    ///
    /// # Panics
    ///
    /// Panics if the tables are missing. Use [`VinDecoder::try_new`] to handle
    /// that.
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|err| panic!("{err}"))
    }

    /// Construct a decoder, reporting missing lookup tables instead of panicking.
    pub fn try_new() -> Result<Self, MapError> {
        Ok(Self {
            wmis: FstRkyvMap::open()?,
            schemas: FstRkyvMap::open()?,
            patterns: FstRkyvMap::open()?,
            models: FstRkyvMap::open()?,
            specs: FstRkyvMap::open()?,
            engine_models: FstRkyvMap::open()?,
            defaults: FstRkyvMap::open()?,
            generations: FstRkyvMap::open()?,
            now_year: chrono::Utc::now().year(),
            strict_check_digit: false,
        })
    }

    /// Reject VINs whose check digit does not match, instead of decoding them
    /// and reporting [`Warning::CheckDigitMismatch`].
    ///
    /// Off by default: salvage and grey-import VINs frequently carry a wrong
    /// check digit yet decode correctly, and NHTSA itself decodes them.
    pub fn require_check_digit(mut self, strict: bool) -> Self {
        self.strict_check_digit = strict;
        self
    }

    /// Pin the "current year" used to disambiguate the 30-year model-year cycle.
    ///
    /// Only useful for tests and for reproducing a historical decode.
    pub fn with_current_year(mut self, year: i32) -> Self {
        self.now_year = year;
        self
    }

    /// Decode a single VIN.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use corgi_rs::VinDecoder;
    ///
    /// let decoder = VinDecoder::new();
    /// let info = decoder.decode("1C6RR7LT2JS179571").expect("valid VIN");
    /// assert_eq!(info.make, "Ram");
    /// ```
    pub fn decode(&self, vin: &str) -> Result<VehicleInfo, CorgiError> {
        let vin = normalize(vin);

        if let Err((code, invalid)) = vin::validate_structure(&vin) {
            let message = match code {
                StructureErrorCode::InvalidLength => {
                    format!(
                        "VIN must be 17 characters, got {}: {vin}",
                        vin.chars().count()
                    )
                }
                StructureErrorCode::InvalidCharacters => format!(
                    "invalid characters in VIN {vin}: {}",
                    invalid
                        .iter()
                        .map(|c| format!("`{}` at position {}", c.character, c.position))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            return Err(CorgiError::VinDecoder(
                vin,
                VinDecoderError::InvalidStructure { message, code },
            ));
        }

        let wmi = vin::extract_wmi(&vin);
        let wmi_entry = self
            .wmis
            .get(&wmi)
            .and_then(|mut entries| (!entries.is_empty()).then(|| entries.swap_remove(0)))
            .ok_or_else(|| {
                CorgiError::VinDecoder(
                    vin.clone(),
                    VinDecoderError::UnreadableWmi {
                        message: format!(
                            "WMI `{wmi}` of VIN {vin} is not a registered car, truck or MPV manufacturer"
                        ),
                        code: WmiErrorCode::UnknownWmi,
                    },
                )
            })?;

        let is_car_mpv_lt =
            element::is_car_mpv_lt(wmi_entry.vehicle_type_id, wmi_entry.truck_type_id);

        let model_year = vin::model_year(&vin, is_car_mpv_lt, self.now_year).ok_or_else(|| {
            CorgiError::VinDecoder(
                vin.clone(),
                VinDecoderError::UnreadableModelYear {
                    message: format!(
                        "position 10 of VIN {vin} does not encode a model year: `{}`",
                        vin.chars().nth(9).unwrap_or('?')
                    ),
                    code: ModelYearErrorCode::UnencodedModelYear,
                },
            )
        })?;

        let mut warnings = Vec::new();

        if !vin::check_digit_valid(&vin, is_car_mpv_lt) {
            if self.strict_check_digit {
                let expected = vin::check_digit(&vin, is_car_mpv_lt).unwrap_or('?');
                return Err(CorgiError::VinDecoder(
                    vin.clone(),
                    VinDecoderError::InvalidCheckDigit {
                        message: format!(
                            "check digit of VIN {vin} should be `{expected}`, not `{}`",
                            vin.chars().nth(8).unwrap_or('?')
                        ),
                        expected,
                    },
                ));
            }
            warnings.push(Warning::CheckDigitMismatch);
        }

        if !model_year.conclusive {
            warnings.push(Warning::ModelYearAmbiguous);
        }

        let (year, conclusive, matches) = self.settle_model_year(&vin, model_year);
        if conclusive && !model_year.conclusive {
            warnings.retain(|warning| *warning != Warning::ModelYearAmbiguous);
        }

        Ok(self.assemble(&wmi_entry, year, conclusive, matches, warnings))
    }

    /// Choose between the two 30-year blocks position 10 could mean, and
    /// return the pattern matches for the winner.
    ///
    /// Position 10 repeats every 30 years, so `L` is both 1990 and 2020. NHTSA
    /// disambiguates cars, MPVs and light trucks by position 7, which is
    /// supposed to hold a letter from model year 2010 on; for everything else
    /// its fallback is to reject a year more than two ahead of today.
    ///
    /// Both rules misfire on real VINs. A 1997 heavy truck reads as 2027
    /// because the fallback never triggers, and Volvo and Mercedes put digits
    /// in position 7 on cars built well after 2010, which reads a 2020 XC90 as
    /// a 1990 one.
    ///
    /// The schema tables settle it: a WMI has VIN schemas only for the years it
    /// actually built vehicles in, and a schema only resolves a model if the
    /// VIN's descriptor really belongs to it. The second year is only matched
    /// when the first leaves genuine doubt, so the common case still costs one
    /// pass over the patterns.
    fn settle_model_year(&self, vin: &str, model_year: vin::ModelYear) -> (i32, bool, YearMatch) {
        let chosen = self.match_year(vin, model_year.year);

        // A conclusive year that resolved a model needs no second opinion.
        if model_year.conclusive && chosen.has_model {
            return (model_year.year, true, chosen);
        }

        // Position 10 decodes to a base year in 2010..=2039; NHTSA's rollback
        // maps that onto 1980..=2009. So the other candidate lies whichever way
        // the current answer is not.
        let alternative = if model_year.year < 2010 {
            model_year.year + 30
        } else {
            model_year.year - 30
        };
        if alternative < 1980 {
            return (model_year.year, model_year.conclusive, chosen);
        }

        let other = self.match_year(vin, alternative);

        if model_year.conclusive {
            // Overrule position 7 when the other block resolves a model and
            // this one does not -- which is what Volvo and Mercedes VINs need.
            if other.has_model {
                return (alternative, true, other);
            }

            // Or when this block has no schema coverage at all and the other
            // does. Decisive, except for a year around today, where a missing
            // schema usually means NHTSA has not published one yet rather than
            // that the year is wrong; guessing 30 years earlier would be worse
            // than saying nothing.
            if !chosen.is_supported() && other.is_supported() && !self.is_recent(model_year.year) {
                return (alternative, true, other);
            }

            return (model_year.year, true, chosen);
        }

        match (chosen.is_supported(), other.is_supported()) {
            // Nothing to go on either way: keep NHTSA's answer.
            (false, false) => (model_year.year, false, chosen),
            (true, false) => (model_year.year, true, chosen),
            (false, true) => (alternative, true, other),
            // Both plausible, so the VIN really is ambiguous. Prefer whichever
            // the data supports better, and only claim certainty when one of
            // them resolves a model and the other does not.
            (true, true) => {
                let decisive = chosen.has_model != other.has_model;
                if other.rank() > chosen.rank() {
                    (alternative, decisive, other)
                } else {
                    (model_year.year, decisive, chosen)
                }
            }
        }
    }

    /// Whether `year` is close enough to today that missing schema data is
    /// expected rather than evidence of a misread year.
    fn is_recent(&self, year: i32) -> bool {
        (self.now_year - 1..=self.now_year + 2).contains(&year)
    }

    /// Match every pattern of every schema this WMI used in `year`.
    fn match_year(&self, vin: &str, year: i32) -> YearMatch {
        let mut result = YearMatch::default();
        if year < 0 {
            return result;
        }

        let wmi = vin::extract_wmi(vin);
        let key = match_key(vin);

        for schema in self
            .schemas
            .get(&wmi)
            .unwrap_or_default()
            .into_iter()
            .filter(|schema| schema.covers(year as u16))
        {
            result.had_schemas = true;
            let Some(rows) = self.patterns.get(&schema.schema_id.to_string()) else {
                continue;
            };

            for row in rows {
                // Formula keys read digits straight out of the VIN instead of
                // naming a fixed value.
                if pattern::is_formula(&row.keys) {
                    if let Some(value) = pattern::formula_value(&row.keys, &key) {
                        result.candidates.push(Candidate {
                            element_id: row.element_id,
                            attribute_id: value.clone(),
                            value,
                            priority: FORMULA_PRIORITY,
                            updated_on: row.updated_on,
                            literal_len: literal_len(&row.keys),
                            tie_break: row.id,
                        });
                    }
                    continue;
                }

                if keys_match(&row.keys, &key) {
                    result.has_model |= row.element_id == element::MODEL;
                    result.candidates.push(Candidate {
                        element_id: row.element_id,
                        attribute_id: row.attribute_id,
                        value: row.value,
                        priority: schema.year_from as i32,
                        updated_on: row.updated_on,
                        literal_len: literal_len(&row.keys),
                        tie_break: row.id,
                    });
                }
            }
        }

        result
    }

    fn assemble(
        &self,
        wmi_entry: &WmiEntry,
        year: i32,
        year_conclusive: bool,
        matches: YearMatch,
        mut warnings: Vec<Warning>,
    ) -> VehicleInfo {
        if !matches.had_schemas {
            warnings.push(Warning::NoSchemaForYear);
        } else if matches.candidates.is_empty() {
            warnings.push(Warning::NoPatternMatched);
        }

        let mut candidates = matches.candidates;

        // An engine model implies further engine elements.
        if let Some(engine_model) = best_of(&candidates, element::ENGINE_MODEL) {
            let name = engine_model.value.trim().to_string();
            for row in self.engine_models.get(&name).unwrap_or_default() {
                candidates.push(Candidate {
                    element_id: row.element_id,
                    attribute_id: row.attribute_id,
                    value: row.value,
                    priority: ENGINE_MODEL_PRIORITY,
                    updated_on: 0,
                    literal_len: 0,
                    tie_break: 0,
                });
            }
        }

        let mut resolved = rank(candidates);

        // The make follows from the model: vPIC's Make_Model is one-to-one, so a
        // decoded model names its make exactly, where a WMI often cannot.
        let model_entry = resolved
            .get(&element::MODEL)
            .and_then(|values| values.first())
            .and_then(|model| self.models.get(&model.attribute_id))
            .and_then(|mut entries| (!entries.is_empty()).then(|| entries.swap_remove(0)));

        let (make, make_id) = match &model_entry {
            Some(entry) => (entry.make.clone(), entry.make_id),
            None => {
                warnings.push(Warning::ModelNotFound);
                (wmi_entry.make.clone(), wmi_entry.make_id)
            }
        };

        let model_id: u32 = resolved
            .get(&element::MODEL)
            .and_then(|values| values.first())
            .and_then(|model| model.attribute_id.parse().ok())
            .unwrap_or(0);

        self.apply_displacement_conversions(&mut resolved);
        self.apply_vehicle_specs(&mut resolved, wmi_entry, make_id, model_id, year);
        self.apply_defaults(&mut resolved, wmi_entry.vehicle_type_id);

        self.build(
            resolved,
            wmi_entry,
            make,
            make_id,
            model_id,
            year,
            year_conclusive,
            warnings,
        )
    }

    /// Fill in the displacement units that were not decoded directly.
    fn apply_displacement_conversions(&self, resolved: &mut HashMap<u16, Vec<Candidate>>) {
        for (from, to, factor) in DISPLACEMENT_CONVERSIONS {
            if resolved.contains_key(&to) {
                continue;
            }
            let Some(source) = resolved
                .get(&from)
                .and_then(|values| values.first())
                .and_then(|value| value.value.trim().parse::<f64>().ok())
            else {
                continue;
            };

            let converted = format_number(source * factor);
            resolved.insert(
                to,
                vec![Candidate {
                    element_id: to,
                    attribute_id: converted.clone(),
                    value: converted,
                    priority: FORMULA_PRIORITY,
                    updated_on: 0,
                    literal_len: 0,
                    tie_break: 0,
                }],
            );
        }
    }

    /// Apply the vehicle-spec tables, which key off the decoded make, vehicle
    /// type, model and year rather than off the VIN.
    ///
    /// A spec record only applies when every one of its key cells agrees with
    /// what the VIN already resolved to, which is how NHTSA distinguishes, say,
    /// the all-wheel-drive trim of a model from the front-wheel-drive one.
    fn apply_vehicle_specs(
        &self,
        resolved: &mut HashMap<u16, Vec<Candidate>>,
        wmi_entry: &WmiEntry,
        make_id: u32,
        model_id: u32,
        year: i32,
    ) {
        if make_id == 0 || model_id == 0 {
            return;
        }

        let vehicle_type = wmi_entry.vehicle_type_id;
        let mut rows: Vec<SpecRow> = Vec::new();
        // Schemas with no year rows apply to every year; they are stored under 0.
        for year_key in [year.to_string(), "0".to_string()] {
            let key = format!("{make_id}|{vehicle_type}|{model_id}|{year_key}");
            rows.extend(self.specs.get(&key).unwrap_or_default());
        }

        if rows.is_empty() {
            return;
        }

        // Group into records, then keep the records whose key cells all agree
        // with the decode so far.
        let mut records: HashMap<u32, Vec<SpecRow>> = HashMap::new();
        for row in rows {
            records.entry(row.row_id).or_default().push(row);
        }

        let mut winners: HashMap<u16, SpecRow> = HashMap::new();
        for record in records.into_values() {
            let matches = record.iter().filter(|row| row.is_key).all(|row| {
                resolved
                    .get(&row.element_id)
                    .and_then(|values| values.first())
                    .is_some_and(|decoded| {
                        decoded.attribute_id.eq_ignore_ascii_case(&row.attribute_id)
                    })
            });
            if !matches {
                continue;
            }

            for row in record.into_iter().filter(|row| !row.is_key) {
                // Never overwrite something the VIN itself said, except for the
                // free-text elements that accumulate.
                if resolved.contains_key(&row.element_id)
                    && !element::is_multi_valued(row.element_id)
                {
                    continue;
                }
                match winners.get(&row.element_id) {
                    Some(existing) if existing.updated_on >= row.updated_on => {}
                    _ => {
                        winners.insert(row.element_id, row);
                    }
                }
            }
        }

        for (element_id, row) in winners {
            resolved.entry(element_id).or_default().push(Candidate {
                element_id,
                attribute_id: row.attribute_id,
                value: row.value,
                priority: VEHICLE_SPEC_PRIORITY,
                updated_on: row.updated_on,
                literal_len: 0,
                tie_break: 0,
            });
        }
    }

    /// Apply the per-vehicle-type defaults to whatever is still missing.
    fn apply_defaults(&self, resolved: &mut HashMap<u16, Vec<Candidate>>, vehicle_type_id: u16) {
        for row in self
            .defaults
            .get(&vehicle_type_id.to_string())
            .unwrap_or_default()
        {
            if resolved.contains_key(&row.element_id) {
                continue;
            }
            resolved.insert(
                row.element_id,
                vec![Candidate {
                    element_id: row.element_id,
                    attribute_id: row.attribute_id,
                    value: row.value,
                    priority: DEFAULT_PRIORITY,
                    updated_on: 0,
                    literal_len: 0,
                    tie_break: 0,
                }],
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        resolved: HashMap<u16, Vec<Candidate>>,
        wmi_entry: &WmiEntry,
        make: String,
        make_id: u32,
        model_id: u32,
        year: i32,
        year_conclusive: bool,
        warnings: Vec<Warning>,
    ) -> VehicleInfo {
        let mut info = VehicleInfo {
            make,
            make_id,
            model_id,
            year,
            year_conclusive,
            warnings,
            ..Default::default()
        };

        for (element_id, values) in &resolved {
            // Free-text elements can hold several values; NHTSA renders them as
            // one semicolon-separated string.
            let joined = values
                .iter()
                .map(|candidate| candidate.value.trim())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            info.set_attribute(*element_id, &joined);
        }

        // WMI-derived elements, which no pattern carries.
        info.vehicle_type =
            element::vehicle_type_name(wmi_entry.vehicle_type_id).map(str::to_string);
        if let Some(vehicle_type) = &info.vehicle_type {
            info.attributes.insert("VehicleType", vehicle_type.clone());
        }
        if !wmi_entry.manufacturer.is_empty() {
            info.manufacturer = Some(wmi_entry.manufacturer.clone());
            info.attributes
                .insert("Manufacturer", wmi_entry.manufacturer.clone());
        }
        if !wmi_entry.country.is_empty() {
            info.country = Some(wmi_entry.country.clone());
            info.attributes.insert("Country", wmi_entry.country.clone());
        }
        if !info.make.is_empty() {
            info.attributes.insert("Make", info.make.clone());
        }
        info.attributes.insert("ModelYear", year.to_string());

        // Copy the elements that have a field of their own out of the map.
        info.promote_named_fields();
        info.generation = self.generation_of(&info.make, info.model.as_deref(), year);

        info
    }

    /// Which generation of the model this model year falls in.
    ///
    /// Returns `None` for a model the generation table does not cover, which is
    /// most of the long tail: the table is worth maintaining for the few
    /// hundred models that make up the bulk of the fleet, not for every model
    /// ever sold.
    fn generation_of(&self, make: &str, model: Option<&str>, year: i32) -> Option<Generation> {
        let model = model?;
        if make.is_empty() {
            return None;
        }

        let key = format!("{}|{}", make.to_lowercase(), model.to_lowercase());
        let row = self
            .generations
            .get(&key)?
            .into_iter()
            .find(|row| row.covers(year))?;

        Some(Generation {
            ordinal: (row.ordinal != 0).then_some(row.ordinal),
            code: (!row.code.is_empty()).then_some(row.code),
            name: row.name,
            year_from: row.year_from,
            year_to: (row.year_to != 0).then_some(row.year_to),
        })
    }

    /// Decode a slice of VINs and return a map from each input reference to its result.
    ///
    /// With the `parallel` feature the VINs are split across Rayon chunks.
    pub fn decode_batch<'inp>(
        &self,
        vins: &'inp [VIN],
    ) -> HashMap<&'inp VIN, Result<VehicleInfo, CorgiError>> {
        #[cfg(not(feature = "parallel"))]
        let decoded = vins.iter().map(|vin| (vin, self.decode(vin))).collect();

        #[cfg(feature = "parallel")]
        let decoded = vins
            .into_par_iter()
            .chunks(RAYON_CHUNK_SIZE)
            .flat_map_iter(|chunk| chunk.into_iter().map(|vin| (vin, self.decode(vin))))
            .collect();

        decoded
    }

    /// Decode every VIN in `vins`, returning one result per input, in input order.
    ///
    /// This is the shape to use when the results have to line up with the rows
    /// they came from: unlike [`decode_batch`](Self::decode_batch) it keeps
    /// duplicates and the order of the input, and it accepts anything that
    /// borrows as a string. With the `parallel` feature the VINs are split
    /// across Rayon chunks.
    ///
    /// ```no_run
    /// # use corgi_rs::VinDecoder;
    /// let decoder = VinDecoder::new();
    /// let vins = ["1C6RR7LT2JS179571", "5XYRLDLC3NG097496"];
    ///
    /// for (vin, result) in vins.iter().zip(decoder.decode_all(&vins)) {
    ///     match result {
    ///         Ok(info) => println!("{vin}: {} {}", info.year, info.make),
    ///         Err(err) => eprintln!("{vin}: {err}"),
    ///     }
    /// }
    /// ```
    pub fn decode_all<S>(&self, vins: &[S]) -> Vec<Result<VehicleInfo, CorgiError>>
    where
        S: AsRef<str> + Sync,
    {
        #[cfg(not(feature = "parallel"))]
        let decoded = vins.iter().map(|vin| self.decode(vin.as_ref())).collect();

        // `collect` off an indexed parallel iterator restores the input order.
        #[cfg(feature = "parallel")]
        let decoded = vins
            .into_par_iter()
            .with_min_len(RAYON_CHUNK_SIZE)
            .map(|vin| self.decode(vin.as_ref()))
            .collect();

        decoded
    }

    /// Consume an owned `Vec<VIN>` and decode each entry, returning owned VIN keys.
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

impl Default for VinDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Uppercase the VIN and drop the separators people paste along with it.
fn normalize(vin: &str) -> String {
    vin.trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The highest-ranked candidate for `element_id`, if any.
fn best_of(candidates: &[Candidate], element_id: u16) -> Option<&Candidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.element_id == element_id)
        .reduce(|best, next| if next.outranks(best) { next } else { best })
}

/// Reduce the candidates to one winner per element, keeping every value for the
/// free-text elements that are allowed to repeat.
fn rank(candidates: Vec<Candidate>) -> HashMap<u16, Vec<Candidate>> {
    let mut resolved: HashMap<u16, Vec<Candidate>> = HashMap::new();

    for candidate in candidates {
        let entry = resolved.entry(candidate.element_id).or_default();

        if element::is_multi_valued(candidate.element_id) {
            if !entry.iter().any(|seen| seen.value == candidate.value) {
                entry.push(candidate);
            }
            continue;
        }

        match entry.first() {
            Some(best) if !candidate.outranks(best) => {}
            _ => {
                entry.clear();
                entry.push(candidate);
            }
        }
    }

    resolved
}

/// Render a converted number without trailing noise: `3600` rather than `3600.0`,
/// `3.6` rather than `3.5999999999999996`.
fn format_number(value: f64) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    if (rounded.fract()).abs() < f64::EPSILON {
        format!("{}", rounded as i64)
    } else {
        let mut text = format!("{rounded:.3}");
        while text.ends_with('0') {
            text.pop();
        }
        text.trim_end_matches('.').to_string()
    }
}

/// Element metadata for everything the decoder can produce.
pub fn elements() -> &'static [Element] {
    element::all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_all_keeps_input_order_and_duplicates() {
        let decoder = VinDecoder::new();
        // A duplicate and a deliberately undecodable entry: both must survive,
        // in place, so results line up with the rows they came from.
        let vins = [
            "5XYRLDLC3NG097496",
            "1C6RR7LT2JS179571",
            "5XYRLDLC3NG097496",
            "NOTAVIN",
            "1HGCP26739A060971",
        ];

        let results = decoder.decode_all(&vins);

        assert_eq!(results.len(), vins.len());
        assert_eq!(results[0].as_ref().expect("decodes").make, "Kia");
        assert_eq!(results[1].as_ref().expect("decodes").make, "Ram");
        assert_eq!(results[2].as_ref().expect("decodes").make, "Kia");
        assert!(results[3].is_err());
        assert_eq!(results[4].as_ref().expect("decodes").make, "Honda");
    }

    #[test]
    fn decode_all_accepts_owned_and_borrowed_vins() {
        let decoder = VinDecoder::new();
        let owned = vec!["1C6RR7LT2JS179571".to_string()];
        let borrowed = ["1C6RR7LT2JS179571"];

        assert_eq!(
            decoder.decode_all(&owned)[0]
                .as_ref()
                .expect("decodes")
                .make,
            decoder.decode_all(&borrowed)[0]
                .as_ref()
                .expect("decodes")
                .make
        );
    }

    #[test]
    fn decode_all_agrees_with_decode_one_at_a_time() {
        let decoder = VinDecoder::new();
        let vins = [
            "5XYRLDLC3NG097496",
            "1C6RR7LT2JS179571",
            "1HGCP26739A060971",
            "4T1BF1FK8HU640530",
        ];

        for (vin, batched) in vins.iter().zip(decoder.decode_all(&vins)) {
            let single = decoder.decode(vin).expect("decodes");
            let batched = batched.expect("decodes");
            assert_eq!(single.make, batched.make, "{vin}");
            assert_eq!(single.model, batched.model, "{vin}");
            assert_eq!(single.attributes, batched.attributes, "{vin}");
        }
    }

    #[test]
    fn normalize_strips_formatting_and_upcases() {
        assert_eq!(normalize(" 1c6rr7lt2js179571 "), "1C6RR7LT2JS179571");
        assert_eq!(normalize("1C6-RR7LT2-JS179571"), "1C6RR7LT2JS179571");
    }

    #[test]
    fn candidates_rank_by_year_first_then_by_recency() {
        let base = Candidate {
            element_id: element::MODEL,
            attribute_id: "1".into(),
            value: "old".into(),
            priority: 1994,
            updated_on: 900,
            literal_len: 5,
            tie_break: 1,
        };
        let newer_schema = Candidate {
            priority: 2019,
            value: "new".into(),
            ..base.clone()
        };
        assert!(newer_schema.outranks(&base));
        assert!(!base.outranks(&newer_schema));

        let same_year_newer_row = Candidate {
            updated_on: 1000,
            ..base.clone()
        };
        assert!(same_year_newer_row.outranks(&base));
    }

    #[test]
    fn candidates_break_ties_on_shorter_keys_then_id() {
        let long_key = Candidate {
            element_id: element::MODEL,
            attribute_id: "1".into(),
            value: "a".into(),
            priority: 2019,
            updated_on: 1000,
            literal_len: 7,
            tie_break: 1,
        };
        let short_key = Candidate {
            literal_len: 3,
            ..long_key.clone()
        };
        assert!(short_key.outranks(&long_key));

        let lower_id = Candidate {
            tie_break: 0,
            ..long_key.clone()
        };
        assert!(lower_id.outranks(&long_key));
    }

    #[test]
    fn rank_keeps_one_value_per_element_but_all_notes() {
        let note = element::MULTI_VALUED[1];
        let candidates = vec![
            Candidate {
                element_id: element::MODEL,
                attribute_id: "1".into(),
                value: "loser".into(),
                priority: 1990,
                updated_on: 0,
                literal_len: 0,
                tie_break: 0,
            },
            Candidate {
                element_id: element::MODEL,
                attribute_id: "2".into(),
                value: "winner".into(),
                priority: 2020,
                updated_on: 0,
                literal_len: 0,
                tie_break: 0,
            },
            Candidate {
                element_id: note,
                attribute_id: "a".into(),
                value: "first note".into(),
                priority: 2020,
                updated_on: 0,
                literal_len: 0,
                tie_break: 0,
            },
            Candidate {
                element_id: note,
                attribute_id: "b".into(),
                value: "second note".into(),
                priority: 1990,
                updated_on: 0,
                literal_len: 0,
                tie_break: 0,
            },
        ];

        let resolved = rank(candidates);
        assert_eq!(resolved[&element::MODEL].len(), 1);
        assert_eq!(resolved[&element::MODEL][0].value, "winner");
        assert_eq!(resolved[&note].len(), 2);
    }

    #[test]
    fn format_number_trims_float_noise() {
        assert_eq!(format_number(3600.0), "3600");
        assert_eq!(format_number(3.5999999999999996), "3.6");
        assert_eq!(format_number(219.68), "219.68");
    }
}
