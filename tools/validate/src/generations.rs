//! Check the hand-maintained generation table against real VINs.
//!
//! A generation boundary should be visible in the VIN: when a manufacturer
//! redesigns a model it usually issues new body codes, so the set of codes in
//! the first year of a generation shares little with the year before it.
//!
//! The test has known blind spots and reports rather than fails. Ford puts cab,
//! series and weight rating in those positions and carries them across a
//! redesign unchanged, so an F-150 generation boundary is invisible here even
//! though it is real. Two generations sold side by side in the changeover year
//! look the same way. Treat a flagged boundary as something to look at, not as
//! a proven error.

use std::collections::{HashMap, HashSet};

use corgi_rs::{VehicleInfo, VinDecoder};

/// How the VIN data judged one declared boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The body codes turned over almost completely: a redesign is visible.
    Confirmed,
    /// Some turnover, but not a clean break.
    Weak,
    /// The body codes barely changed. Either the boundary is wrong, or this
    /// manufacturer does not encode the generation there.
    NotVisible,
    /// Too few lots either side to say anything.
    NoData,
}

/// Body codes seen for a model in a model year, keyed by (make, model, year).
type Codes = HashMap<(String, String, i32), HashMap<String, u32>>;

/// Group the decoded corpus by model and year, keeping VIN positions 4-8.
fn index(decoded: &[(String, Result<VehicleInfo, String>)]) -> Codes {
    let mut codes: Codes = HashMap::new();
    for (vin, result) in decoded {
        let Ok(info) = result else { continue };
        let (Some(model), true) = (info.model.as_deref(), vin.len() == 17) else {
            continue;
        };
        if info.make.is_empty() {
            continue;
        }
        *codes
            .entry((info.make.clone(), model.to_string(), info.year))
            .or_default()
            .entry(vin[3..8].to_string())
            .or_default() += 1;
    }
    codes
}

