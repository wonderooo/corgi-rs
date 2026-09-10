//! Put auction listings and decoded VINs into the same vocabulary.
//!
//! Copart and IAAI describe a car in their own words — `CHEV`, `FOUR BY FOUR`,
//! `SUV` — while vPIC uses NHTSA's, `Chevrolet`, `4WD/4-Wheel Drive/4x4`,
//! `Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)`. Comparing them
//! raw would score a correct decode as wrong, so both sides are folded to a
//! canonical form first.
//!
//! Everything here is deliberately conservative: when a mapping is uncertain the
//! value becomes `None` and the row is counted as "no ground truth" rather than
//! as a disagreement.

use corgi_rs::BodyStyle;

/// Uppercase and strip everything that is not a letter or digit.
///
/// This alone reconciles `MERCEDES BENZ`, `Mercedes-Benz` and `MERCEDES-BENZ`.
pub fn squash(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Auction make abbreviations, keyed by their squashed form.
///
/// Copart truncates makes to four characters in some feeds and appends dealer
/// wording in others.
const MAKE_ALIASES: &[(&str, &str)] = &[
    ("CHEV", "CHEVROLET"),
    ("NISS", "NISSAN"),
    ("TOYT", "TOYOTA"),
    ("HOND", "HONDA"),
    ("DODG", "DODGE"),
    ("HYUN", "HYUNDAI"),
    ("JEP", "JEEP"),
    ("CHRY", "CHRYSLER"),
    ("LEXS", "LEXUS"),
    ("MAZD", "MAZDA"),
    ("VOLV", "VOLVO"),
    ("VOLK", "VOLKSWAGEN"),
    ("INFI", "INFINITI"),
    ("MERZ", "MERCEDESBENZ"),
    ("MERC", "MERCEDESBENZ"),
    ("RAMTRUCKS", "RAM"),
    ("LUCIDMOTORS", "LUCID"),
    ("LINCOLNTOWNHOUSE", "LINCOLN"),
    ("MCLARENAUTOMOTIVE", "MCLAREN"),
    ("FISKERINC", "FISKER"),
    ("BMWMOTORRAD", "BMW"),
    ("TRIUMPHCAR", "TRIUMPH"),
    ("LANDROVER", "LANDROVER"),
    ("ROLLSROYCE", "ROLLSROYCE"),
    ("MINICOOPER", "MINI"),
    ("VW", "VOLKSWAGEN"),
    ("MERCEDES", "MERCEDESBENZ"),
];

/// Canonical make, or `None` when the listing did not name one.
pub fn make(value: &str) -> Option<String> {
    let squashed = squash(value);
    if squashed.is_empty() {
        return None;
    }
    let canonical = MAKE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == squashed)
        .map(|(_, canonical)| (*canonical).to_string())
        .unwrap_or(squashed);
    Some(canonical)
}

/// Canonical model. Auction models frequently carry the body or trim along with
/// the model itself (`G70 BASE`, `1500 CREW CAB`), which [`model_agrees`] deals
/// with; this only normalises the spelling.
pub fn model(value: &str) -> Option<String> {
    let squashed = squash(value);
    (!squashed.is_empty()).then_some(squashed)
}

/// Whether a decoded model is consistent with what the auction listed.
///
/// Exact agreement is the headline number. `relaxed` additionally accepts the
/// listing prefixing or suffixing the model with words vPIC keeps in `Series`
/// or `Trim` — `SORENTO LX` against `Sorento`, or `G70 BASE` against `G70`.
pub fn model_agrees(auction: &str, decoded: &str, relaxed: bool) -> bool {
    let (a, d) = (squash(auction), squash(decoded));
    if a.is_empty() || d.is_empty() {
        return false;
    }
    if a == d {
        return true;
    }
    relaxed && (a.starts_with(&d) || d.starts_with(&a) || a.contains(&d) || d.contains(&a))
}

