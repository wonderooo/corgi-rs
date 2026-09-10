//! The decoded vehicle record.

use std::collections::BTreeMap;
use std::fmt::Display;

use crate::element;

/// Everything a VIN resolved to.
///
/// The named fields cover the attributes most callers want. Everything vPIC
/// produced — well over a hundred elements, from `EngineHP` to `SeatRows` to the
/// driver-assist systems — is also in [`VehicleInfo::attributes`], keyed by
/// element code, and reachable through [`VehicleInfo::get`] and its typed
/// siblings.
///
/// # Examples
///
/// ```no_run
/// use corgi_rs::VinDecoder;
///
/// let decoder = VinDecoder::new();
/// let info = decoder.decode("1C6RR7LT2JS179571").expect("valid VIN");
///
/// println!("{} {}", info.make, info.model.as_deref().unwrap_or("?"));
/// println!("{:?} hp", info.engine_hp);
/// for (code, value) in &info.attributes {
///     println!("{code}: {value}");
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct VehicleInfo {
    //
    // Identity
    //
    /// Marque, e.g. `Ram`. Derived from the decoded model where possible, since
    /// several makes share a WMI.
    pub make: String,
    /// vPIC make id, `0` when the make could not be resolved.
    pub make_id: u32,
    pub model: Option<String>,
    /// vPIC model id, `0` when no model pattern matched.
    pub model_id: u32,
    pub year: i32,
    /// False when position 10 alone could not fix the 30-year block, so the year
    /// is NHTSA's best guess rather than a certainty.
    pub year_conclusive: bool,
    pub series: Option<String>,
    pub series2: Option<String>,
    pub trim: Option<String>,
    pub trim2: Option<String>,
    pub manufacturer: Option<String>,
    /// Country the manufacturer registered the WMI from.
    pub country: Option<String>,
    /// NHTSA `VehicleType`: `Passenger Car`, `Truck`, `Multipurpose Passenger
    /// Vehicle (MPV)`, `Bus` or `Incomplete Vehicle`.
    pub vehicle_type: Option<String>,
    /// Which generation of the model this is.
    ///
    /// `None` when the model is not in the generation table, which covers the
    /// several hundred models that make up the bulk of the US fleet rather than
    /// every model ever sold. See [`Generation`].
    pub generation: Option<Generation>,

    //
    // Body
    //
    /// Normalised body style. See [`VehicleInfo::body_class`] for NHTSA's own
    /// wording.
    pub body_style: Option<BodyStyle>,
    /// Raw NHTSA `BodyClass`, e.g. `Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)`.
    pub body_class: Option<String>,
    /// Pickup cab, e.g. `Crew/Super Crew/Crew Max`.
    pub cab_type: Option<String>,
    pub doors: Option<i32>,
    pub seats: Option<i32>,
    pub seat_rows: Option<i32>,
    pub wheels: Option<i32>,
    /// `Left-Hand Drive (LHD)` or `Right-Hand Drive (RHD)`.
    pub steering_location: Option<String>,

    //
    // Engine
    //
    /// Engine model designation, e.g. `Pentastar`.
    pub engine_type: Option<String>,
    pub engine_manufacturer: Option<String>,
    pub engine_cylinders: Option<i32>,
    /// `V-Shaped`, `In-Line`, `Rotary` and so on.
    pub engine_configuration: Option<String>,
    /// Output in horsepower, at the bottom of the published range. `EngineHP_to`
    /// in [`VehicleInfo::attributes`] gives the top of it.
    pub engine_hp: Option<f64>,
    pub displacement_l: Option<f64>,
    pub displacement_cc: Option<f64>,
    pub displacement_ci: Option<f64>,
    pub valve_train_design: Option<String>,
    pub fuel_injection_type: Option<String>,
    pub turbo: Option<String>,
    /// Primary fuel. `fuel_type_secondary` and `electrification_level` describe
    /// hybrids and dual-fuel vehicles.
    pub fuel_type: Option<String>,
    pub fuel_type_secondary: Option<String>,
    /// `Mild HEV`, `Strong HEV`, `PHEV`, `BEV`, `FCEV`. `None` on a conventional
    /// vehicle; see [`VehicleInfo::is_electrified`].
    pub electrification_level: Option<String>,

    //
    // Drivetrain
    //
    pub drive_type: Option<String>,
    /// Transmission style, e.g. `Automatic` or `Manual/Standard`.
    pub transmission: Option<String>,
    pub transmission_speeds: Option<i32>,
    pub axles: Option<i32>,
    pub brake_system_type: Option<String>,

    //
    // Weight and dimensions
    //
    /// Gross vehicle weight rating, as a class band. `gvwr_to` gives the top of
    /// the range where the manufacturer supplied one.
    pub gvwr: Option<String>,
    pub gvwr_to: Option<String>,
    /// Wheelbase in inches, at the short end where a range was published.
    pub wheel_base_in: Option<f64>,

    //
    // Plant of assembly
    //
    pub plant_country: Option<String>,
    pub plant_state: Option<String>,
    pub plant_city: Option<String>,
    pub plant_company: Option<String>,

    //
    // Safety equipment. NHTSA records these as `Standard`, `Optional` or
    // `Not Available`; [`VehicleInfo::get_bool`] collapses that to a yes/no.
    //
    pub abs: Option<String>,
    pub esc: Option<String>,
    pub traction_control: Option<String>,
    pub tpms: Option<String>,
    /// Backup camera (NHTSA calls it the rear visibility system).
    pub backup_camera: Option<String>,
    pub forward_collision_warning: Option<String>,
    pub lane_departure_warning: Option<String>,
    pub lane_keep_assist: Option<String>,
    pub blind_spot_monitor: Option<String>,
    pub adaptive_cruise_control: Option<String>,
    pub daytime_running_light: Option<String>,
    pub keyless_ignition: Option<String>,
    pub airbag_front: Option<String>,
    pub airbag_side: Option<String>,
    pub airbag_curtain: Option<String>,
    pub airbag_knee: Option<String>,
    pub seat_belt_type: Option<String>,

    //
    // Everything else
    //
    /// Every decoded element, keyed by its vPIC element code.
    ///
    /// This is the complete record: each named field above is a copy of one
    /// entry here, kept for convenience. Elements without a field of their own
    /// — engine notes, battery details, wheel sizes, the rarer driver-assist
    /// systems — are only here.
    pub attributes: BTreeMap<&'static str, String>,
    /// Problems NHTSA would have reported alongside the decode. An empty list
    /// means a clean decode.
    pub warnings: Vec<Warning>,
}

