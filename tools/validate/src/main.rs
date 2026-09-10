//! Measure how well `corgi-rs` decodes real VINs.
//!
//! Copart and IAAI publish make, model, year, body, fuel, drive and
//! transmission alongside each lot's VIN. That makes the auction table a
//! several-hundred-thousand-row ground truth for a VIN decoder, which is the
//! only honest way to tell whether a change to the pattern matcher helped.
//!
//! ```sh
//! export DATABASE_URL='postgresql://…/neondb?sslmode=require'
//! cargo run --release -p corgi-validate -- --limit 100000 --baseline
//! ```
//!
//! `--baseline` additionally scores the `nhtsa_*` columns already stored on
//! each row, so the run reports the change rather than an absolute number in a
//! vacuum.

mod generations;
mod normalize;
mod report;

use std::collections::HashMap;

use corgi_rs::{BodyStyle, VehicleInfo, VinDecoder};
use native_tls::TlsConnector;
use postgres::{Client, NoTls};
use postgres_native_tls::MakeTlsConnector;
use rayon::prelude::*;

use report::{FieldStats, Report};

/// A lot as the auction described it, plus what the database currently thinks
/// the VIN decodes to.
///
/// The powertrain and body columns come from `lot_vehicle_pre0008_cols` where
/// possible. See [`fetch`] for why that matters.
#[derive(Debug, Clone)]
struct Lot {
    lot_number: i32,
    vin: String,
    make: String,
    model: String,
    series: Option<String>,
    year: i32,
    /// `None` when the listing only said "AUTOMOBILE", or when the only value
    /// available came from a previous decode.
    vehicle_type: Option<String>,
    fuel_type: Option<String>,
    drive_type: Option<String>,
    transmission: Option<String>,
    engine_cylinders: Option<String>,
    engine_name: Option<String>,
    /// Whether the body and powertrain values are the auction's own.
    original_ground_truth: bool,
    stored: StoredDecode,
}

/// The `nhtsa_*` columns, i.e. whatever decoder last wrote to this row.
#[derive(Debug, Clone, Default)]
struct StoredDecode {
    make: Option<String>,
    model: Option<String>,
    year: Option<i32>,
    body_style: Option<String>,
    fuel_type: Option<String>,
    drive_type: Option<String>,
    transmission: Option<String>,
}

struct Args {
    limit: i64,
    offset: i64,
    source: Option<String>,
    mismatches: usize,
    baseline: bool,
    cars_only: bool,
    write: bool,
    sample_failures: usize,
    dump: Option<String>,
    allow_derived: bool,
    check_generations: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            limit: 50_000,
            offset: 0,
            source: None,
            mismatches: 10,
            baseline: false,
            cars_only: false,
            write: false,
            sample_failures: 0,
            dump: None,
            allow_derived: false,
            check_generations: false,
        };

        let mut argv = std::env::args().skip(1);
        while let Some(flag) = argv.next() {
            let mut value = || argv.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--limit" => {
                    args.limit = value()?.parse().map_err(|_| "--limit must be a number")?
                }
                "--offset" => {
                    args.offset = value()?.parse().map_err(|_| "--offset must be a number")?
                }
                "--source" => args.source = Some(value()?),
                "--dump" => args.dump = Some(value()?),
                "--mismatches" => {
                    args.mismatches = value()?
                        .parse()
                        .map_err(|_| "--mismatches must be a number")?
                }
                "--sample-failures" => {
                    args.sample_failures = value()?
                        .parse()
                        .map_err(|_| "--sample-failures must be a number")?
                }
                "--baseline" => args.baseline = true,
                "--allow-derived" => args.allow_derived = true,
                "--check-generations" => args.check_generations = true,
                "--cars-only" => args.cars_only = true,
                "--write" => args.write = true,
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("unknown flag {other}\n\n{}", usage())),
            }
        }

        Ok(args)
    }
}

