//! Row types for the archived vPIC assets, shared between `build.rs` and the
//! runtime decoder.
//!
//! Every asset is a tab-separated file whose first column is the lookup key and
//! whose rows are already sorted by that key in byte order, so `build.rs` can
//! stream them straight into an `fst` map with the remaining columns archived
//! per key via `rkyv`. See `tools/extract_assets.sql` for the queries that
//! produce them.

use std::{borrow::Cow, iter::Peekable, str::FromStr};

use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighDeserializer, HighSerializer},
    rancor::Error,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};

#[allow(dead_code)]
/// Marker trait for types that support rkyv deserialization via the shared helpers.
pub trait RkyvDeserialize<D>: Deserialize<D, HighDeserializer<Error>> {}

/// Marker trait used to enforce rkyv serialization compatibility for cached assets.
pub trait RkyvSerialize:
    for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>
{
}

/// Types that know how to name their cached `.fst`/`.bin` assets.
pub trait Saveable<'a> {
    /// Base file name (without extension) that corresponds to the persisted map data.
    fn base_file_name() -> Cow<'a, str>;
}

/// Parse error for a malformed asset row.
#[derive(Debug)]
pub struct RowParseError(pub String);

impl std::fmt::Display for RowParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed asset row: {}", self.0)
    }
}

impl std::error::Error for RowParseError {}

impl From<RowParseError> for std::io::Error {
    fn from(value: RowParseError) -> Self {
        std::io::Error::other(value.0)
    }
}

/// Split a value row into its tab-separated fields.
fn fields(s: &str) -> Vec<&str> {
    s.split('\t').collect()
}

fn field<'a>(f: &[&'a str], idx: usize, row: &str) -> Result<&'a str, RowParseError> {
    f.get(idx)
        .copied()
        .ok_or_else(|| RowParseError(format!("missing column {idx} in `{row}`")))
}

fn num<T: FromStr + Default>(f: &[&str], idx: usize) -> T {
    f.get(idx)
        .and_then(|v| v.parse::<T>().ok())
        .unwrap_or_default()
}

fn owned(f: &[&str], idx: usize) -> String {
    f.get(idx).copied().unwrap_or("").to_string()
}

/// Turns an empty asset column into `None`.
fn optional(f: &[&str], idx: usize) -> Option<String> {
    match f.get(idx).copied().unwrap_or("") {
        "" => None,
        v => Some(v.to_string()),
    }
}

//
// wmi.tsv -- one row per world manufacturer identifier.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Default)]
/// Everything the decoder derives from the WMI alone.
pub struct WmiEntry {
    /// vPIC `VehicleType.Id`: 2 = Passenger Car, 3 = Truck, 7 = MPV, 10 = Incomplete.
    pub vehicle_type_id: u16,
    /// vPIC `TruckType.Id`; 1 marks a light truck, which changes model-year decoding.
    pub truck_type_id: u16,
    /// Make id, but only when this WMI maps to exactly one make. `0` when the WMI
    /// is shared (as `1C4` is between Jeep, Ram, Dodge, Chrysler and Fiat), in
    /// which case the make has to come from the decoded model instead.
    pub make_id: u32,
    /// Make name that goes with [`WmiEntry::make_id`], empty when ambiguous.
    pub make: String,
    pub manufacturer: String,
    pub country: String,
}

impl RkyvDeserialize<WmiEntry> for ArchivedWmiEntry {}
impl RkyvSerialize for WmiEntry {}

impl<'a> Saveable<'a> for WmiEntry {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("wmi")
    }
}

impl FromStr for WmiEntry {
    type Err = RowParseError;

    /// Parse `vehicle_type<TAB>truck_type<TAB>make_id<TAB>make<TAB>manufacturer<TAB>country`.
    ///
    /// # Examples
    ///
    /// ```
    /// use corgi_rs::build_shared::WmiEntry;
    /// let wmi: WmiEntry = "7\t0\t0\t\tFCA US LLC\tUNITED STATES (USA)".parse().unwrap();
    /// assert_eq!(wmi.vehicle_type_id, 7);
    /// assert_eq!(wmi.make_id, 0);
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(WmiEntry {
            vehicle_type_id: num(&f, 0),
            truck_type_id: num(&f, 1),
            make_id: num(&f, 2),
            make: owned(&f, 3),
            manufacturer: owned(&f, 4),
            country: owned(&f, 5),
        })
    }
}