/// Which generation of a model a vehicle belongs to.
///
/// This does not come from vPIC. NHTSA registers what a vehicle *is*, not how
/// its manufacturer markets the redesign cycle, so there is no generation
/// element and the VIN schemas are filed per model year rather than per
/// generation. The table behind this is maintained by hand in
/// `tools/generations.tsv`, and every boundary in it is checked against the
/// body codes of real VINs.
///
/// The VIN itself is not a reliable source either: Honda puts its chassis code
/// in positions 4-6, so a Civic's generation is readable straight off the VIN,
/// but Ford and Toyota use those positions for cab, series and weight rating,
/// and reuse the same codes across two decades.
///
/// # Examples
///
/// ```no_run
/// # use corgi_rs::VinDecoder;
/// let info = VinDecoder::new().decode("1HGCP26739A060971").unwrap();
/// let generation = info.generation.as_ref().unwrap();
/// assert_eq!(generation.ordinal, Some(8));
/// assert_eq!(generation.name, "8th generation");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Generation {
    /// Ordinal as commonly used, e.g. `10` for a tenth-generation Civic.
    /// `None` when the generation has no number people actually use.
    pub ordinal: Option<u8>,
    /// Manufacturer platform or chassis code, e.g. `W205`, `XV70`, `P552`.
    pub code: Option<String>,
    /// Display label, e.g. `10th generation` or `W205`.
    pub name: String,
    /// First model year, inclusive.
    pub year_from: u16,
    /// Last model year, inclusive. `None` while the generation is current.
    pub year_to: Option<u16>,
}

impl Generation {
    /// Whether this generation was still in production in `model_year`.
    pub fn covers(&self, model_year: i32) -> bool {
        model_year >= self.year_from as i32
            && self.year_to.is_none_or(|last| model_year <= last as i32)
    }

    /// How many model years the generation ran, counting `through` as the last
    /// year when it is still current.
    pub fn span(&self, through: i32) -> i32 {
        let last = self.year_to.map_or(through, i32::from);
        (last - self.year_from as i32 + 1).max(1)
    }
}

impl Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.year_to {
            Some(last) => write!(f, "{} ({}-{})", self.name, self.year_from, last),
            None => write!(f, "{} ({}-)", self.name, self.year_from),
        }
    }
}