fn usage() -> String {
    "\
corgi-validate -- score corgi-rs against Copart/IAAI listings

  --limit N            rows to read (default 50000)
  --offset N           skip N rows, for sampling a different slice
  --source NAME        restrict to one auction source, e.g. copart
  --cars-only          drop lots the decoder classifies as non-passenger
  --baseline           also score the nhtsa_* columns already on each row
  --mismatches N       show the N worst disagreements per field (default 10)
  --sample-failures N  print N VINs that failed to decode
  --dump PATH          write every disagreeing row to PATH as CSV
  --allow-derived      score body and powertrain even on rows whose only
                       values were written by a previous decode
  --check-generations  check tools/generations.tsv against the VIN body codes
  --write              write the decode back to the nhtsa_* columns

DATABASE_URL must point at the auction database."
        .to_string()
}

fn main() {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("DATABASE_URL is not set.\n\n{}", usage());
            std::process::exit(2);
        }
    };

    let mut client = match connect(&database_url) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("could not connect to the auction database: {err}");
            std::process::exit(1);
        }
    };

    let lots = match fetch(&mut client, &args) {
        Ok(lots) => lots,
        Err(err) => {
            eprintln!("could not read lots: {err}");
            std::process::exit(1);
        }
    };
    let original = lots.iter().filter(|lot| lot.original_ground_truth).count();
    println!("read {} lots", lots.len());
    println!(
        "  {original} of them ({:.1}%) still have the auction's own body and powertrain values;\n  \
         the rest were overwritten by migration 0008 and are {}.",
        100.0 * original as f64 / lots.len().max(1) as f64,
        if args.allow_derived {
            "scored anyway (--allow-derived)"
        } else {
            "left out of those four fields"
        }
    );

    let decoder = match VinDecoder::try_new() {
        Ok(decoder) => decoder,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };

    let decoded: Vec<(Lot, Result<VehicleInfo, String>)> = lots
        .into_par_iter()
        .map(|lot| {
            let result = decoder.decode(&lot.vin).map_err(|err| err.to_string());
            (lot, result)
        })
        .collect();

    let current = score_current(&decoded, args.cars_only);
    current.print();
    current.print_mismatches(args.mismatches);

    if args.baseline {
        let baseline = score_stored(&decoded, args.cars_only);
        baseline.print();
        baseline.print_mismatches(args.mismatches);
        report::print_comparison(&baseline, &current);
    }

    print_attribute_yield(&decoded);

    if args.check_generations {
        let vins: Vec<(String, Result<VehicleInfo, String>)> = decoded
            .iter()
            .map(|(lot, result)| (lot.vin.clone(), result.clone()))
            .collect();
        generations::check(&vins, &decoder);
    }

    if args.sample_failures > 0 {
        print_failures(&decoded, args.sample_failures);
    }

    if let Some(path) = &args.dump {
        match dump_disagreements(path, &decoded) {
            Ok(count) => println!("\nwrote {count} disagreeing rows to {path}"),
            Err(err) => eprintln!("could not write {path}: {err}"),
        }
    }

    if args.write {
        match write_back(&mut client, &decoded) {
            Ok(updated) => println!("\nwrote {updated} rows back to lot_vehicle"),
            Err(err) => {
                eprintln!("write-back failed: {err}");
                std::process::exit(1);
            }
        }
    }
}

/// Neon requires TLS; a plain local Postgres does not.
fn connect(database_url: &str) -> Result<Client, Box<dyn std::error::Error>> {
    if database_url.contains("sslmode=disable") {
        return Ok(Client::connect(database_url, NoTls)?);
    }
    let connector = MakeTlsConnector::new(TlsConnector::new()?);
    Ok(Client::connect(database_url, connector)?)
}