/// Body style as the auction describes it.
///
/// `AUTOMOBILE` is Copart's catch-all for "a car of some kind" and covers
/// roughly half the listings; it says nothing about the shape, so it yields
/// `None` rather than a guess.
pub fn body_style(value: &str) -> Option<BodyStyle> {
    match squash(value).as_str() {
        "SUV" | "SPORTUTILITYVEHICLE" => Some(BodyStyle::Suv),
        "SEDAN" => Some(BodyStyle::Sedan),
        "PICKUP" | "PICKUPTRUCK" => Some(BodyStyle::Pickup),
        "HATCHBACK" => Some(BodyStyle::Hatchback),
        "MINIVAN" => Some(BodyStyle::Minivan),
        "COUPE" => Some(BodyStyle::Coupe),
        "VAN" | "CARGOVAN" | "PASSENGERVAN" => Some(BodyStyle::Van),
        "CONVERTIBLE" => Some(BodyStyle::Convertible),
        "WAGON" => Some(BodyStyle::Wagon),
        "TRUCK" => Some(BodyStyle::Truck),
        // "MEDIUM DUTY/BOX TRUCKS" and "HEAVY DUTY TRUCKS" are weight classes,
        // not shapes: the same category holds box trucks, cutaway vans and bare
        // chassis cabs. Not usable as body-style ground truth.
        "MEDIUMDUTYBOXTRUCKS" | "HEAVYDUTYTRUCKS" => None,
        "MOTORCYCLE" => Some(BodyStyle::Motorcycle),
        "BUS" => Some(BodyStyle::Bus),
        "TRAILER" => Some(BodyStyle::Trailer),
        _ => None,
    }
}

/// Whether two body styles agree, allowing for the distinctions the auctions do
/// not draw.
///
/// Copart files every crossover, minivan-shaped MPV and three-row wagon as
/// `SUV`, and vPIC's `Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)`
/// covers the same ground, so SUV/Minivan/Van/Wagon confusions between the two
/// are not decoder errors. Sedan against pickup would be.
pub fn body_agrees(auction: BodyStyle, decoded: BodyStyle, relaxed: bool) -> bool {
    if auction == decoded {
        return true;
    }
    if !relaxed {
        return false;
    }

    let family = |style: BodyStyle| match style {
        BodyStyle::Suv | BodyStyle::Minivan | BodyStyle::Van | BodyStyle::Wagon => 1,
        BodyStyle::Sedan | BodyStyle::Hatchback => 2,
        BodyStyle::Coupe | BodyStyle::Convertible => 3,
        BodyStyle::Pickup | BodyStyle::Truck => 4,
        other => 100 + other as u8 as i32,
    };
    family(auction) == family(decoded)
}

/// Fuel, reduced to the categories both sides express.
///
/// `HYBRID` is an auction-only category: vPIC records the fuel a hybrid burns
/// under `FuelTypePrimary` and its electrification separately, so hybrids are
/// compared through [`is_hybrid_listing`] instead.
pub fn fuel(value: &str) -> Option<Fuel> {
    let v = squash(value);
    if v.is_empty() || v == "UNKNOWN" || v == "OTHER" || v == "NONE" {
        return None;
    }
    // Order matters. "ELECTRIC AND GAS HYBRID" contains all three of ELECTRIC,
    // GAS and HYBRID, and "NATURAL GAS" contains GAS.
    if v.contains("HYBRID") {
        return None; // handled by is_hybrid_listing
    }
    if v.contains("HYDROGEN") || v.contains("FUELCELL") {
        return Some(Fuel::Hydrogen);
    }
    if v.contains("NATURALGAS") || v.contains("CNG") || v.contains("LNG") {
        return Some(Fuel::NaturalGas);
    }
    if v.contains("PROPANE") || v.contains("LPG") {
        return Some(Fuel::Lpg);
    }
    if v.contains("FLEX") || v.contains("E85") || v.contains("M85") || v.contains("ETHANOL") {
        return Some(Fuel::Flex);
    }
    if v.contains("DIESEL") {
        return Some(Fuel::Diesel);
    }
    if v.contains("ELECTRIC") {
        return Some(Fuel::Electric);
    }
    if v.contains("GAS") || v == "PETROL" {
        return Some(Fuel::Gasoline);
    }
    None
}