//
// wmi_schema.tsv -- which VIN schemas a WMI may use, and for which model years.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// A VIN schema together with the model-year window it applies to.
///
/// The year window is what keeps a 2019 Ram 1500 from being decoded against the
/// 1994 Dodge schema that shares its WMI.
pub struct SchemaRef {
    pub schema_id: u32,
    pub year_from: u16,
    pub year_to: u16,
}

impl RkyvDeserialize<SchemaRef> for ArchivedSchemaRef {}
impl RkyvSerialize for SchemaRef {}

impl<'a> Saveable<'a> for SchemaRef {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("wmi_schema")
    }
}

impl SchemaRef {
    /// Whether this schema is valid for `model_year`.
    pub fn covers(&self, model_year: u16) -> bool {
        model_year >= self.year_from && model_year <= self.year_to
    }
}

impl FromStr for SchemaRef {
    type Err = RowParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(SchemaRef {
            schema_id: num(&f, 0),
            year_from: num(&f, 1),
            year_to: num(&f, 2),
        })
    }
}

//
// pattern.tsv -- the VIN patterns themselves.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// One `vpic.Pattern` row: a key matched against the VIN, and the element value
/// it yields.
pub struct PatternRow {
    /// Pattern key, e.g. `RJFBG` or `*****|*H`. See [`crate::pattern`].
    pub keys: String,
    /// vPIC `Element.Id`.
    pub element_id: u16,
    /// Raw attribute id. For lookup elements this is the numeric id in the
    /// element's lookup table; vehicle-spec keys are matched on it.
    pub attribute_id: String,
    /// Display value, already resolved through the element's lookup table.
    pub value: String,
    /// `UpdatedOn`/`CreatedOn` as a unix timestamp, used to break ranking ties.
    pub updated_on: i64,
    /// vPIC `Pattern.Id`, the last tie-break so ranking is deterministic.
    pub id: u32,
}

impl RkyvDeserialize<PatternRow> for ArchivedPatternRow {}
impl RkyvSerialize for PatternRow {}

impl<'a> Saveable<'a> for PatternRow {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("pattern")
    }
}

impl FromStr for PatternRow {
    type Err = RowParseError;

    /// Parse `keys<TAB>element_id<TAB>attribute_id<TAB>value<TAB>updated_on<TAB>id`.
    ///
    /// # Examples
    ///
    /// ```
    /// use corgi_rs::build_shared::PatternRow;
    /// let row: PatternRow = "*B\t37\t3\tManual/Standard\t1425463435\t14".parse().unwrap();
    /// assert_eq!(row.element_id, 37);
    /// assert_eq!(row.value, "Manual/Standard");
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(PatternRow {
            keys: field(&f, 0, s)?.to_string(),
            element_id: num(&f, 1),
            attribute_id: owned(&f, 2),
            value: owned(&f, 3),
            updated_on: num(&f, 4),
            id: num(&f, 5),
        })
    }
}

//
// model.tsv -- model id to make.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// A model and the make that owns it.
///
/// `Make_Model` is one-to-one in vPIC, so a decoded model pins the make exactly.
/// This is how the decoder tells a Ram from a Jeep on the shared FCA WMIs.
pub struct ModelEntry {
    pub model: String,
    pub make_id: u32,
    pub make: String,
}

impl RkyvDeserialize<ModelEntry> for ArchivedModelEntry {}
impl RkyvSerialize for ModelEntry {}

impl<'a> Saveable<'a> for ModelEntry {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("model")
    }
}

impl FromStr for ModelEntry {
    type Err = RowParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(ModelEntry {
            model: owned(&f, 0),
            make_id: num(&f, 1),
            make: owned(&f, 2),
        })
    }
}

//
// vspec.tsv -- the VehicleSpecSchema tables.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// One cell of a vehicle-spec table, keyed by `make|vehicle_type|model|year`.
///
/// Rows with the same [`SpecRow::row_id`] form a record: every `is_key` cell has
/// to agree with what the VIN patterns already decoded before the remaining
/// cells are applied. These supply the drive type, transmission and
/// driver-assist elements that no VIN pattern carries.
pub struct SpecRow {
    pub row_id: u32,
    pub is_key: bool,
    pub element_id: u16,
    pub attribute_id: String,
    pub value: String,
    pub updated_on: i64,
}

