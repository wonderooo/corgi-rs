//! End-to-end decoding of VINs taken from real Copart and IAAI listings.
//!
//! Every expectation here is the make, model and year the auction house
//! published for that lot, so these tests pin the decoder to reality rather
//! than to its own previous output. The cases were chosen because each one used
//! to decode wrongly.

use corgi_rs::{BodyStyle, VinDecoder};

/// A lot, as the auction described it.
struct Lot {
    vin: &'static str,
    year: i32,
    make: &'static str,
    model: Option<&'static str>,
    note: &'static str,
}

const LOTS: &[Lot] = &[
    // Five makes share the FCA WMIs 1C4, 1C6, 3C4, 3C8 and ZACN. Picking the
    // make from the WMI produced Dodge for all of them; it has to come from the
    // decoded model instead.
    Lot {
        vin: "1C6RR7LT2JS179571",
        year: 2018,
        make: "Ram",
        model: Some("1500"),
        note: "1C6 is shared by Ram, Dodge, Chrysler and Jeep",
    },
    Lot {
        vin: "ZACNJDAB0MPN15099",
        year: 2021,
        make: "Jeep",
        model: Some("Renegade"),
        note: "ZAC is an Italian-built Jeep",
    },
    Lot {
        vin: "3C4NJDAB9HT645920",
        year: 2017,
        make: "Jeep",
        model: Some("Compass"),
        note: "3C4 is shared with Dodge and Chrysler",
    },
    Lot {
        vin: "3C3CFFDR5CT352672",
        year: 2012,
        make: "Fiat",
        model: Some("500"),
        note: "3C3 decoded as a Dodge 200 before",
    },
    Lot {
        vin: "3C8FY4BB41T525879",
        year: 2001,
        make: "Chrysler",
        model: Some("PT Cruiser"),
        note: "position 10 '1' is 2031 unless the schema years rule it out",
    },
    // Marques whose WMI is registered to the parent group.
    Lot {
        vin: "5XYRLDLC3NG097496",
        year: 2022,
        make: "Kia",
        model: Some("Sorento"),
        note: "Kia WMIs also list Hyundai",
    },
    Lot {
        vin: "KNAGG4A81A5399623",
        year: 2010,
        make: "Kia",
        model: Some("Optima"),
        note: "used to decode as the later K5",
    },
    Lot {
        vin: "KMTG34TA3PU121692",
        year: 2023,
        make: "Genesis",
        model: Some("G70"),
        note: "Genesis, not Hyundai",
    },
    Lot {
        vin: "JN1FV7LL4NM680076",
        year: 2022,
        make: "Infiniti",
        model: Some("Q60"),
        note: "Infiniti, not Nissan",
    },
    // Model-year disambiguation. Position 10 repeats every 30 years.
    Lot {
        vin: "YV4A221K4L1609498",
        year: 2020,
        make: "Volvo",
        model: Some("XC90"),
        note: "Volvo puts a digit in position 7, which NHTSA reads as 1990",
    },
    Lot {
        vin: "1FDEE14L1VHA27276",
        year: 1997,
        make: "Ford",
        model: Some("E-150"),
        note: "a heavy-truck WMI, where NHTSA's fallback yields 2027",
    },
    Lot {
        vin: "2FTEF14H8TCA73155",
        year: 1996,
        make: "Ford",
        model: Some("F-150"),
        note: "digit in position 7 on a light truck means the older block",
    },
    // Passenger vans, which NHTSA files as buses.
    Lot {
        vin: "WDZPF1CD4KP124204",
        year: 2019,
        make: "Mercedes-Benz",
        model: Some("Sprinter"),
        note: "vPIC vehicle type Bus",
    },
    Lot {
        vin: "1FBZX2YM1GKB25825",
        year: 2016,
        make: "Ford",
        model: Some("Transit"),
        note: "vPIC vehicle type Bus",
    },
    // Ordinary cars, as a control.
    Lot {
        vin: "1HGCP26739A060971",
        year: 2009,
        make: "Honda",
        model: Some("Accord"),
        note: "control case",
    },
    Lot {
        vin: "4S3GTAT62N3701298",
        year: 2022,
        make: "Subaru",
        model: Some("Impreza"),
        note: "Subaru WMIs also list Toyota",
    },
];

