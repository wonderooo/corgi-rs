use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{
    CorgiError,
    db::Db,
    types::{
        LookupId, PatternQuery, PatternType, RawPattern, ResolvedPattern, SchemaQuery, TableName,
    },
};

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
    db: Arc<Db>,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub struct PatternDescriptor {
    pub wmi: String,
    pub model_year: i32,
    pub vds: String,
    pub vis: String,
}

#[derive(Debug)]
pub struct PatternMatch {
    pub element: String,
    pub element_code: String,
    pub attribute_id: String,
    pub resolved: String,
    pub confidence: f64,
    pub positions: Vec<usize>,
    pub schema_name: String,
    pub metadata: Option<Metadata>,
}

#[derive(Debug)]
pub struct Metadata {
    pub lookup_table_name: Option<String>,
    pub group_name: Option<String>,
    pub element_weight: Option<i32>,
    pub pattern_type: PatternType,
    pub raw_pattern: String,
    pub match_details: Option<MatchDetails>,
}

#[derive(Debug)]
pub struct MatchDetails {
    pub exact_matches: Option<i32>,
    pub wildcard_matches: Option<i32>,
    pub total_positions: Option<i32>,
}

impl PatternMatcher {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    pub async fn new_with_default_db() -> Self {
        let db = Db::new().await;
        let db = Arc::new(db);
        Self { db }
    }

