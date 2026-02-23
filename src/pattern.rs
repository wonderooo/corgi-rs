use std::collections::{HashMap, HashSet};

#[cfg(feature = "parallel")]
use rayon::{
    iter::{IndexedParallelIterator, IntoParallelIterator, ParallelBridge, ParallelIterator},
    slice::ParallelSliceMut,
};

#[cfg(feature = "parallel")]
use crate::RAYON_CHUNK_SIZE;

use crate::{Lookup, SchemaId, maps::FstRkyvMap};

#[allow(dead_code)]
static LOOKUP_TABLES: &[&str] = &[
    "DriveType",
    "EngineModel",
    "EngineConfiguration",
    "FuelType",
    "Transmission",
    "BodyStyle",
    "GrossVehicleWeightRating",
    "GrossVehicleWeightRatingTo",
    "GrossVehicleWeightRatingFrom",
    "ChargerLevel",
    "ElectrificationLevel",
    "EVDriveUnit",
    "BatteryType",
    "Make",
    "Model",
    "Series",
    "Trim",
    "Turbo",
    "DaytimeRunningLight",
    "Plant",
    "Country",
    "DaytimeRunningLight",
    "DestinationMarket",
    "Conversion",
];

pub struct PatternMatcher {
    wmi_schema_id_map: FstRkyvMap<SchemaId>,
    schema_id_lookup_map: FstRkyvMap<Lookup>,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct MatchQuery<'a> {
    pub wmi: &'a str,
    pub model_year: i32,
    pub vds: &'a str,
    pub vis: &'a str,
}

#[derive(Debug)]
pub struct PatternMatch {
    pub lookup: Lookup,
    pub schema_id: SchemaId,
    pub confidence: f64,
    pub positions: Vec<usize>,
    pub pattern_type: PatternType,
}

#[derive(Debug, Clone, Copy)]
pub enum PatternType {
    VDS,
    VIS,
}

struct LookupWithSchemaId {
    schema: SchemaId,
    lookup: Lookup,
}

impl PatternMatcher {
    pub fn new() -> Self {
        Self {
            wmi_schema_id_map: FstRkyvMap::new(),
            schema_id_lookup_map: FstRkyvMap::new(),
        }
    }

    pub fn matches(&self, query: &MatchQuery) -> Vec<PatternMatch> {
        #[cfg(not(feature = "parallel"))]
        let groupped = self
            .raw_matches(&query)
            .into_iter()
            //
            // Filter by confidence
            //
            .filter(|m| {
                // More lenient threshold for plant patterns
                if m.lookup.element_code.to_lowercase().contains("plant") {
                    return m.confidence > 0.3;
                }
                m.confidence > 0.5
            })
            //
            // Group by pattern name
            //
            .fold(
                HashMap::new(),
                |mut accu: HashMap<String, Vec<PatternMatch>>, next: PatternMatch| {
                    if let Some(patterns) = accu.get_mut(&next.lookup.element_code) {
                        patterns.push(next);
                    } else {
                        accu.insert(next.lookup.element_code.clone(), vec![next]);
                    }

                    accu
                },
            );

        #[cfg(feature = "parallel")]
        let groupped = self
            .raw_matches(&query)
            .into_par_iter()
            //
            // Filter by confidence
            //
            .filter(|m| {
                // More lenient threshold for plant patterns
                if m.lookup.element_code.to_lowercase().contains("plant") {
                    return m.confidence > 0.3;
                }
                m.confidence > 0.5
            })
            //
            // Group by pattern name
            //
            .fold(
                || HashMap::new(),
                |mut accu: HashMap<String, Vec<PatternMatch>>, next: PatternMatch| {
                    if let Some(patterns) = accu.get_mut(&next.lookup.element_code) {
                        patterns.push(next);
                    } else {
                        accu.insert(next.lookup.element_code.clone(), vec![next]);
                    }

                    accu
                },
            )
            .reduce(
                || HashMap::new(),
                |mut a, b| {
                    for (k, mut v) in b {
                        a.entry(k).or_default().append(&mut v);
                    }
                    a
                },
            );

        let matches = groupped
            .into_values()
            .map(|mut patterns| {
                //
                // Sort patterns by weight then by confidence
                //
                #[cfg(not(feature = "parallel"))]
                patterns.sort_by(|pat1, pat2| {
                    let w1 = pat1.lookup.element_weight.unwrap_or(0);
                    let w2 = pat2.lookup.element_weight.unwrap_or(0);

                    w2.cmp(&w1)
                        .then(pat2.confidence.total_cmp(&pat1.confidence))
                });

                #[cfg(feature = "parallel")]
                patterns.par_sort_by(|pat1, pat2| {
                    let w1 = pat1.lookup.element_weight.unwrap_or(0);
                    let w2 = pat2.lookup.element_weight.unwrap_or(0);

                    w2.cmp(&w1)
                        .then(pat2.confidence.total_cmp(&pat1.confidence))
                });

                //
                // Deduplicate patterns
                //
                #[derive(Hash, PartialEq, Eq)]
                struct PatternKey {
                    resolved: String,
                    positions: Vec<usize>,
                    schema_name: String,
                }

                let mut seen = HashSet::new();
                patterns.retain(|pat| {
                    let key = PatternKey {
                        resolved: pat.lookup.resolved.clone(),
                        positions: pat.positions.clone(),
                        schema_name: pat.schema_id.schema_id.clone(),
                    };
                    seen.insert(key)
                });

                patterns
            })
            //
            // Remove groupping by element name
            //
            .flatten()
            .collect::<Vec<_>>();

        return matches;
    }