/// Something questionable about a VIN that did not stop it from decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Warning {
    /// Position 9 does not agree with the computed check digit. Common on
    /// off-road and grey-import VINs, and on transcription errors.
    CheckDigitMismatch,
    /// The 30-year model-year block could not be established from the VIN.
    ModelYearAmbiguous,
    /// The WMI resolved, but no schema covered this model year.
    NoSchemaForYear,
    /// Schemas were found but no pattern matched, so only WMI-level data is set.
    NoPatternMatched,
    /// No model pattern matched, so the make comes from the WMI (or is missing).
    ModelNotFound,
}

impl Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::CheckDigitMismatch => "check digit does not match",
            Self::ModelYearAmbiguous => "model year could not be established conclusively",
            Self::NoSchemaForYear => "no VIN schema covers this model year",
            Self::NoPatternMatched => "no VIN pattern matched",
            Self::ModelNotFound => "no model pattern matched",
        };
        write!(f, "{text}")
    }
}

impl VehicleInfo {
    /// The raw value of an element, by its vPIC code.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use corgi_rs::VinDecoder;
    /// # let info = VinDecoder::new().decode("1C6RR7LT2JS179571").unwrap();
    /// let seats = info.get("Seats");
    /// ```
    pub fn get(&self, code: &str) -> Option<&str> {
        self.attributes.get(code).map(String::as_str)
    }

    /// An element parsed as an integer, ignoring values that are not numeric.
    pub fn get_i32(&self, code: &str) -> Option<i32> {
        self.get(code)?.trim().parse().ok()
    }

    /// An element parsed as a float, ignoring values that are not numeric.
    pub fn get_f64(&self, code: &str) -> Option<f64> {
        self.get(code)?.trim().parse().ok()
    }

    /// Whether an element resolved to an affirmative value. NHTSA spells these
    /// as `Standard`, `Optional` or `Yes` depending on the element.
    pub fn get_bool(&self, code: &str) -> Option<bool> {
        let value = self.get(code)?.trim();
        match value.to_ascii_lowercase().as_str() {
            "standard" | "optional" | "yes" => Some(true),
            "not available" | "no" | "not applicable" | "" => Some(false),
            _ => None,
        }
    }

    /// Copy the elements that have a field of their own out of
    /// [`VehicleInfo::attributes`].
    ///
    /// Which elements earn a field is decided by how often they actually
    /// decode: everything resolved on more than about a fifth of real VINs is
    /// here, plus a few rarer ones that change how a vehicle is classified.
    pub(crate) fn promote_named_fields(&mut self) {
        macro_rules! text {
            ($($field:ident <- $code:literal),* $(,)?) => {
                $( self.$field = self.attributes.get($code).cloned(); )*
            };
        }
        macro_rules! number {
            ($($field:ident : $ty:ty = $code:literal),* $(,)?) => {
                $( self.$field = self
                    .attributes
                    .get($code)
                    .and_then(|value| value.trim().parse::<$ty>().ok()); )*
            };
        }

        text! {
            model <- "Model",
            series <- "Series",
            series2 <- "Series2",
            trim <- "Trim",
            trim2 <- "Trim2",
            body_class <- "BodyClass",
            cab_type <- "BodyCabType",
            steering_location <- "SteeringLocation",

            engine_type <- "EngineModel",
            engine_manufacturer <- "EngineManufacturer",
            engine_configuration <- "EngineConfiguration",
            valve_train_design <- "ValveTrainDesign",
            fuel_injection_type <- "FuelInjectionType",
            turbo <- "Turbo",
            fuel_type <- "FuelTypePrimary",
            fuel_type_secondary <- "FuelTypeSecondary",
            electrification_level <- "ElectrificationLevel",

            drive_type <- "DriveType",
            transmission <- "TransmissionStyle",
            brake_system_type <- "BrakeSystemType",

            gvwr <- "GVWR",
            gvwr_to <- "GVWR_to",

            plant_country <- "PlantCountry",
            plant_state <- "PlantState",
            plant_city <- "PlantCity",
            plant_company <- "PlantCompanyName",

            abs <- "ABS",
            esc <- "ESC",
            traction_control <- "TractionControl",
            tpms <- "TPMS",
            backup_camera <- "RearVisibilitySystem",
            forward_collision_warning <- "ForwardCollisionWarning",
            lane_departure_warning <- "LaneDepartureWarning",
            lane_keep_assist <- "LaneKeepSystem",
            blind_spot_monitor <- "BlindSpotMon",
            adaptive_cruise_control <- "AdaptiveCruiseControl",
            daytime_running_light <- "DaytimeRunningLight",
            keyless_ignition <- "KeylessIgnition",
            airbag_front <- "AirBagLocFront",
            airbag_side <- "AirBagLocSide",
            airbag_curtain <- "AirBagLocCurtain",
            airbag_knee <- "AirBagLocKnee",
            seat_belt_type <- "SeatBeltsAll",
        }

        number! {
            doors: i32 = "Doors",
            seats: i32 = "Seats",
            seat_rows: i32 = "SeatRows",
            wheels: i32 = "Wheels",
            engine_cylinders: i32 = "EngineCylinders",
            transmission_speeds: i32 = "TransmissionSpeeds",
            axles: i32 = "Axles",
            engine_hp: f64 = "EngineHP",
            displacement_l: f64 = "DisplacementL",
            displacement_cc: f64 = "DisplacementCC",
            displacement_ci: f64 = "DisplacementCI",
            wheel_base_in: f64 = "WheelBaseShort",
        }

        self.body_style = self.body_class.as_deref().map(BodyStyle::classify);
    }