    pub async fn matches(
        &self,
        pattern_descriptors: Vec<PatternDescriptor>,
    ) -> Result<HashMap<PatternDescriptor, Vec<PatternMatch>>, CorgiError> {
        //
        // Get raw matches
        //
        let raw_matches = self.raw_matches(pattern_descriptors).await?;

        //
        // Map raw matches to cleaner format, filter by confidence, group by element name and dedup
        //
        let matches = raw_matches
            .into_iter()
            .map(|(descriptor, matches)| {
                (
                    descriptor,
                    matches
                        .into_iter()
                        //
                        // Filter by confidence
                        //
                        .filter(|m| {
                            // More lenient threshold for plant patterns
                            if m.pattern.element_name.to_lowercase().contains("plant") {
                                return m.confidence > 0.3;
                            }
                            m.confidence > 0.5
                        })
                        //
                        // Map to cleaner format
                        //
                        .map(|m| m.into())
                        //
                        // Group by pattern name
                        //
                        .fold(
                            HashMap::new(),
                            |mut accu: HashMap<String, Vec<PatternMatch>>, next: PatternMatch| {
                                if let Some(patterns) = accu.get_mut(&next.element) {
                                    patterns.push(next);
                                } else {
                                    accu.insert(next.element.clone(), vec![next]);
                                }

                                accu
                            },
                        )
                        .into_values()
                        .map(|mut patterns| {
                            //
                            // Sort patterns by weight then by confidence
                            //
                            patterns.sort_by(|pat1, pat2| {
                                let w1 = pat1
                                    .metadata
                                    .as_ref()
                                    .map(|met| met.element_weight)
                                    .flatten()
                                    .unwrap_or(0);
                                let w2 = pat2
                                    .metadata
                                    .as_ref()
                                    .map(|met| met.element_weight)
                                    .flatten()
                                    .unwrap_or(0);

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
                                    resolved: pat.resolved.clone(),
                                    positions: pat.positions.clone(),
                                    schema_name: pat.schema_name.clone(),
                                };
                                seen.insert(key)
                            });

                            patterns
                        })
                        //
                        // Remove groupping by element name
                        //
                        .flatten()
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();

        Ok(matches)
    }

    pub async fn raw_matches(
        &self,
        pattern_descriptors: Vec<PatternDescriptor>,
    ) -> Result<HashMap<PatternDescriptor, Vec<RawPattern>>, CorgiError> {
        //
        // Get schemas for all wmi, model year pairs
        //
        let schemas = self
            .db
            .get_schemas(
                pattern_descriptors
                    .iter()
                    .map(|d| SchemaQuery {
                        wmi: d.wmi.to_string(),
                        model_year: d.model_year,
                        vds: d.vds.to_string(),
                        vis: d.vis.to_string(),
                    })
                    .collect(),
            )
            .await?;

        //
        // Get all patterns for all schema ids
        //
        let mut patterns = self
            .db
            .get_patterns(
                schemas
                    .into_iter()
                    .map(|s| PatternQuery {
                        schema_id: s.schema_id,
                        wmi: s.wmi,
                        model_year: s.model_year,
                        vds: s.vds,
                        vis: s.vis,
                    })
                    .collect(),
            )
            .await?;

        //
        // Filter patterns based on lookup table classes
        //
        patterns = patterns
            .into_iter()
            .filter(|p| {
                if let Some(lookup_table) = &p.lookup_table {
                    if !LOOKUP_TABLES.contains(&lookup_table.as_str())
                        || lookup_table.contains("vNCSA")
                    {
                        return false;
                    }
                }
                return true;
            })
            .collect();

        //
        // Get lookup names for all patterns that have lookup table
        //
        let mut lookup_map = HashMap::<TableName, Vec<LookupId>>::new();
        for pat in &patterns {
            if let Some(lookup_table) = &pat.lookup_table {
                if let Some(attrs) = lookup_map.get_mut(lookup_table) {
                    attrs.push(pat.attribute_id.clone());
                } else {
                    lookup_map.insert(lookup_table.clone(), vec![pat.attribute_id.clone()]);
                }
            }
        }
        let lookup = self.db.get_lookup(lookup_map).await?;

        //
        // Apply resolved lookup names to patterns
        //
        let resolved_patterns = patterns
            .into_iter()
            .map(|pat| {
                let resolved = if let Some(lookup_table) = &pat.lookup_table
                    && let Some(lookup_name) = lookup
                        .get(lookup_table)
                        .and_then(|inner| inner.get(&pat.attribute_id))
                {
                    lookup_name.clone()
                } else {
                    pat.attribute_id.clone()
                };

                ResolvedPattern {
                    pattern: pat,
                    resolved: resolved,
                }
            })
            .collect::<Vec<_>>();

        //
        // Group by resolved patterns to its original descriptors
        //
        let mut descriptor_patterns: HashMap<PatternDescriptor, Vec<ResolvedPattern>> =
            HashMap::new();
        for pat in resolved_patterns {
            let descriptor = PatternDescriptor {
                wmi: pat.pattern.wmi.clone(),
                model_year: pat.pattern.model_year,
                vds: pat.pattern.vds.clone(),
                vis: pat.pattern.vis.clone(),
            };

            if let Some(pats) = descriptor_patterns.get_mut(&descriptor) {
                pats.push(pat);
            } else {
                descriptor_patterns.insert(descriptor, vec![pat]);
            }
        }

        //
        // Sort descriptor patterns by weight or pattern code
        //
        descriptor_patterns
            .values_mut()
            .for_each(|resolved_patterns| {
                resolved_patterns.sort_by(|res1, res2| {
                    res2.pattern
                        .element_weight
                        .cmp(&res1.pattern.element_weight) // Descending
                        .then_with(|| res1.pattern.pattern.cmp(&res2.pattern.pattern)) // Ascending
                })
            });

        let descriptor_patterns = descriptor_patterns
            .into_iter()
            .map(|(descriptor, resolved_patterns)| {
                //
                // Find the most specific schema by looking at model patterns
                //
                let mut model_patterns = resolved_patterns
                    .iter()
                    .filter(|pat| pat.pattern.element_name == "Model")
                    .map(|pat| {
                        (
                            calculate_confidence(
                                &pat.pattern.pattern,
                                &format!("{}{}", &pat.pattern.vds, &pat.pattern.vis),
                            ),
                            pat,
                        )
                    })
                    .collect::<Vec<_>>();
                model_patterns.sort_by(|(co1, _), (co2, _)| co2.total_cmp(co1)); // Desc

                //
                // Get the most relevant schema name
                //
                let primary_schema = model_patterns
                    .get(0)
                    .map(|mp| mp.1.pattern.schema_name.clone());

                //
                // Format descriptor patterns
                //
                let resolved_patterns = resolved_patterns
                    .into_iter()
                    .map(|resolved_pattern| {
                        let is_vis_pattern = resolved_pattern.pattern.pattern.contains('|');
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
                                &resolved_pattern.pattern.pattern,
                                &resolved_pattern.pattern.vis.get(1..2).unwrap_or(""),
                            )
                        } else {
                            calculate_confidence(
                                &resolved_pattern.pattern.pattern,
                                &format!(
                                    "{}{}",
                                    &resolved_pattern.pattern.vds, &resolved_pattern.pattern.vis
                                ),
                            )
                        };

                        //
                        // Adjust confidence based on schema match for plant codes
                        //
                        let mut confidence = base_confidence;
                        if resolved_pattern
                            .pattern
                            .element_name
                            .to_lowercase()
                            .contains("plant")
                        {
                            if let Some(ps) = &primary_schema {
                                confidence = if resolved_pattern.pattern.schema_name == *ps {
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
                        let actual_pattern = resolved_pattern
                            .pattern
                            .pattern
                            .split('|')
                            .nth(0)
                            .unwrap_or(&resolved_pattern.pattern.pattern);
                        let start_pos = if is_vis_pattern { 9 } else { 3 };
                        actual_pattern.chars().enumerate().for_each(|(idx, c)| {
                            if c != '|' {
                                positions.push(start_pos + idx);
                            }
                        });

                        RawPattern {
                            pattern: resolved_pattern.pattern,
                            resolved: resolved_pattern.resolved,
                            confidence,
                            positions,
                            pattern_type,
                        }
                    })
                    .collect::<Vec<_>>();
                (descriptor, resolved_patterns)
            })
            .collect::<HashMap<_, _>>();

        Ok(descriptor_patterns)
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

impl From<RawPattern> for PatternMatch {
    fn from(value: RawPattern) -> Self {
        Self {
            element: value.pattern.element_name,
            element_code: value.pattern.element_code,
            attribute_id: value.pattern.attribute_id,
            resolved: value.resolved,
            confidence: value.confidence,
            positions: value.positions,
            schema_name: value.pattern.schema_name,
            metadata: Some(Metadata {
                lookup_table_name: value.pattern.lookup_table,
                group_name: value.pattern.group_name,
                element_weight: value.pattern.element_weight,
                pattern_type: value.pattern_type,
                raw_pattern: value.pattern.pattern,
                match_details: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn test_pattern_raw() {
        let matcher = PatternMatcher::new_with_default_db().await;
        let v = vec![
            PatternDescriptor {
                wmi: "2C3".to_string(),
                vds: "CDXBG1".to_string(),
                vis: "FH832587".to_string(),
                model_year: 2015,
            },
            PatternDescriptor {
                wmi: "SCB".to_string(),
                vds: "BR9ZA8".to_string(),
                vis: "DC079455".to_string(),
                model_year: 2013,
            },
            PatternDescriptor {
                wmi: "5NP".to_string(),
                vds: "D74LF7".to_string(),
                vis: "HH126052".to_string(),
                model_year: 2017,
            },
            PatternDescriptor {
                wmi: "1HD".to_string(),
                vds: "1KHM11".to_string(),
                vis: "CB675783".to_string(),
                model_year: 2012,
            },
            PatternDescriptor {
                wmi: "3KP".to_string(),
                vds: "F24AD6".to_string(),
                vis: "PE638817".to_string(),
                model_year: 2023,
            },
            PatternDescriptor {
                wmi: "1HG".to_string(),
                vds: "CP2673".to_string(),
                vis: "9A060971".to_string(),
                model_year: 2009,
            },
        ];
        let v = vec![PatternDescriptor {
            wmi: "1HG".to_string(),
            vds: "CP2673".to_string(),
            vis: "9A060971".to_string(),
            model_year: 2009,
        }];
        let map = matcher.matches(v).await.unwrap();
    }
}