impl RkyvDeserialize<SpecRow> for ArchivedSpecRow {}
impl RkyvSerialize for SpecRow {}

impl<'a> Saveable<'a> for SpecRow {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("vspec")
    }
}

impl FromStr for SpecRow {
    type Err = RowParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(SpecRow {
            row_id: num(&f, 0),
            is_key: num::<u8>(&f, 1) == 1,
            element_id: num(&f, 2),
            attribute_id: owned(&f, 3),
            value: owned(&f, 4),
            updated_on: num(&f, 5),
        })
    }
}

//
// engine_model.tsv / default_value.tsv -- both are plain element/value rows.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// Extra elements implied by a decoded engine model name.
pub struct EngineModelRow {
    pub element_id: u16,
    pub attribute_id: String,
    pub value: String,
}

impl RkyvDeserialize<EngineModelRow> for ArchivedEngineModelRow {}
impl RkyvSerialize for EngineModelRow {}

impl<'a> Saveable<'a> for EngineModelRow {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("engine_model")
    }
}

impl FromStr for EngineModelRow {
    type Err = RowParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(EngineModelRow {
            element_id: num(&f, 0),
            attribute_id: owned(&f, 1),
            value: owned(&f, 2),
        })
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// Per-vehicle-type fallback applied to elements nothing else resolved.
pub struct DefaultValueRow {
    pub element_id: u16,
    pub attribute_id: String,
    pub value: String,
}

impl RkyvDeserialize<DefaultValueRow> for ArchivedDefaultValueRow {}
impl RkyvSerialize for DefaultValueRow {}

impl<'a> Saveable<'a> for DefaultValueRow {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("default_value")
    }
}

impl FromStr for DefaultValueRow {
    type Err = RowParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(DefaultValueRow {
            element_id: num(&f, 0),
            attribute_id: owned(&f, 1),
            value: owned(&f, 2),
        })
    }
}

//
// generation.tsv -- hand-maintained model generations.
//

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// One generation of one model, keyed by `make|model` in lower case.
///
/// vPIC has no generation element -- NHTSA registers what a vehicle is, not how
/// its maker markets the redesign cycle -- so this table is maintained by hand
/// in `tools/generations.tsv` and checked against real VINs.
pub struct GenerationRow {
    /// First model year, inclusive.
    pub year_from: u16,
    /// Last model year, inclusive. `0` means the generation is still current.
    pub year_to: u16,
    /// Ordinal as commonly used, e.g. `10` for a tenth-generation Civic.
    /// `0` when the generation has no number people actually use.
    pub ordinal: u8,
    /// Manufacturer platform or chassis code, e.g. `W205` or `XV70`. May be empty.
    pub code: String,
    /// Display label.
    pub name: String,
}

impl RkyvDeserialize<GenerationRow> for ArchivedGenerationRow {}
impl RkyvSerialize for GenerationRow {}

impl<'a> Saveable<'a> for GenerationRow {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("generation")
    }
}

impl GenerationRow {
    /// Whether this generation covers `model_year`.
    pub fn covers(&self, model_year: i32) -> bool {
        model_year >= self.year_from as i32
            && (self.year_to == 0 || model_year <= self.year_to as i32)
    }
}

impl FromStr for GenerationRow {
    type Err = RowParseError;

    /// Parse `year_from<TAB>year_to<TAB>ordinal<TAB>code<TAB>name`.
    ///
    /// # Examples
    ///
    /// ```
    /// use corgi_rs::build_shared::GenerationRow;
    /// let row: GenerationRow = "2016\t2021\t10\tFC/FK\t10th generation".parse().unwrap();
    /// assert!(row.covers(2018));
    /// assert!(!row.covers(2022));
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(GenerationRow {
            year_from: num(&f, 0),
            year_to: num(&f, 1),
            ordinal: num(&f, 2),
            code: owned(&f, 3),
            name: field(&f, 4, s)?.to_string(),
        })
    }
}

/// A row of `element.tsv`, used by `build.rs` to emit the static element table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementMeta {
    pub id: u16,
    pub code: String,
    pub name: String,
    pub data_type: String,
    pub group: Option<String>,
}

impl FromStr for ElementMeta {
    type Err = RowParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let f = fields(s);
        Ok(ElementMeta {
            id: num(&f, 0),
            code: field(&f, 1, s)?.to_string(),
            name: owned(&f, 2),
            data_type: owned(&f, 3),
            group: optional(&f, 4),
        })
    }
}