    /// Whether the powertrain carries any electrification, hybrid included.
    ///
    /// # Examples
    ///
    /// ```
    /// use corgi_rs::VehicleInfo;
    /// let mut info = VehicleInfo::default();
    /// assert!(!info.is_electrified());
    /// info.electrification_level = Some("PHEV".to_string());
    /// assert!(info.is_electrified());
    /// ```
    pub fn is_electrified(&self) -> bool {
        if self
            .electrification_level
            .as_deref()
            .is_some_and(|level| !level.eq_ignore_ascii_case("not applicable"))
        {
            return true;
        }

        [
            self.fuel_type.as_deref(),
            self.fuel_type_secondary.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|fuel| fuel.to_ascii_lowercase().contains("electric"))
    }

    /// Whether the record looks like a passenger vehicle rather than a
    /// motorcycle, bus, trailer or piece of equipment.
    pub fn is_passenger_vehicle(&self) -> bool {
        matches!(
            self.vehicle_type.as_deref(),
            Some("Passenger Car" | "Truck" | "Multipurpose Passenger Vehicle (MPV)")
        )
    }

    /// How many elements the VIN resolved to. Useful for comparing decoders.
    pub fn attribute_count(&self) -> usize {
        self.attributes.len()
    }

    /// Store `value` under `code`, dropping values NHTSA uses to mean "nothing
    /// known".
    pub(crate) fn set_attribute(&mut self, element_id: u16, value: &str) {
        let Some(code) = element::code_of(element_id) else {
            return;
        };
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("not applicable") {
            return;
        }
        self.attributes.insert(code, value.to_string());
    }
}

/// Reduce a label to letters and digits, so punctuation and spacing changes in
/// the source vocabulary cannot break a lookup.
fn squash(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Normalised body style.
///
/// vPIC publishes 70-odd body classes, most of them motorcycle and off-road
/// subdivisions. This collapses them to the shapes a car listing cares about;
/// [`VehicleInfo::body_class`] keeps NHTSA's original wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
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
    Limousine,
    /// A chassis or cutaway sold to a second-stage manufacturer.
    Incomplete,
    Other,
}