    pub fn raw_matches(&self, query: &MatchQuery) -> Vec<PatternMatch> {
        let schemas = if let Some(schemas) = self.wmi_schema_id_map.get(&query.wmi) {
            schemas
        } else {
            return Vec::new();
        };

        #[cfg(not(feature = "parallel"))]
        let mut patterns = schemas
            .into_iter()
            .filter_map(|schema| {
                self.schema_id_lookup_map.get(&schema.schema_id).map(|v| {
                    v.into_iter().map(move |lookup| LookupWithSchemaId {
                        schema: schema.clone(),
                        lookup,
                    })
                })
            })
            .flatten()
            .collect::<Vec<LookupWithSchemaId>>();

        #[cfg(feature = "parallel")]
        let mut patterns = schemas
            .chunks(RAYON_CHUNK_SIZE)
            .par_bridge()
            .flat_map_iter(|chunk| {
                chunk
                    .into_iter()
                    .filter_map(|schema| {
                        self.schema_id_lookup_map.get(&schema.schema_id).map(|v| {
                            v.into_iter().map(move |lookup| LookupWithSchemaId {
                                schema: schema.clone(),
                                lookup,
                            })
                        })
                    })
                    .flatten()
            })
            .collect::<Vec<LookupWithSchemaId>>();

        #[cfg(not(feature = "parallel"))]
        patterns.sort_by(|res1, res2| {
            res2.lookup
                .element_weight
                .cmp(&res1.lookup.element_weight) // Descending
                .then_with(|| res1.lookup.pattern.cmp(&res2.lookup.pattern)) // Ascending
        });

        #[cfg(feature = "parallel")]
        patterns.par_sort_by(|res1, res2| {
            res2.lookup
                .element_weight
                .cmp(&res1.lookup.element_weight) // Descending
                .then_with(|| res1.lookup.pattern.cmp(&res2.lookup.pattern)) // Ascending
        });

        //
        // Find the most specific schema by looking at model patterns
        //
        #[cfg(not(feature = "parallel"))]
        let mut model_patterns = patterns
            .iter()
            .filter_map(|pat| {
                if pat.lookup.element_code == "Model" {
                    return Some((
                        calculate_confidence(
                            &pat.lookup.pattern,
                            &format!("{}{}", &query.vds, &query.vis),
                        ),
                        pat,
                    ));
                }
                None
            })
            .collect::<Vec<_>>();

        #[cfg(feature = "parallel")]
        let mut model_patterns = patterns
            .chunks(RAYON_CHUNK_SIZE)
            .par_bridge()
            .flat_map_iter(|chunk| {
                chunk.iter().filter_map(|pat| {
                    if pat.lookup.element_code == "Model" {
                        return Some((
                            calculate_confidence(
                                &pat.lookup.pattern,
                                &format!("{}{}", &query.vds, &query.vis),
                            ),
                            pat,
                        ));
                    }
                    None
                })
            })
            .collect::<Vec<_>>();

        #[cfg(not(feature = "parallel"))]
        model_patterns.sort_by(|(co1, _), (co2, _)| co2.total_cmp(co1)); // Desc

        #[cfg(feature = "parallel")]
        model_patterns.par_sort_by(|(co1, _), (co2, _)| co2.total_cmp(co1)); // Desc

        //
        // Get the most relevant schema name
        //
        let primary_schema = model_patterns.get(0).map(|mp| mp.1.schema.clone());
        drop(model_patterns);

        //
        // Format patterns
        //
        #[cfg(not(feature = "parallel"))]
        let patterns = patterns
            .into_iter()
            .map(|pattern| create_pattern_match(pattern, &query, primary_schema.as_ref()))
            .collect();

        #[cfg(feature = "parallel")]
        let patterns = patterns
            .into_par_iter()
            .chunks(RAYON_CHUNK_SIZE)
            .flat_map_iter(|chunk| {
                chunk
                    .into_iter()
                    .map(|pattern| create_pattern_match(pattern, &query, primary_schema.as_ref()))
            })
            .collect();

        patterns
    }
}