/// Read the lots, preferring ground truth the decoder cannot have written.
///
/// Migration `0008_normalize_filters` rewrote four columns of `lot_vehicle` in
/// place, and two of them — `vehicle_type` and `fuel_type` — were filled from
/// the `nhtsa_*` columns, i.e. from a previous run of this very decoder.
/// Scoring against those would be measuring agreement with the old decoder
/// rather than accuracy, and it flatters both: the old decoder called ~10,000
/// petrol Ford 3.5 EcoBoosts "Diesel", and `fuel_type` now says DIESEL for all
/// of them.
///
/// `lot_vehicle_pre0008_cols` holds the values as the auctions published them,
/// so those win wherever they exist.
fn fetch(client: &mut Client, args: &Args) -> Result<Vec<Lot>, postgres::Error> {
    // Order by primary key so --offset walks a stable sequence rather than
    // resampling the same rows.
    let statement = "
        select lv.lot_number, lv.vin, lv.make, lv.model, lv.series, lv.year,
               pre.vehicle_type, pre.fuel_type, pre.drive_type, pre.transmission,
               lv.vehicle_type, lv.fuel_type, lv.drive_type, lv.transmission,
               lv.engine_cylinders, lv.engine_name,
               lv.nhtsa_make, lv.nhtsa_model, lv.nhtsa_year, lv.nhtsa_body_style,
               lv.nhtsa_fuel_type_primary, lv.nhtsa_drive_type, lv.nhtsa_transmission
        from lot_vehicle lv
        left join lot_vehicle_pre0008_cols pre on pre.lot_number = lv.lot_number
        where length(lv.vin) = 17
          and ($1::text is null or lv.source = $1)
        order by lv.lot_number
        offset $2 limit $3";

    let rows = client.query(statement, &[&args.source, &args.offset, &args.limit])?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let original: [Option<String>; 4] = [row.get(6), row.get(7), row.get(8), row.get(9)];
            let rewritten: [Option<String>; 4] =
                [row.get(10), row.get(11), row.get(12), row.get(13)];
            // A row has original ground truth if the pre-migration snapshot
            // covers it at all.
            let original_ground_truth = original.iter().any(Option::is_some);

            let pick = |index: usize| -> Option<String> {
                if original_ground_truth {
                    original[index].clone()
                } else if args.allow_derived {
                    rewritten[index].clone()
                } else {
                    None
                }
            };

            Lot {
                lot_number: row.get(0),
                vin: row.get(1),
                make: row.get(2),
                model: row.get(3),
                series: row.get(4),
                year: row.get(5),
                vehicle_type: pick(0),
                fuel_type: pick(1),
                drive_type: pick(2),
                transmission: pick(3),
                engine_cylinders: row.get(14),
                engine_name: row.get(15),
                original_ground_truth,
                stored: StoredDecode {
                    make: row.get(16),
                    model: row.get(17),
                    year: row.get(18),
                    body_style: row.get(19),
                    fuel_type: row.get(20),
                    drive_type: row.get(21),
                    transmission: row.get(22),
                },
            }
        })
        .collect())
}

/// Score the decoder in this build.
fn score_current(decoded: &[(Lot, Result<VehicleInfo, String>)], cars_only: bool) -> Report {
    decoded
        .par_iter()
        .fold(
            || Report::new("corgi-rs (this build)"),
            |mut report, (lot, result)| {
                match result {
                    Err(err) => {
                        report.rows += 1;
                        report.record_failure(failure_reason(err));
                    }
                    Ok(info) => {
                        if cars_only && !info.is_passenger_vehicle() {
                            return report;
                        }
                        report.rows += 1;
                        score_row(
                            &mut report,
                            lot,
                            info.make.as_str(),
                            info.model.as_deref(),
                            Some(info.year),
                            info.body_style,
                            info.fuel_type.as_deref(),
                            info.fuel_type_secondary.as_deref(),
                            info.drive_type.as_deref(),
                            info.transmission.as_deref(),
                            info.engine_cylinders,
                            info.displacement_l,
                            info.is_electrified(),
                        );
                    }
                }
                report
            },
        )
        .reduce(
            || Report::new("corgi-rs (this build)"),
            |mut a, b| {
                a.merge(&b);
                a
            },
        )
}

/// Score whatever already sits in the `nhtsa_*` columns.
fn score_stored(decoded: &[(Lot, Result<VehicleInfo, String>)], cars_only: bool) -> Report {
    decoded
        .par_iter()
        .fold(
            || Report::new("stored nhtsa_* columns"),
            |mut report, (lot, result)| {
                if cars_only && !result.as_ref().is_ok_and(VehicleInfo::is_passenger_vehicle) {
                    return report;
                }
                report.rows += 1;
                let stored = &lot.stored;
                if stored.make.is_none() && stored.model.is_none() {
                    report.record_failure("no stored decode".to_string());
                }
                score_row(
                    &mut report,
                    lot,
                    stored.make.as_deref().unwrap_or_default(),
                    stored.model.as_deref(),
                    stored.year,
                    stored.body_style.as_deref().map(BodyStyle::classify),
                    stored.fuel_type.as_deref(),
                    None,
                    stored.drive_type.as_deref(),
                    stored.transmission.as_deref(),
                    None,
                    None,
                    stored
                        .fuel_type
                        .as_deref()
                        .is_some_and(|fuel| fuel.eq_ignore_ascii_case("hybrid")),
                );
                report
            },
        )
        .reduce(
            || Report::new("stored nhtsa_* columns"),
            |mut a, b| {
                a.merge(&b);
                a
            },
        )
}