impl BodyStyle {
    /// Classify an NHTSA `BodyClass` string.
    ///
    /// Matching ignores punctuation and spacing, because NHTSA reformats the
    /// vocabulary without warning: the August 2026 release turned
    /// `Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)` into
    /// `Sport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]`. Both forms,
    /// and this type's own `Display` output, land on the same variant.
    ///
    /// # Examples
    ///
    /// ```
    /// use corgi_rs::BodyStyle;
    /// assert_eq!(BodyStyle::classify("Sedan/Saloon"), BodyStyle::Sedan);
    /// assert_eq!(
    ///     BodyStyle::classify("Sport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]"),
    ///     BodyStyle::Suv,
    /// );
    /// assert_eq!(BodyStyle::classify("Incomplete - Cutaway"), BodyStyle::Incomplete);
    /// assert_eq!(BodyStyle::classify("Motorcycle - Cruiser"), BodyStyle::Motorcycle);
    /// ```
    pub fn classify(body_class: &str) -> BodyStyle {
        let key = squash(body_class);

        // The vPIC BodyStyle table, which is a closed set.
        let exact = match key.as_str() {
            "sedansaloon" => Some(Self::Sedan),
            "coupe" => Some(Self::Coupe),
            "convertiblecabriolet" | "roadster" => Some(Self::Convertible),
            "hatchbackliftbacknotchback" => Some(Self::Hatchback),
            "wagon" => Some(Self::Wagon),
            "sportutilityvehiclesuvmultipurposevehiclempv" | "crossoverutilityvehiclecuv" => {
                Some(Self::Suv)
            }
            // A sport utility truck has a bed, so it reads as a pickup.
            "sportutilitytrucksut" | "pickup" => Some(Self::Pickup),
            "minivan" => Some(Self::Minivan),
            "van" | "cargovan" | "stepvanwalkinvan" => Some(Self::Van),
            "truck" => Some(Self::Truck),
            "trucktractor" => Some(Self::Tractor),
            "trailer" => Some(Self::Trailer),
            "limousine" => Some(Self::Limousine),
            "streetcartrolley" => Some(Self::Bus),
            _ => None,
        };
        if let Some(style) = exact {
            return style;
        }

        // Accept this type's own `Display` output, so a classification that has
        // been stored as text and read back round-trips.
        if let Some(style) = [
            Self::Sedan,
            Self::Coupe,
            Self::Convertible,
            Self::Hatchback,
            Self::Wagon,
            Self::Suv,
            Self::Van,
            Self::Minivan,
            Self::Pickup,
            Self::Truck,
            Self::Trailer,
            Self::Tractor,
            Self::Bus,
            Self::Motorcycle,
            Self::Limousine,
            Self::Incomplete,
            Self::Other,
        ]
        .into_iter()
        .find(|style| squash(&style.to_string()) == key)
        {
            return style;
        }

        // Families NHTSA prefixes consistently.
        if key.starts_with("motorcycle") {
            return Self::Motorcycle;
        }
        if key.starts_with("incomplete") {
            return Self::Incomplete;
        }
        if key.starts_with("bus") {
            return Self::Bus;
        }

        Self::Other
    }

    /// Whether this is a shape a car buyer would recognise, as opposed to a
    /// motorcycle, trailer or commercial chassis.
    pub fn is_car_like(&self) -> bool {
        matches!(
            self,
            Self::Sedan
                | Self::Coupe
                | Self::Convertible
                | Self::Hatchback
                | Self::Wagon
                | Self::Suv
                | Self::Van
                | Self::Minivan
                | Self::Pickup
                | Self::Limousine
        )
    }
}

impl From<&str> for BodyStyle {
    fn from(value: &str) -> Self {
        Self::classify(value)
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
            Self::Limousine => "Limousine",
            Self::Incomplete => "Incomplete",
            Self::Other => "Other",
        };

        write!(f, "{text}")
    }
}