fn create_pattern_match(
    pattern: LookupWithSchemaId,
    match_query: &MatchQuery,
    primary_schema: Option<&SchemaId>,
) -> PatternMatch {
    let is_vis_pattern = pattern.lookup.pattern.contains('|');
    let pattern_type = if is_vis_pattern {
        PatternType::VIS
    } else {
        PatternType::VDS
    };

    //
    // Calculate base confidence
    //
    let base_confidence = if is_vis_pattern {
        calculate_confidence(
            &pattern.lookup.pattern,
            &match_query.vis.get(1..2).unwrap_or(""),
        )
    } else {
        calculate_confidence(
            &pattern.lookup.pattern,
            &format!("{}{}", &match_query.vds, &match_query.vis),
        )
    };

    //
    // Adjust confidence based on schema match for plant codes
    //
    let mut confidence = base_confidence;
    if pattern.lookup.element_code.to_lowercase().contains("plant") {
        if let Some(ps) = primary_schema {
            confidence = if pattern.schema == *ps {
                base_confidence
            } else {
                0.
            }
        }
    }

    //
    // Calculate correct positions based on pattern type
    //
    let mut positions = Vec::new();
    let actual_pattern = pattern
        .lookup
        .pattern
        .split('|')
        .nth(0)
        .unwrap_or(&pattern.lookup.pattern);
    let start_pos = if is_vis_pattern { 9 } else { 3 };
    actual_pattern.chars().enumerate().for_each(|(idx, c)| {
        if c != '|' {
            positions.push(start_pos + idx);
        }
    });

    PatternMatch {
        lookup: pattern.lookup,
        schema_id: pattern.schema,
        confidence,
        positions,
        pattern_type,
    }
}

pub fn calculate_confidence(pattern: &str, input: &str) -> f64 {
    if pattern.is_empty() || input.is_empty() {
        return 0.0;
    }

    // Split pattern into actual pattern + metadata
    let mut parts = pattern.split('|');
    let actual_pattern = match parts.next() {
        Some(p) => p,
        None => return 0.0,
    };
    let metadata: Vec<&str> = parts.collect();

    // Special handling for VIS patterns
    if !metadata.is_empty() && actual_pattern.len() == 5 {
        let plant_code_char = match input.chars().next() {
            Some(c) => c,
            None => return 0.0,
        };

        let vis_pattern = metadata[0];
        let expected_plant_code = match vis_pattern.chars().nth(1) {
            Some(c) => c,
            None => return 0.0,
        };

        if expected_plant_code == '*' {
            return 0.8;
        }

        if expected_plant_code == plant_code_char {
            return 1.0;
        }

        return 0.0;
    }

    // Must match first
    if !matches_pattern(input, actual_pattern) {
        return 0.0;
    }

    let pattern_chars: Vec<char> = actual_pattern.chars().collect();
    let input_chars: Vec<char> = input.chars().collect();

    let mut exact_matches = 0.0;
    let mut class_matches = 0.0;
    let mut wildcard_matches = 0.0;
    let mut total_length = 0.0;

    let mut pattern_index = 0usize;
    let mut input_index = 0usize;

    while pattern_index < pattern_chars.len() && input_index < input_chars.len() {
        let p = pattern_chars[pattern_index];
        let i = input_chars[input_index];

        if p == '[' {
            let mut close = None;
            for idx in pattern_index + 1..pattern_chars.len() {
                if pattern_chars[idx] == ']' {
                    close = Some(idx);
                    break;
                }
            }

            let close = match close {
                Some(c) => c,
                None => break,
            };

            let content: String = pattern_chars[pattern_index + 1..close].iter().collect();

            // Ranges are less specific than explicit lists
            if content.contains('-') {
                class_matches += 0.7;
            } else {
                class_matches += 0.8;
            }

            total_length += 1.0;
            pattern_index = close + 1;
            input_index += 1;
        } else if p == '*' {
            wildcard_matches += 1.0;
            total_length += 1.0;
            pattern_index += 1;
            input_index += 1;
        } else {
            if p == i {
                exact_matches += 1.0;
            }

            total_length += 1.0;
            pattern_index += 1;
            input_index += 1;
        }
    }

    if total_length == 0.0 {
        return 0.0;
    }

    let score: f64 = (exact_matches * 1.0 + class_matches + wildcard_matches * 0.5) / total_length;

    score.clamp(0.0, 1.0)
}