/// The body codes a model used in `year`, ignoring one-off oddities.
fn codes_in(codes: &Codes, make: &str, model: &str, year: i32) -> HashSet<String> {
    codes
        .get(&(make.to_string(), model.to_string(), year))
        .map(|counts| {
            counts
                .iter()
                .filter(|(_, count)| **count >= MIN_LOTS_PER_CODE)
                .map(|(code, _)| code.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A body code needs this many lots before it counts, so a mistyped VIN cannot
/// look like a redesign.
const MIN_LOTS_PER_CODE: u32 = 3;
/// Below this many distinct codes there is not enough to compare.
const MIN_CODES_PER_YEAR: usize = 3;

/// Share of `year`'s body codes that were already in use the year before.
fn continuity(codes: &Codes, make: &str, model: &str, year: i32) -> Option<f64> {
    let current = codes_in(codes, make, model, year);
    let previous = codes_in(codes, make, model, year - 1);
    if current.len() < MIN_CODES_PER_YEAR || previous.len() < MIN_CODES_PER_YEAR {
        return None;
    }
    let shared = current.intersection(&previous).count();
    Some(100.0 * shared as f64 / current.len().max(previous.len()) as f64)
}

fn verdict(continuity: Option<f64>) -> Verdict {
    match continuity {
        None => Verdict::NoData,
        Some(shared) if shared <= 25.0 => Verdict::Confirmed,
        Some(shared) if shared <= 55.0 => Verdict::Weak,
        Some(_) => Verdict::NotVisible,
    }
}

/// Report on every generation boundary the decoder can produce.
pub fn check(decoded: &[(String, Result<VehicleInfo, String>)], decoder: &VinDecoder) {
    let codes = index(decoded);

    // Which (make, model, year) combinations the corpus actually contains, and
    // what generation the decoder assigns to each.
    let mut boundaries: HashMap<(String, String), HashSet<(i32, String)>> = HashMap::new();
    let mut with_generation = 0usize;
    let mut total = 0usize;

    for (_, result) in decoded {
        let Ok(info) = result else { continue };
        total += 1;
        let Some(generation) = &info.generation else {
            continue;
        };
        with_generation += 1;
        if let Some(model) = info.model.as_deref() {
            boundaries
                .entry((info.make.clone(), model.to_string()))
                .or_default()
                .insert((generation.year_from as i32, generation.name.clone()));
        }
    }

    println!("\n=== generation table ===");
    println!(
        "resolved on {with_generation} of {total} decoded lots ({:.1}%), \
         across {} models",
        100.0 * with_generation as f64 / total.max(1) as f64,
        boundaries.len()
    );

    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut flagged: Vec<(f64, String, String, i32, String)> = Vec::new();

    for ((make, model), starts) in &boundaries {
        // The earliest generation present has no predecessor to compare with.
        let earliest = starts.iter().map(|(year, _)| *year).min();
        for (year, name) in starts {
            if Some(*year) == earliest {
                continue;
            }
            let shared = continuity(&codes, make, model, *year);
            let label = match verdict(shared) {
                Verdict::Confirmed => "confirmed",
                Verdict::Weak => "weak",
                Verdict::NoData => "no data",
                Verdict::NotVisible => {
                    flagged.push((
                        shared.unwrap_or_default(),
                        make.clone(),
                        model.clone(),
                        *year,
                        name.clone(),
                    ));
                    "not visible"
                }
            };
            *counts.entry(label).or_default() += 1;
        }
    }

    println!(
        "\nboundaries visible in the VIN body codes: {} confirmed, {} weak, {} not visible, {} no data",
        counts.get("confirmed").copied().unwrap_or(0),
        counts.get("weak").copied().unwrap_or(0),
        counts.get("not visible").copied().unwrap_or(0),
        counts.get("no data").copied().unwrap_or(0),
    );

    if !flagged.is_empty() {
        flagged.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        println!("\n  boundaries worth a look (body codes barely changed):");
        for (shared, make, model, year, name) in &flagged {
            println!("    {shared:5.0}%  {make} {model} {year}  {name}");
        }
    }

    // Models the corpus has plenty of but the table does not cover.
    let mut missing: HashMap<(String, String), usize> = HashMap::new();
    for (_, result) in decoded {
        let Ok(info) = result else { continue };
        if info.generation.is_some() {
            continue;
        }
        if let Some(model) = info.model.as_deref() {
            *missing
                .entry((info.make.clone(), model.to_string()))
                .or_default() += 1;
        }
    }
    let mut missing: Vec<_> = missing.into_iter().collect();
    missing.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if !missing.is_empty() {
        println!("\n  biggest models with no generation in the table:");
        for ((make, model), count) in missing.iter().take(15) {
            println!("    {count:>7}  {make} {model}");
        }
    }

    // Keep the decoder argument meaningful: assert the table round-trips for a
    // model we know is covered, so a broken asset shows up here too.
    if decoder
        .decode("1HGCP26739A060971")
        .ok()
        .and_then(|info| info.generation)
        .is_none()
    {
        println!("\n  WARNING: the generation table did not resolve a known-covered VIN");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(rows: &[(&str, &str, &str, i32)]) -> Codes {
        let mut codes: Codes = HashMap::new();
        for (make, model, code, year) in rows {
            *codes
                .entry((make.to_string(), model.to_string(), *year))
                .or_default()
                .entry(code.to_string())
                .or_default() += MIN_LOTS_PER_CODE;
        }
        codes
    }

    #[test]
    fn a_clean_redesign_reads_as_confirmed() {
        let codes = corpus(&[
            ("Honda", "Civic", "FC1AA", 2015),
            ("Honda", "Civic", "FC2AA", 2015),
            ("Honda", "Civic", "FC3AA", 2015),
            ("Honda", "Civic", "FE1AA", 2016),
            ("Honda", "Civic", "FE2AA", 2016),
            ("Honda", "Civic", "FE3AA", 2016),
        ]);
        assert_eq!(
            verdict(continuity(&codes, "Honda", "Civic", 2016)),
            Verdict::Confirmed
        );
    }

    #[test]
    fn carried_over_codes_read_as_not_visible() {
        let codes = corpus(&[
            ("Ford", "F-150", "W1E5A", 2020),
            ("Ford", "F-150", "W1E5B", 2020),
            ("Ford", "F-150", "W1E5C", 2020),
            ("Ford", "F-150", "W1E5A", 2021),
            ("Ford", "F-150", "W1E5B", 2021),
            ("Ford", "F-150", "W1E5C", 2021),
        ]);
        assert_eq!(
            verdict(continuity(&codes, "Ford", "F-150", 2021)),
            Verdict::NotVisible
        );
    }

    #[test]
    fn too_little_data_is_reported_as_such_rather_than_as_a_pass() {
        let codes = corpus(&[
            ("Rare", "Model", "AAAAA", 2020),
            ("Rare", "Model", "BBBBB", 2021),
        ]);
        assert_eq!(
            verdict(continuity(&codes, "Rare", "Model", 2021)),
            Verdict::NoData
        );
    }

    #[test]
    fn a_single_mistyped_vin_cannot_look_like_a_redesign() {
        let mut codes = corpus(&[
            ("Honda", "Civic", "FC1AA", 2015),
            ("Honda", "Civic", "FC2AA", 2015),
            ("Honda", "Civic", "FC3AA", 2015),
            ("Honda", "Civic", "FC1AA", 2016),
            ("Honda", "Civic", "FC2AA", 2016),
            ("Honda", "Civic", "FC3AA", 2016),
        ]);
        // One stray code, below the threshold, must not shift the verdict.
        codes
            .get_mut(&("Honda".into(), "Civic".into(), 2016))
            .unwrap()
            .insert("XXXXX".into(), 1);
        assert_eq!(
            verdict(continuity(&codes, "Honda", "Civic", 2016)),
            Verdict::NotVisible
        );
    }
}