/// Normalize a raw `BodyClass` string to a [`BodyStyle`].
///
/// # Examples
///
/// ```
/// use corgi_rs::{BodyStyle, extract_body_style};
/// assert_eq!(extract_body_style("Sedan/Saloon"), BodyStyle::Sedan);
/// assert_eq!(extract_body_style("Pickup"), BodyStyle::Pickup);
/// ```
pub fn extract_body_style(raw_body_style: &str) -> BodyStyle {
    BodyStyle::classify(raw_body_style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_covers_the_common_car_shapes() {
        for (raw, expected) in [
            ("Sedan/Saloon", BodyStyle::Sedan),
            ("Coupe", BodyStyle::Coupe),
            ("Convertible/Cabriolet", BodyStyle::Convertible),
            ("Roadster", BodyStyle::Convertible),
            ("Hatchback/Liftback/Notchback", BodyStyle::Hatchback),
            ("Wagon", BodyStyle::Wagon),
            ("Crossover Utility Vehicle (CUV)", BodyStyle::Suv),
            ("Minivan", BodyStyle::Minivan),
            ("Cargo Van", BodyStyle::Van),
            ("Pickup", BodyStyle::Pickup),
            ("Sport Utility Truck (SUT)", BodyStyle::Pickup),
            ("Limousine", BodyStyle::Limousine),
        ] {
            assert_eq!(BodyStyle::classify(raw), expected, "{raw}");
        }
    }

    #[test]
    fn both_spellings_nhtsa_has_used_map_to_the_same_style() {
        // NHTSA reformatted its body-class vocabulary in the August 2026
        // release. Decoding must not depend on which release the assets came
        // from.
        for (before, after) in [
            (
                "Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)",
                "Sport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]",
            ),
            (
                "Crossover Utility Vehicle (CUV)",
                "Crossover Utility Vehicle [CUV]",
            ),
            ("Sport Utility Truck (SUT)", "Sport Utility Truck [SUT]"),
        ] {
            assert_eq!(
                BodyStyle::classify(before),
                BodyStyle::classify(after),
                "{after}"
            );
            assert_ne!(BodyStyle::classify(after), BodyStyle::Other, "{after}");
        }
    }

    #[test]
    fn classify_collapses_the_prefixed_families() {
        assert_eq!(
            BodyStyle::classify("Motorcycle - Scooter"),
            BodyStyle::Motorcycle
        );
        assert_eq!(BodyStyle::classify("Bus - School Bus"), BodyStyle::Bus);
        assert_eq!(
            BodyStyle::classify("Incomplete - Chassis Cab (Single Cab)"),
            BodyStyle::Incomplete
        );
        assert_eq!(
            BodyStyle::classify("Off-road Vehicle - Snowmobile"),
            BodyStyle::Other
        );
    }

    #[test]
    fn classify_is_deterministic_for_ambiguous_looking_input() {
        // "Truck" is a substring of "Truck-Tractor"; exact matching keeps them apart.
        assert_eq!(BodyStyle::classify("Truck"), BodyStyle::Truck);
        assert_eq!(BodyStyle::classify("Truck-Tractor"), BodyStyle::Tractor);
        for _ in 0..64 {
            assert_eq!(
                BodyStyle::classify("Hatchback/Liftback/Notchback"),
                BodyStyle::Hatchback
            );
        }
    }

    #[test]
    fn classify_round_trips_its_own_display_output() {
        for style in [
            BodyStyle::Sedan,
            BodyStyle::Coupe,
            BodyStyle::Convertible,
            BodyStyle::Hatchback,
            BodyStyle::Wagon,
            BodyStyle::Suv,
            BodyStyle::Van,
            BodyStyle::Minivan,
            BodyStyle::Pickup,
            BodyStyle::Truck,
            BodyStyle::Trailer,
            BodyStyle::Tractor,
            BodyStyle::Bus,
            BodyStyle::Motorcycle,
            BodyStyle::Limousine,
            BodyStyle::Incomplete,
            BodyStyle::Other,
        ] {
            assert_eq!(BodyStyle::classify(&style.to_string()), style, "{style}");
        }
    }

    #[test]
    fn unknown_body_classes_fall_through_to_other() {
        assert_eq!(BodyStyle::classify(""), BodyStyle::Other);
        assert_eq!(BodyStyle::classify("Spaceship"), BodyStyle::Other);
    }

    #[test]
    fn a_generation_knows_the_years_it_covers() {
        let closed = Generation {
            ordinal: Some(10),
            code: Some("FC/FK".to_string()),
            name: "10th generation".to_string(),
            year_from: 2016,
            year_to: Some(2021),
        };
        assert!(closed.covers(2016) && closed.covers(2021));
        assert!(!closed.covers(2015) && !closed.covers(2022));
        assert_eq!(closed.span(2026), 6);
        assert_eq!(closed.to_string(), "10th generation (2016-2021)");

        let current = Generation {
            ordinal: Some(11),
            code: None,
            name: "11th generation".to_string(),
            year_from: 2022,
            year_to: None,
        };
        assert!(current.covers(2030));
        assert_eq!(current.span(2026), 5);
        assert_eq!(current.to_string(), "11th generation (2022-)");
    }

    #[test]
    fn set_attribute_drops_placeholders() {
        let mut info = VehicleInfo::default();
        info.set_attribute(element::DRIVE_TYPE, "Not Applicable");
        info.set_attribute(element::SEATS, "  ");
        info.set_attribute(element::DOORS, " 4 ");
        assert_eq!(info.attributes.len(), 1);
        assert_eq!(info.get("Doors"), Some("4"));
        assert_eq!(info.get_i32("Doors"), Some(4));
    }

    #[test]
    fn is_electrified_reads_both_the_level_and_the_fuel_types() {
        let mut info = VehicleInfo::default();
        assert!(!info.is_electrified());

        info.fuel_type = Some("Gasoline".to_string());
        assert!(!info.is_electrified());

        info.fuel_type_secondary = Some("Electric".to_string());
        assert!(info.is_electrified());

        let bev = VehicleInfo {
            electrification_level: Some("BEV".to_string()),
            ..Default::default()
        };
        assert!(bev.is_electrified());

        // NHTSA spells "this is not a hybrid" as a value, not as an absence.
        let plain = VehicleInfo {
            electrification_level: Some("Not Applicable".to_string()),
            ..Default::default()
        };
        assert!(!plain.is_electrified());
    }
}