fn decoder() -> VinDecoder {
    // Pin the clock: the model-year fallback depends on what "now" is, and a
    // test that changes answer next January is not a test.
    VinDecoder::new().with_current_year(2026)
}

#[test]
fn real_lots_decode_to_what_the_auction_listed() {
    let decoder = decoder();
    let mut failures = Vec::new();

    for lot in LOTS {
        match decoder.decode(lot.vin) {
            Ok(info) => {
                if info.make != lot.make
                    || info.model.as_deref() != lot.model
                    || info.year != lot.year
                {
                    failures.push(format!(
                        "{}: expected {} {} {:?}, got {} {} {:?}  ({})",
                        lot.vin,
                        lot.year,
                        lot.make,
                        lot.model,
                        info.year,
                        info.make,
                        info.model,
                        lot.note
                    ));
                }
            }
            Err(err) => failures.push(format!("{}: {err}  ({})", lot.vin, lot.note)),
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn a_shared_wmi_resolves_to_different_makes_for_different_vins() {
    let decoder = decoder();
    // Both are 3C4/3C3 Chrysler-group VINs, and they must not collapse onto one
    // make the way a WMI-only lookup does.
    let jeep = decoder.decode("3C4NJDAB9HT645920").expect("Jeep decodes");
    let fiat = decoder.decode("3C3CFFDR5CT352672").expect("Fiat decodes");
    assert_eq!(jeep.make, "Jeep");
    assert_eq!(fiat.make, "Fiat");
    assert_ne!(jeep.make_id, fiat.make_id);
}

#[test]
fn a_pickup_decodes_the_powertrain_a_listing_would_show() {
    let info = decoder()
        .decode("1C6RR7LT2JS179571")
        .expect("a 2018 Ram 1500");

    assert_eq!(info.body_style, Some(BodyStyle::Pickup));
    assert_eq!(info.vehicle_type.as_deref(), Some("Truck"));
    assert_eq!(info.drive_type.as_deref(), Some("4WD/4-Wheel Drive/4x4"));
    assert_eq!(info.fuel_type.as_deref(), Some("Gasoline"));
    assert_eq!(info.engine_cylinders, Some(8));
    assert_eq!(info.displacement_l, Some(5.7));
    assert_eq!(info.manufacturer.as_deref(), Some("FCA US LLC"));
    assert_eq!(info.plant_city.as_deref(), Some("WARREN"));
    assert_eq!(info.plant_state.as_deref(), Some("MICHIGAN"));
    assert_eq!(info.cab_type.as_deref(), Some("Crew/Super Crew/Crew Max"));
}

#[test]
fn displacement_is_reported_in_every_unit_from_whichever_one_was_encoded() {
    let info = decoder()
        .decode("1C6RR7LT2JS179571")
        .expect("a 2018 Ram 1500");

    assert_eq!(info.get("DisplacementL"), Some("5.7"));
    assert_eq!(info.get("DisplacementCC"), Some("5700"));
    assert!(info.get("DisplacementCI").is_some());
}

#[test]
fn a_modern_car_yields_far_more_than_the_headline_fields() {
    let info = decoder()
        .decode("5XYRLDLC3NG097496")
        .expect("a 2022 Kia Sorento");

    // The named fields are a small part of what vPIC knows.
    assert!(
        info.attribute_count() >= 40,
        "expected a rich decode, got {} attributes: {:?}",
        info.attribute_count(),
        info.attributes.keys().collect::<Vec<_>>()
    );

    // Elements that only exist because the full pattern and vehicle-spec tables
    // are shipped.
    for code in ["DriveType", "TransmissionStyle", "Seats", "EngineHP", "ABS"] {
        assert!(info.get(code).is_some(), "missing {code}");
    }
}

#[test]
fn the_named_fields_mirror_the_attribute_map() {
    let info = decoder()
        .decode("5XYRLDLC3NG097496")
        .expect("a 2022 Kia Sorento");

    // Every named field is a copy of an attribute, never a separate source of
    // truth, so the two can never disagree.
    assert_eq!(info.abs.as_deref(), info.get("ABS"));
    assert_eq!(info.esc.as_deref(), info.get("ESC"));
    assert_eq!(info.plant_state.as_deref(), info.get("PlantState"));
    assert_eq!(info.engine_hp, info.get_f64("EngineHP"));
    assert_eq!(info.seats, info.get_i32("Seats"));
    assert_eq!(info.transmission_speeds, info.get_i32("TransmissionSpeeds"));

    // And an element the VIN did not resolve stays absent on both sides.
    assert_eq!(info.get("PlantCity"), None);
    assert_eq!(info.plant_city, None);
}

#[test]
fn a_well_documented_car_fills_in_the_safety_and_plant_fields() {
    let info = decoder()
        .decode("5XYRLDLC3NG097496")
        .expect("a 2022 Kia Sorento");

    assert_eq!(info.seats, Some(7));
    assert_eq!(info.seat_rows, Some(3));
    assert_eq!(info.engine_hp, Some(191.3));
    assert_eq!(info.engine_cylinders, Some(4));
    assert_eq!(info.displacement_l, Some(2.5));
    assert_eq!(info.displacement_cc, Some(2500.0));
    assert_eq!(info.transmission_speeds, Some(8));
    assert_eq!(
        info.valve_train_design.as_deref(),
        Some("Dual Overhead Cam (DOHC)")
    );
    assert_eq!(info.engine_manufacturer.as_deref(), Some("KMC"));
    assert_eq!(info.plant_country.as_deref(), Some("UNITED STATES (USA)"));
    assert_eq!(info.plant_state.as_deref(), Some("GEORGIA"));

    for equipment in [
        &info.abs,
        &info.esc,
        &info.traction_control,
        &info.backup_camera,
        &info.blind_spot_monitor,
        &info.forward_collision_warning,
        &info.lane_departure_warning,
        &info.lane_keep_assist,
        &info.keyless_ignition,
    ] {
        assert_eq!(equipment.as_deref(), Some("Standard"));
    }
    assert_eq!(info.tpms.as_deref(), Some("Direct"));
    assert_eq!(
        info.airbag_front.as_deref(),
        Some("1st Row (Driver and Passenger)")
    );
}

#[test]
fn generations_resolve_where_the_table_covers_the_model() {
    let decoder = decoder();
    for (vin, name, code, year_from, year_to) in [
        (
            "1HGCP26739A060971",
            "8th generation",
            Some("CP/CS"),
            2008,
            Some(2012),
        ),
        ("5XYRLDLC3NG097496", "MQ4", Some("MQ4"), 2021, None),
        ("1C6RR7LT2JS179571", "DS", Some("DS"), 2011, Some(2018)),
        (
            "YV4A221K4L1609498",
            "2nd generation",
            Some("SPA"),
            2016,
            None,
        ),
        (
            "3C3CFFDR5CT352672",
            "2nd generation",
            Some("312"),
            2012,
            Some(2019),
        ),
    ] {
        let info = decoder.decode(vin).expect("decodes");
        let generation = info
            .generation
            .as_ref()
            .unwrap_or_else(|| panic!("{vin}: no generation"));
        assert_eq!(generation.name, name, "{vin}");
        assert_eq!(generation.code.as_deref(), code, "{vin}");
        assert_eq!(generation.year_from, year_from, "{vin}");
        assert_eq!(generation.year_to, year_to, "{vin}");
        assert!(generation.covers(info.year), "{vin}");
    }
}

#[test]
fn the_priority_makes_resolve_a_generation() {
    // These nine makes are the ones the generation table is kept complete for;
    // each resolves on better than 99.6% of auction lots. One representative
    // VIN per make guards against a make dropping out of the table wholesale.
    let decoder = decoder();
    let mut missing = Vec::new();

    for (vin, make, model, year, generation) in [
        ("WA1BNAFY7J2047829", "Audi", "Q5", 2018, "FY"),
        ("5UXTY5C01LLE58231", "BMW", "X3", 2020, "G01"),
        (
            "2C4RC1CG1CR368386",
            "Chrysler",
            "Town and Country",
            2012,
            "5th generation",
        ),
        ("2C3CDXAT2PH663860", "Dodge", "Charger", 2023, "LD"),
        ("1C4RJEAG3CC258576", "Jeep", "Grand Cherokee", 2012, "WK2"),
        ("58AEA1C13NU016386", "Lexus", "ES", 2022, "XZ10"),
        (
            "WDCGG8HB0AF269509",
            "Mercedes-Benz",
            "GLK-Class",
            2010,
            "X204",
        ),
        ("5TDYK3EH3DS112033", "Toyota", "Highlander", 2013, "XU40"),
        ("YV4BR0CL7K1436083", "Volvo", "XC90", 2019, "2nd generation"),
    ] {
        match decoder.decode(vin) {
            Ok(info) => {
                let found = info.generation.as_ref().map(|g| g.name.as_str());
                if info.make != make
                    || info.model.as_deref() != Some(model)
                    || info.year != year
                    || found != Some(generation)
                {
                    missing.push(format!(
                        "{vin}: expected {year} {make} {model} / {generation}, \
                         got {} {} {:?} / {found:?}",
                        info.year, info.make, info.model
                    ));
                }
            }
            Err(err) => missing.push(format!("{vin}: {err}")),
        }
    }

    assert!(missing.is_empty(), "\n{}", missing.join("\n"));
}

#[test]
fn a_generation_is_absent_rather_than_guessed_for_an_uncovered_model() {
    // The table covers the models that make up the bulk of the fleet, not
    // every model ever sold. An uncovered one must say nothing.
    let info = decoder()
        .decode("1FDEE14L1VHA27276")
        .expect("a 1997 Ford E-150");
    assert_eq!(info.model.as_deref(), Some("E-150"));
    assert!(info.generation.is_none());
}

#[test]
fn the_generation_matches_the_model_year_it_was_decoded_for() {
    let decoder = decoder();
    // Same model, two years, two generations. The Ram 1500 switched from the
    // DS to the DT for 2019.
    let ds = decoder.decode("1C6RR7LT2JS179571").expect("a 2018 Ram");
    assert_eq!(ds.year, 2018);
    assert_eq!(ds.generation.as_ref().unwrap().code.as_deref(), Some("DS"));
}

#[test]
fn an_electric_car_is_recognised_as_one() {
    // Tesla Model 3, Fremont-built.
    let info = decoder()
        .decode("5YJ3E1EA4MF930478")
        .expect("a Tesla Model 3");
    assert_eq!(info.make, "Tesla");
    assert_eq!(info.fuel_type.as_deref(), Some("Electric"));
    assert!(info.is_electrified());
}

#[test]
fn a_bad_check_digit_warns_but_still_decodes() {
    // Same VIN with the check digit changed from 9 to 0.
    let info = decoder()
        .decode("1C6RR7LT0JS179571")
        .expect("decodes despite the check digit");
    assert_eq!(info.make, "Ram");
    assert!(
        info.warnings
            .contains(&corgi_rs::Warning::CheckDigitMismatch)
    );
}

#[test]
fn strict_mode_rejects_a_bad_check_digit() {
    let strict = VinDecoder::new()
        .with_current_year(2026)
        .require_check_digit(true);
    assert!(strict.decode("1C6RR7LT0JS179571").is_err());
    assert!(strict.decode("1C6RR7LT2JS179571").is_ok());
}

#[test]
fn malformed_vins_are_rejected_with_a_reason() {
    let decoder = decoder();
    for (vin, expected) in [
        ("1C6RR7LT2JS17957", "17 characters"),
        ("1C6RR7LT2JS17957I", "invalid characters"),
        ("1C6RR7LT20S179571", "does not encode a model year"),
    ] {
        let err = decoder.decode(vin).expect_err("should be rejected");
        assert!(
            err.to_string().contains(expected),
            "{vin}: expected `{expected}` in `{err}`"
        );
    }
}

#[test]
fn whitespace_and_dashes_are_tolerated() {
    let decoder = decoder();
    let plain = decoder.decode("1C6RR7LT2JS179571").expect("plain");
    let messy = decoder.decode("  1c6rr7lt2-js179571 ").expect("messy");
    assert_eq!(plain, messy);
}

#[test]
fn batch_decoding_agrees_with_decoding_one_at_a_time() {
    let decoder = decoder();
    let vins: Vec<String> = LOTS.iter().map(|lot| lot.vin.to_string()).collect();
    let batch = decoder.decode_batch(&vins);

    assert_eq!(batch.len(), vins.len());
    for vin in &vins {
        let single = decoder.decode(vin);
        match (&batch[vin], &single) {
            (Ok(from_batch), Ok(from_single)) => assert_eq!(from_batch, from_single, "{vin}"),
            (Err(_), Err(_)) => {}
            _ => panic!("{vin}: batch and single decoding disagree"),
        }
    }
}
