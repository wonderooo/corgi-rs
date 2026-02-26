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

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
/// A single decoded lookup pattern that ties a VIN pattern to a resolved value.
pub struct Lookup {
    /// The raw VIN pattern.
    pub pattern: String,
    /// The schema field name, e.g. `Model` or `FuelTypePrimary`.
    pub element_code: String,
    /// The human-readable value derived from the pattern entry.
    pub resolved: String,
    /// Optional weight used to prefer more specific lookups.
    pub element_weight: Option<usize>,
}

impl RkyvDeserialize<Lookup> for ArchivedLookup {}
impl RkyvSerialize for Lookup {}

impl<'a> Saveable<'a> for Lookup {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("schema_id_lookup")
    }
}

impl FromStr for Lookup {
    type Err = std::io::Error;

    /// Parse a CSV line into a [`Lookup`].
    ///
    /// # Examples
    ///
    /// ```
    /// use corgi_rs::build_shared::Lookup;
    /// let lookup: Lookup = "AJ,Model,L3337,99".parse().unwrap();
    /// assert_eq!(lookup.element_code, "Model");
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split(',').collect::<Vec<_>>();
        if let Some(pattern) = parts.get(0)
            && let Some(element_code) = parts.get(1)
            && let Some(resolved) = parts.get(2)
        {
            let element_weight = parts.get(3).map(|ew| ew.parse::<usize>().ok()).flatten();
            return Ok(Lookup {
                pattern: pattern.to_string(),
                element_code: element_code.to_string(),
                resolved: resolved.to_string(),
                element_weight,
            });
        };

        Err(std::io::Error::other(
            "could not construct Lookup struct from given str",
        ))
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
/// A single WMI make entry extracted from the official master data.
pub struct Make {
    /// Manufacturer name such as `FORD` or `TESLA`.
    pub make: String,
}

impl RkyvDeserialize<Make> for ArchivedMake {}
impl RkyvSerialize for Make {}

impl<'a> Saveable<'a> for Make {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("wmi_make")
    }
}

impl FromStr for Make {
    type Err = std::io::Error;

    /// Parse a raw make string into [`Make`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Make {
            make: s.to_string(),
        })
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
/// Schema identifier used to group lookups that belong to the same VIN definition.
pub struct SchemaId {
    /// Actual schema identifier string supplied by the master data.
    pub schema_id: String,
}

impl RkyvDeserialize<SchemaId> for ArchivedSchemaId {}
impl RkyvSerialize for SchemaId {}

impl<'a> Saveable<'a> for SchemaId {
    fn base_file_name() -> Cow<'a, str> {
        Cow::Borrowed("wmi_schema_id")
    }
}

impl FromStr for SchemaId {
    type Err = std::io::Error;

    /// Turn a plain schema ID string into a typed [`SchemaId`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(SchemaId {
            schema_id: s.to_string(),
        })
    }
}

/// Utility trait that helps parse CSV tables grouped by their leading key.
pub trait UntilNextKey<'a> {
    /// Returns the next key and collection of rows until the key changes.
    fn next_key(&mut self) -> Option<(&'a str, Vec<&'a str>)>;
}

impl<'a, I> UntilNextKey<'a> for Peekable<I>
where
    I: Iterator<Item = &'a str>,
{
    fn next_key(&mut self) -> Option<(&'a str, Vec<&'a str>)> {
        let mut current_key = None;
        let mut values = Vec::new();

        while let Some(line) = self.peek() {
            let (key, rest) = line.split_once(',').expect("must have comma");

            match current_key {
                Some(ck) if ck != key => {
                    return Some((current_key.expect("must have been set"), values));
                }
                None => {
                    current_key = Some(key);
                }
                Some(_) => {}
            };

            values.push(rest);
            self.next();
        }

        if let Some(key) = current_key {
            Some((key, values))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::UntilNextKey;

    #[test]
    fn test_until_next_key() {
        let csv = include_str!("../assets/wmi_make.csv");
        let mut iter = csv.lines().skip(1).peekable();
        let (k1, v1) = iter.next_key().expect("must be some");
        let (k2, v2) = iter.next_key().expect("must be some");

        assert_eq!(k1, "101");
        assert_eq!(v1, vec!["Mo Trailers Corp."]);
        assert_eq!(k2, "102");
        assert_eq!(v2, vec!["CAMELOT"]);
    }

    #[test]
    fn test_until2_next_key() {
        let csv = include_str!("../assets/wmi_make.csv");
        let mut iter = csv.lines().skip(1).peekable();

        let mut last_key = "";
        while let Some((k, _)) = iter.next_key() {
            last_key = k
        }
        assert_eq!(last_key, "ZZ3");
    }
}

#[cfg(test)]
mod parse_tests {
    use super::Lookup;

    #[test]
    fn lookup_from_str_parses_weight() {
        let lookup: Lookup = "AJ,Model,F-150,99".parse().expect("parse");
        assert_eq!(lookup.element_code, "Model");
        assert_eq!(lookup.element_weight, Some(99));
    }

    #[test]
    fn lookup_from_str_handles_missing_weight() {
        let lookup: Lookup = "AJ,Model,F-150".parse().expect("parse");
        assert_eq!(lookup.element_weight, None);
    }

    #[test]
    fn lookup_from_str_errors_on_short_input() {
        let err = "only,one".parse::<Lookup>().unwrap_err();
        assert!(
            err.to_string().contains("could not construct Lookup"),
            "{err}"
        );
    }
}