/// Compare one decode against one listing, field by field.
#[allow(clippy::too_many_arguments)]
fn score_row(
    report: &mut Report,
    lot: &Lot,
    make: &str,
    model: Option<&str>,
    year: Option<i32>,
    body_style: Option<BodyStyle>,
    fuel: Option<&str>,
    fuel_secondary: Option<&str>,
    drive: Option<&str>,
    transmission: Option<&str>,
    cylinders: Option<i32>,
    displacement: Option<f64>,
    electrified: bool,
) {
    // make
    {
        let expected = normalize::make(&lot.make);
        let actual = normalize::make(make);
        let agrees = expected.is_some() && expected == actual;
        observe(report.field("make"), expected, actual, agrees, agrees);
    }

    // model -- the listing often appends the series or trim.
    {
        let expected = normalize::model(&lot.model);
        let actual = model.and_then(normalize::model);
        let (agrees, relaxed) = match (&expected, &actual) {
            (Some(_), Some(_)) => {
                let strict = normalize::model_agrees(&lot.model, model.unwrap_or_default(), false);
                // The series is a legitimate second chance: vPIC splits
                // "SORENTO LX" into Model=Sorento, Series=LX.
                let relaxed = strict
                    || normalize::model_agrees(&lot.model, model.unwrap_or_default(), true)
                    || lot.series.as_deref().is_some_and(|series| {
                        normalize::model_agrees(series, model.unwrap_or_default(), true)
                    });
                (strict, relaxed)
            }
            _ => (false, false),
        };
        observe(report.field("model"), expected, actual, agrees, relaxed);
    }

    // year
    {
        let expected = (lot.year > 1900).then(|| lot.year.to_string());
        let actual = year.map(|year| year.to_string());
        let agrees = expected.is_some() && expected == actual;
        // A listing and a VIN can legitimately disagree by one: the model year
        // rolls over mid-calendar-year and sellers often enter the sale year.
        let relaxed = agrees
            || match (lot.year, year) {
                (listed, Some(decoded)) if listed > 1900 => (listed - decoded).abs() <= 1,
                _ => false,
            };
        observe(report.field("year"), expected, actual, agrees, relaxed);
    }

    // body style
    {
        let expected = lot.vehicle_type.as_deref().and_then(normalize::body_style);
        let (agrees, relaxed) = match (expected, body_style) {
            (Some(expected), Some(actual)) => (
                normalize::body_agrees(expected, actual, false),
                normalize::body_agrees(expected, actual, true),
            ),
            _ => (false, false),
        };
        observe(
            report.field("body style"),
            expected.map(|style| style.to_string()),
            body_style.map(|style| style.to_string()),
            agrees,
            relaxed,
        );
    }

    // fuel -- the listings have a HYBRID category vPIC expresses separately.
    {
        let listed = lot.fuel_type.as_deref().unwrap_or_default();
        if normalize::is_hybrid_listing(listed) {
            observe(
                report.field("fuel"),
                Some("HYBRID".to_string()),
                fuel.map(|_| if electrified { "HYBRID" } else { "NOT HYBRID" }.to_string()),
                electrified,
                electrified,
            );
        } else {
            let expected = normalize::fuel(listed);
            let actual = fuel.and_then(normalize::fuel);
            let (agrees, relaxed) = match expected {
                Some(expected) => (
                    normalize::fuel_agrees(expected, fuel, fuel_secondary, false),
                    normalize::fuel_agrees(expected, fuel, fuel_secondary, true),
                ),
                None => (false, false),
            };
            observe(
                report.field("fuel"),
                expected.map(|fuel| format!("{fuel:?}")),
                actual.map(|fuel| format!("{fuel:?}")),
                agrees,
                relaxed,
            );
        }
    }

    // drive
    {
        let expected = normalize::drive(lot.drive_type.as_deref().unwrap_or_default());
        let actual = drive.and_then(normalize::drive);
        let (agrees, relaxed) = match (expected, actual) {
            (Some(expected), Some(actual)) => (
                normalize::drive_agrees(expected, actual, false),
                normalize::drive_agrees(expected, actual, true),
            ),
            _ => (false, false),
        };
        observe(
            report.field("drive"),
            expected.map(|drive| format!("{drive:?}")),
            actual.map(|drive| format!("{drive:?}")),
            agrees,
            relaxed,
        );
    }

    // transmission
    {
        let expected = normalize::transmission(lot.transmission.as_deref().unwrap_or_default());
        let actual = transmission.and_then(normalize::transmission);
        let agrees = expected.is_some() && expected == actual;
        observe(
            report.field("transmission"),
            expected.map(|value| format!("{value:?}")),
            actual.map(|value| format!("{value:?}")),
            agrees,
            agrees,
        );
    }

    // cylinders
    {
        let expected = lot
            .engine_cylinders
            .as_deref()
            .and_then(normalize::cylinders);
        let agrees = expected.is_some() && expected == cylinders;
        observe(
            report.field("cylinders"),
            expected.map(|count| count.to_string()),
            cylinders.map(|count| count.to_string()),
            agrees,
            agrees,
        );
    }

    // displacement
    {
        let expected = lot
            .engine_name
            .as_deref()
            .and_then(normalize::displacement_l);
        let agrees = match (expected, displacement) {
            (Some(expected), Some(actual)) => normalize::displacement_agrees(expected, actual),
            _ => false,
        };
        observe(
            report.field("displacement"),
            expected.map(|litres| format!("{litres:.1}")),
            displacement.map(|litres| format!("{litres:.1}")),
            agrees,
            agrees,
        );
    }
}