pub fn matches_pattern(input: &str, pattern: &str) -> bool {
    if input.is_empty() || pattern.is_empty() {
        return false;
    }

    // Split pattern into actual pattern + metadata
    let mut parts = pattern.split('|');
    let actual_pattern = match parts.next() {
        Some(p) => p,
        None => return false,
    };

    let metadata: Vec<&str> = parts.collect();

    // Special handling for VIS patterns (e.g. "*****|*U")
    if !metadata.is_empty() && actual_pattern.chars().count() == 5 {
        let vis_pattern = metadata[0];

        let plant_code_char = match input.chars().next() {
            Some(c) => c,
            None => return false,
        };

        let expected_plant_code = match vis_pattern.chars().nth(1) {
            Some(c) => c,
            None => return false,
        };

        return expected_plant_code == '*' || plant_code_char == expected_plant_code;
    }

    matches_simple_pattern(input, actual_pattern)
}

pub fn matches_simple_pattern(input: &str, pattern: &str) -> bool {
    let input_chars: Vec<char> = input.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();

    let mut pattern_index = 0usize;
    let mut input_index = 0usize;

    while pattern_index < pattern_chars.len() && input_index < input_chars.len() {
        let pattern_char = pattern_chars[pattern_index];
        let input_char = input_chars[input_index];

        // Handle character class patterns: [ ... ]
        if pattern_char == '[' {
            let mut close_bracket = None;

            for i in pattern_index + 1..pattern_chars.len() {
                if pattern_chars[i] == ']' {
                    close_bracket = Some(i);
                    break;
                }
            }

            let close_bracket = match close_bracket {
                Some(i) => i,
                None => return false,
            };

            let char_class: String = pattern_chars[pattern_index..=close_bracket]
                .iter()
                .collect();

            if !is_char_in_range(input_char, &char_class) {
                return false;
            }

            pattern_index = close_bracket + 1;
            input_index += 1;
            continue;
        }

        // Handle wildcard '*'
        if pattern_char == '*' {
            // If this is the last pattern character, match rest of input
            if pattern_index == pattern_chars.len() - 1 {
                return true;
            }

            pattern_index += 1;
            input_index += 1;
            continue;
        }

        // Exact character match
        if input_char != pattern_char {
            return false;
        }

        pattern_index += 1;
        input_index += 1;
    }

    // Pattern matched if:
    // - all pattern characters consumed
    // - or the only remaining pattern char is '*'
    pattern_index >= pattern_chars.len()
        || (pattern_index == pattern_chars.len() - 1 && pattern_chars[pattern_index] == '*')
}

pub fn is_char_in_range(ch: char, pattern: &str) -> bool {
    // Not a character class: exact match or wildcard
    if !pattern.starts_with('[') || !pattern.ends_with(']') {
        return pattern == "*" || pattern.chars().next() == Some(ch);
    }

    // Strip [ and ]
    let content = &pattern[1..pattern.len() - 1];
    let chars: Vec<char> = content.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        // Range like A-E
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            let start = chars[i] as u32;
            let end = chars[i + 2] as u32;
            let c = ch as u32;

            if c >= start && c <= end {
                return true;
            }

            i += 3;
        } else {
            // Single character like [ABC]
            if chars[i] == ch {
                return true;
            }

            i += 1;
        }
    }

    false
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_pattern_raw() {
        let matcher = PatternMatcher::new();
        // let _v = vec![
        //     MatchQuery {
        //         wmi: "2C3".to_string(),
        //         vds: "CDXBG1".to_string(),
        //         vis: "FH832587".to_string(),
        //         model_year: 2015,
        //     },
        //     MatchQuery {
        //         wmi: "SCB".to_string(),
        //         vds: "BR9ZA8".to_string(),
        //         vis: "DC079455".to_string(),
        //         model_year: 2013,
        //     },
        //     MatchQuery {
        //         wmi: "5NP".to_string(),
        //         vds: "D74LF7".to_string(),
        //         vis: "HH126052".to_string(),
        //         model_year: 2017,
        //     },
        //     MatchQuery {
        //         wmi: "1HD".to_string(),
        //         vds: "1KHM11".to_string(),
        //         vis: "CB675783".to_string(),
        //         model_year: 2012,
        //     },
        //     MatchQuery {
        //         wmi: "3KP".to_string(),
        //         vds: "F24AD6".to_string(),
        //         vis: "PE638817".to_string(),
        //         model_year: 2023,
        //     },
        //     MatchQuery {
        //         wmi: "1HG".to_string(),
        //         vds: "CP2673".to_string(),
        //         vis: "9A060971".to_string(),
        //         model_year: 2009,
        //     },
        // ];
        let v = vec![MatchQuery {
            wmi: "1HG",
            vds: "CP2673",
            vis: "9A060971",
            model_year: 2009,
        }];
        let r = matcher.matches(&v[0]);
        println!("{r:#?}")
    }
}