/// Utility trait that helps parse asset tables grouped by their leading key.
pub trait UntilNextKey<'a> {
    /// Returns the next key and collection of rows until the key changes.
    fn next_key(&mut self) -> Option<(&'a str, Vec<&'a str>)>;
}

impl<'a, I> UntilNextKey<'a> for Peekable<I>
where
    I: Iterator<Item = &'a str>,
{
    fn next_key(&mut self) -> Option<(&'a str, Vec<&'a str>)> {
        let mut current_key: Option<&'a str> = None;
        let mut values = Vec::new();

        while let Some(line) = self.peek() {
            let Some((key, rest)) = line.split_once('\t') else {
                // A key with no columns after it carries no data; skip it rather
                // than aborting the whole asset.
                self.next();
                continue;
            };

            match current_key {
                Some(ck) if ck != key => return Some((ck, values)),
                None => current_key = Some(key),
                Some(_) => {}
            }

            values.push(rest);
            self.next();
        }

        current_key.map(|key| (key, values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn until_next_key_groups_rows_sharing_a_key() {
        let table = "a\t1\na\t2\nb\t3\n";
        let mut iter = table.lines().peekable();
        assert_eq!(iter.next_key(), Some(("a", vec!["1", "2"])));
        assert_eq!(iter.next_key(), Some(("b", vec!["3"])));
        assert_eq!(iter.next_key(), None);
    }

    #[test]
    fn until_next_key_splits_on_the_first_tab_only() {
        let table = "1C4\t7\t0\t0\t\tFCA US LLC\tUNITED STATES (USA)\n1C6\t3\t0\t0\t\tFCA US LLC\tUNITED STATES (USA)";
        let mut iter = table.lines().peekable();
        let (k1, v1) = iter.next_key().expect("first key");
        let (k2, _) = iter.next_key().expect("second key");
        assert_eq!(k1, "1C4");
        assert_eq!(v1, vec!["7\t0\t0\t\tFCA US LLC\tUNITED STATES (USA)"]);
        assert_eq!(k2, "1C6");
        assert!(iter.next_key().is_none());
    }

    #[test]
    fn wmi_entry_leaves_make_empty_when_the_wmi_is_shared() {
        let wmi: WmiEntry = "7\t0\t0\t\tFCA US LLC\tUNITED STATES (USA)"
            .parse()
            .expect("parse");
        assert_eq!(wmi.make_id, 0);
        assert!(wmi.make.is_empty());
        assert_eq!(wmi.manufacturer, "FCA US LLC");
    }

    #[test]
    fn schema_ref_covers_its_year_window_inclusively() {
        let schema: SchemaRef = "12345\t2014\t2018".parse().expect("parse");
        assert!(schema.covers(2014));
        assert!(schema.covers(2018));
        assert!(!schema.covers(2013));
        assert!(!schema.covers(2019));
    }

    #[test]
    fn pattern_row_keeps_values_containing_separators_intact() {
        let row: PatternRow =
            "RJFBG\t5\t7\tSport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)\t1425463435\t14"
                .parse()
                .expect("parse");
        assert_eq!(
            row.value,
            "Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)"
        );
        assert_eq!(row.id, 14);
    }

    #[test]
    fn a_current_generation_has_an_open_upper_bound() {
        let current: GenerationRow = "2022\t0\t11\tFE/FL\t11th generation"
            .parse()
            .expect("parse");
        assert!(current.covers(2022));
        assert!(current.covers(2030));
        assert!(!current.covers(2021));

        let closed: GenerationRow = "2016\t2021\t10\tFC/FK\t10th generation"
            .parse()
            .expect("parse");
        assert!(closed.covers(2021));
        assert!(!closed.covers(2022));
    }

    #[test]
    fn spec_row_reads_its_key_flag() {
        let key: SpecRow = "14622\t1\t5\t13\tSedan/Saloon\t1692965930"
            .parse()
            .expect("parse");
        let value: SpecRow = "14622\t0\t2\t3\tLithium-Ion/Li-Ion\t1707830521"
            .parse()
            .expect("parse");
        assert!(key.is_key);
        assert!(!value.is_key);
        assert_eq!(key.row_id, value.row_id);
    }
}