fn observe(
    stats: &mut FieldStats,
    expected: Option<String>,
    actual: Option<String>,
    agrees: bool,
    relaxed: bool,
) {
    stats.observe(expected.as_deref(), actual.as_deref(), agrees, relaxed);
}

/// One row-level disagreement, for `--dump`.
struct Disagreement {
    vin: String,
    field: &'static str,
    listed: String,
    decoded: String,
}

/// Re-run the comparison for one lot and collect the fields that disagreed.
///
/// Scoring aggregates; this reproduces the same verdicts per row so a human can
/// go and look at the ones that matter.
fn disagreements(lot: &Lot, info: &VehicleInfo) -> Vec<Disagreement> {
    let mut report = Report::new("row");
    score_row(
        &mut report,
        lot,
        info.make.as_str(),
        info.model.as_deref(),
        Some(info.year),
        info.body_style,
        info.fuel_type.as_deref(),
        info.fuel_type_secondary.as_deref(),
        info.drive_type.as_deref(),
        info.transmission.as_deref(),
        info.engine_cylinders,
        info.displacement_l,
        info.is_electrified(),
    );

    report::FIELDS
        .iter()
        .filter_map(|field| {
            let stats = report.stats.get(field)?;
            let (pair, _) = stats.top_mismatches(1).into_iter().next()?;
            Some(Disagreement {
                vin: lot.vin.clone(),
                field,
                listed: pair.0,
                decoded: pair.1,
            })
        })
        .collect()
}

/// Write every disagreeing row to a CSV, so the aggregate numbers can be traced
/// back to individual cars.
fn dump_disagreements(
    path: &str,
    decoded: &[(Lot, Result<VehicleInfo, String>)],
) -> std::io::Result<usize> {
    use std::io::Write;

    let rows: Vec<Disagreement> = decoded
        .par_iter()
        .filter_map(|(lot, result)| result.as_ref().ok().map(|info| (lot, info)))
        .flat_map(|(lot, info)| disagreements(lot, info))
        .collect();

    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
    writeln!(out, "vin,field,listed,decoded")?;
    for row in &rows {
        writeln!(
            out,
            "{},{},{},{}",
            csv_cell(&row.vin),
            row.field,
            csv_cell(&row.listed),
            csv_cell(&row.decoded)
        )?;
    }
    out.flush()?;

    Ok(rows.len())
}