/// Whether the listing's fuel and the decode's agree.
///
/// Strictly, the decode's `FuelTypePrimary` has to match the category the
/// listing named. `relaxed` also accepts a disagreement between petrol and
/// flex-fuel, because neither source records E85 capability reliably: Copart
/// calls E85-capable Silverados `GAS` while vPIC gives them an `Ethanol (E85)`
/// secondary, and calls E85-capable F-150s petrol while Copart calls them
/// `FLEXIBLE`. Both are petrol cars either way.
pub fn fuel_agrees(
    expected: Fuel,
    primary: Option<&str>,
    secondary: Option<&str>,
    relaxed: bool,
) -> bool {
    let decoded = primary.and_then(fuel);
    if decoded == Some(expected) {
        return true;
    }
    if !relaxed {
        return false;
    }

    let petrol_family = |value: Option<Fuel>| matches!(value, Some(Fuel::Gasoline | Fuel::Flex));
    if petrol_family(Some(expected)) && petrol_family(decoded) {
        return true;
    }

    // A petrol engine with an E85 secondary really is a flex-fuel vehicle.
    expected == Fuel::Flex && secondary.and_then(fuel) == Some(Fuel::Flex)
}

/// Whether the auction called this a hybrid.
///
/// Copart spells it `ELECTRIC AND GAS HYBRID` or `HYBRID ENGINE`.
pub fn is_hybrid_listing(value: &str) -> bool {
    squash(value).contains("HYBRID")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fuel {
    Gasoline,
    Diesel,
    Electric,
    Flex,
    NaturalGas,
    Lpg,
    Hydrogen,
}

/// Driven wheels.
///
/// vPIC's `4x2` is *not* rear-wheel drive: it means two of four wheels are
/// driven and says nothing about which axle. Roughly a fifth of the fleet
/// decodes to it, so mapping it onto `Rear` would invent a hundred thousand
/// disagreements that are really an absence of information.
pub fn drive(value: &str) -> Option<Drive> {
    let v = squash(value);
    if v.is_empty() || v == "UNKNOWN" || v == "OTHER" {
        return None;
    }
    // Four-wheel drive first: Copart writes it as `4X4 W/REAR WHEEL DRV`, which
    // also contains REAR.
    if v.contains("4X4") || v.contains("FOURBYFOUR") || v.contains("FOURWHEEL") || v.contains("4WD")
    {
        return Some(Drive::Four);
    }
    if v.contains("ALLWHEEL") || v == "AWD" {
        return Some(Drive::All);
    }
    if v.contains("FRONT") || v == "FWD" {
        return Some(Drive::Front);
    }
    if v.contains("REAR") || v == "RWD" {
        return Some(Drive::Rear);
    }
    if v == "4X2" {
        return Some(Drive::TwoWheel);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drive {
    Front,
    Rear,
    All,
    Four,
    /// Two driven wheels, axle unspecified -- vPIC's `4x2`.
    TwoWheel,
}

/// Whether two drive types agree.
///
/// A bare `4x2` is consistent with both front- and rear-wheel drive, so it
/// counts as agreement with either rather than as an error. `relaxed`
/// additionally merges all-wheel and four-wheel drive, which listings use
/// interchangeably even though vPIC does not.
pub fn drive_agrees(auction: Drive, decoded: Drive, relaxed: bool) -> bool {
    if auction == decoded {
        return true;
    }

    let two_wheel = |drive| matches!(drive, Drive::Front | Drive::Rear | Drive::TwoWheel);
    if (auction == Drive::TwoWheel || decoded == Drive::TwoWheel)
        && two_wheel(auction)
        && two_wheel(decoded)
    {
        return true;
    }

    relaxed
        && matches!(
            (auction, decoded),
            (Drive::All, Drive::Four) | (Drive::Four, Drive::All)
        )
}

/// Transmission, reduced to what the auctions record.
///
/// Everything that shifts itself — CVT, dual-clutch, e-CVT — is an automatic as
/// far as a listing is concerned. An automated manual is not: it is a manual
/// gearbox with an actuator, and the auctions call those manual.
pub fn transmission(value: &str) -> Option<Transmission> {
    let v = squash(value);
    if v.is_empty() || v == "UNKNOWN" || v == "NONE" {
        return None;
    }
    // An automated manual is a manual gearbox with an actuator, and both
    // vocabularies call it manual -- so check for it before the AUTO prefix.
    if v.contains("MANUAL") || v == "STD" {
        return Some(Transmission::Manual);
    }
    if v.contains("AUTO") || v.contains("CVT") || v.contains("DUALCLUTCH") || v == "DIRECTDRIVE" {
        return Some(Transmission::Automatic);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transmission {
    Automatic,
    Manual,
}

/// Engine displacement in litres from an auction engine description such as
/// `3.5L  6` or `6.7L  8`.
///
/// Unlike `fuel_type`, this field is transcribed from the build sheet and is
/// reliable, which makes it a good independent check on the decode.
pub fn displacement_l(engine_name: &str) -> Option<f64> {
    let text = engine_name.trim();
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let litres: f64 = digits.parse().ok()?;
    // Anything outside this range is a cylinder count or a typo, not a car engine.
    (0.5..=9.0).contains(&litres).then_some(litres)
}

/// Whether two displacements agree to within a tenth of a litre, which is the
/// precision both sides publish.
pub fn displacement_agrees(auction: f64, decoded: f64) -> bool {
    (auction - decoded).abs() < 0.05
}

/// Cylinder count from a free-text auction field such as `8` or `8 Cylinders`.
pub fn cylinders(value: &str) -> Option<i32> {
    let digits: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|count| (1..=16).contains(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squash_reconciles_punctuation_and_case() {
        assert_eq!(squash("Mercedes-Benz"), "MERCEDESBENZ");
        assert_eq!(squash("MERCEDES BENZ"), "MERCEDESBENZ");
        assert_eq!(squash("  land rover "), "LANDROVER");
    }

    #[test]
    fn make_expands_the_auction_abbreviations() {
        assert_eq!(make("CHEV").as_deref(), Some("CHEVROLET"));
        assert_eq!(make("Chevrolet").as_deref(), Some("CHEVROLET"));
        assert_eq!(make("RAM TRUCKS").as_deref(), Some("RAM"));
        assert_eq!(make("Ram").as_deref(), Some("RAM"));
        assert_eq!(make("   ").as_deref(), None);
    }

    #[test]
    fn model_agreement_is_strict_by_default() {
        assert!(model_agrees("SORENTO", "Sorento", false));
        assert!(!model_agrees("G70 BASE", "G70", false));
        assert!(model_agrees("G70 BASE", "G70", true));
        assert!(model_agrees("1500", "1500 Classic", true));
        assert!(!model_agrees("CAMRY", "Corolla", true));
    }

    #[test]
    fn body_agreement_forgives_the_suv_family_only_when_relaxed() {
        assert!(body_agrees(BodyStyle::Suv, BodyStyle::Suv, false));
        assert!(!body_agrees(BodyStyle::Suv, BodyStyle::Minivan, false));
        assert!(body_agrees(BodyStyle::Suv, BodyStyle::Minivan, true));
        // A sedan decoded as a pickup is wrong under any reading.
        assert!(!body_agrees(BodyStyle::Sedan, BodyStyle::Pickup, true));
    }

    #[test]
    fn drive_agreement_can_merge_awd_and_4wd() {
        assert!(drive_agrees(Drive::All, Drive::All, false));
        assert!(!drive_agrees(Drive::All, Drive::Four, false));
        assert!(drive_agrees(Drive::All, Drive::Four, true));
        assert!(!drive_agrees(Drive::Front, Drive::Rear, true));
    }

    #[test]
    fn two_wheel_drive_is_consistent_with_either_axle() {
        // vPIC often knows only that two wheels are driven.
        assert_eq!(drive("4x2"), Some(Drive::TwoWheel));
        assert!(drive_agrees(Drive::Front, Drive::TwoWheel, false));
        assert!(drive_agrees(Drive::Rear, Drive::TwoWheel, false));
        // But it does contradict a driven rear axle plus a driven front one.
        assert!(!drive_agrees(Drive::All, Drive::TwoWheel, true));
        assert!(!drive_agrees(Drive::Four, Drive::TwoWheel, true));
    }

    #[test]
    fn both_vocabularies_map_onto_the_same_categories() {
        // Auction wording and NHTSA wording have to land on the same value.
        assert_eq!(drive("FOUR BY FOUR"), drive("4WD/4-Wheel Drive/4x4"));
        assert_eq!(drive("FRONT WHEEL DRIVE"), drive("FWD/Front-Wheel Drive"));
        assert_eq!(fuel("FLEX FUEL"), fuel("Flexible Fuel Vehicle (FFV)"));
        assert_eq!(
            fuel("LPG"),
            fuel("Liquefied Petroleum Gas (propane or LPG)")
        );
        assert_eq!(transmission("AUTOMATIC"), transmission("Automatic"));
        assert_eq!(transmission("MANUAL"), transmission("Manual/Standard"));
        assert_eq!(
            transmission("AUTOMATIC"),
            transmission("Continuously Variable Transmission (CVT)")
        );
    }

    #[test]
    fn the_raw_auction_vocabulary_maps_correctly() {
        // These are the strings Copart and IAAI actually publish, before the
        // app normalises them.
        assert_eq!(drive("4X4 W/REAR WHEEL DRV"), Some(Drive::Four));
        assert_eq!(drive("4X4 W/FRONT WHL DRV"), Some(Drive::Four));
        assert_eq!(drive("Front-wheel Drive"), Some(Drive::Front));
        assert_eq!(drive("Rear-wheel drive"), Some(Drive::Rear));
        assert_eq!(fuel("GAS"), Some(Fuel::Gasoline));
        assert_eq!(fuel("FLEXIBLE"), Some(Fuel::Flex));
        assert!(is_hybrid_listing("ELECTRIC AND GAS HYBRID"));
        assert!(is_hybrid_listing("HYBRID ENGINE"));
        // A hybrid is not a plain fuel; it is compared separately.
        assert_eq!(fuel("ELECTRIC AND GAS HYBRID"), None);
        assert_eq!(fuel("NATURAL GAS"), Some(Fuel::NaturalGas));
        assert_eq!(transmission("STD"), Some(Transmission::Manual));
    }

    #[test]
    fn fuel_agreement_is_strict_about_everything_but_e85() {
        assert!(fuel_agrees(Fuel::Gasoline, Some("Gasoline"), None, false));
        assert!(!fuel_agrees(Fuel::Diesel, Some("Gasoline"), None, true));
        // Petrol against flex-fuel is a recording difference, not a wrong fuel.
        assert!(!fuel_agrees(Fuel::Flex, Some("Gasoline"), None, false));
        assert!(fuel_agrees(Fuel::Flex, Some("Gasoline"), None, true));
        assert!(fuel_agrees(
            Fuel::Flex,
            Some("Gasoline"),
            Some("Ethanol (E85)"),
            true
        ));
        assert!(!fuel_agrees(Fuel::Electric, Some("Gasoline"), None, true));
    }

    #[test]
    fn copart_catch_all_categories_are_not_ground_truth() {
        // Half of Copart's listings say only "AUTOMOBILE".
        assert_eq!(body_style("AUTOMOBILE"), None);
        assert_eq!(body_style("RECREATIONAL VEHICLE (RV)"), None);
        assert_eq!(body_style("MEDIUM DUTY/BOX TRUCKS"), None);
        assert_eq!(body_style("HEAVY DUTY TRUCKS"), None);
        assert_eq!(fuel("UNKNOWN"), None);
        assert_eq!(transmission("UNKNOWN"), None);
        assert_eq!(transmission("NONE"), None);
    }

    #[test]
    fn unmapped_values_yield_no_ground_truth_rather_than_a_guess() {
        assert_eq!(drive("SOMETHING ELSE"), None);
        assert_eq!(fuel(""), None);
        assert_eq!(body_style("LIMOUSINE"), None);
        assert_eq!(transmission("Motorcycle - Shaft Drive"), None);
    }

    #[test]
    fn displacement_reads_the_leading_litres() {
        assert_eq!(displacement_l("3.5L  6"), Some(3.5));
        assert_eq!(displacement_l("6.7L  8"), Some(6.7));
        assert_eq!(displacement_l(""), None);
        // A bare cylinder count is not a displacement.
        assert_eq!(displacement_l("12"), None);
    }

    #[test]
    fn displacement_agreement_tolerates_published_rounding() {
        assert!(displacement_agrees(3.5, 3.5));
        assert!(displacement_agrees(3.5, 3.53));
        assert!(!displacement_agrees(3.5, 3.6));
    }

    #[test]
    fn cylinders_reads_the_leading_number_only() {
        assert_eq!(cylinders("8"), Some(8));
        assert_eq!(cylinders("6 Cylinders"), Some(6));
        assert_eq!(cylinders("V8"), None);
        assert_eq!(cylinders("99"), None);
    }
}