/// Quote a CSV cell if it contains a character that would break the row.
fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Collapse an error message to the category it belongs to, so the failure
/// breakdown does not list one entry per VIN.
fn failure_reason(message: &str) -> String {
    for (needle, label) in [
        ("is not a registered", "WMI not in the shipped tables"),
        ("does not encode a model year", "model year not encoded"),
        ("must be 17 characters", "wrong length"),
        ("invalid characters", "invalid characters"),
        ("check digit", "check digit rejected"),
    ] {
        if message.contains(needle) {
            return label.to_string();
        }
    }
    message.to_string()
}

/// How many attributes the decoder produced, which is the other half of the
/// goal: being right about more things, not just about the same few.
fn print_attribute_yield(decoded: &[(Lot, Result<VehicleInfo, String>)]) {
    let mut counts: Vec<usize> = decoded
        .iter()
        .filter_map(|(_, result)| result.as_ref().ok())
        .map(VehicleInfo::attribute_count)
        .collect();

    if counts.is_empty() {
        return;
    }
    counts.sort_unstable();

    let total: usize = counts.iter().sum();
    let percentile = |p: f64| counts[((counts.len() - 1) as f64 * p) as usize];

    println!("\n=== attributes decoded per VIN ===");
    println!(
        "  mean {:.1}   p10 {}   median {}   p90 {}   max {}",
        total as f64 / counts.len() as f64,
        percentile(0.10),
        percentile(0.50),
        percentile(0.90),
        counts[counts.len() - 1],
    );

    // Which elements show up at all, and how often. The long tail matters:
    // an element present on 3% of decodes is still tens of thousands of lots.
    let mut per_element: HashMap<&'static str, u64> = HashMap::new();
    for (_, result) in decoded {
        if let Ok(info) = result {
            for code in info.attributes.keys() {
                *per_element.entry(code).or_default() += 1;
            }
        }
    }
    let mut elements: Vec<_> = per_element.into_iter().collect();
    elements.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let decoded_count = counts.len() as f64;
    let widespread = elements
        .iter()
        .filter(|(_, count)| *count as f64 / decoded_count >= 0.5)
        .count();
    println!(
        "  {} distinct elements seen, {widespread} of them on at least half the VINs",
        elements.len()
    );

    println!("\n  element coverage:");
    for (code, count) in elements.iter() {
        println!(
            "    {:<34} {:>7.2}%",
            code,
            100.0 * *count as f64 / decoded_count
        );
    }
}

fn print_failures(decoded: &[(Lot, Result<VehicleInfo, String>)], limit: usize) {
    println!("\n=== sample decode failures ===");
    for (lot, result) in decoded.iter().filter(|(_, r)| r.is_err()).take(limit) {
        let Err(err) = result else { continue };
        println!(
            "  {} {} {} {} -- {err}",
            lot.vin, lot.year, lot.make, lot.model
        );
    }
}

/// Write the decode back into the `nhtsa_*` columns.
///
/// Off unless `--write` is passed: this mutates the live auction table.
fn write_back(
    client: &mut Client,
    decoded: &[(Lot, Result<VehicleInfo, String>)],
) -> Result<u64, postgres::Error> {
    let statement = client.prepare(
        "update lot_vehicle set
             nhtsa_make = $2, nhtsa_model = $3, nhtsa_series = $4, nhtsa_trim = $5,
             nhtsa_year = $6, nhtsa_body_style = $7, nhtsa_drive_type = $8,
             nhtsa_engine_type = $9, nhtsa_fuel_type_primary = $10,
             nhtsa_transmission = $11, nhtsa_doors = $12
         where lot_number = $1",
    )?;

    let mut updated = 0;
    for (lot, result) in decoded {
        let Ok(info) = result else { continue };
        let make = (!info.make.is_empty()).then(|| info.make.clone());
        let body = info.body_class.clone();
        updated += client.execute(
            &statement,
            &[
                &lot.lot_number,
                &make,
                &info.model,
                &info.series,
                &info.trim,
                &info.year,
                &body,
                &info.drive_type,
                &info.engine_type,
                &info.fuel_type,
                &info.transmission,
                &info.doors,
            ],
        )?;
    }

    Ok(updated)
}
