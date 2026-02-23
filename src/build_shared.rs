use std::{borrow::Cow, iter::Peekable, str::FromStr};

use rkyv::{
    Archive, Deserialize, Serialize,
    api::high::{HighDeserializer, HighSerializer},
    rancor::Error,
    ser::allocator::ArenaHandle,
    util::AlignedVec,
};

#[allow(dead_code)]
pub trait RkyvDeserialize<D>: Deserialize<D, HighDeserializer<Error>> {}

pub trait RkyvSerialize:
    for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, Error>>
{
}

pub trait Saveable<'a> {
    fn base_file_name() -> Cow<'a, str>;
}

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
pub struct Lookup {
    pub pattern: String,
    pub element_code: String,
    pub resolved: String,
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
pub struct Make {
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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Make {
            make: s.to_string(),
        })
    }
}

#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct SchemaId {
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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(SchemaId {
            schema_id: s.to_string(),
        })
    }
}

pub trait UntilNextKey<'a> {
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

        assert_eq!(k1, "SCF");
        assert_eq!(v1, vec!["Aston Martin"]);
        assert_eq!(k2, "SAJ");
        assert_eq!(v2, vec!["Jaguar"]);
    }

    #[test]
    fn test_until2_next_key() {
        let csv = include_str!("../assets/wmi_make.csv");
        let mut iter = csv.lines().skip(1).peekable();

        let mut last_key = "";
        while let Some((k, _)) = iter.next_key() {
            last_key = k
        }
        assert_eq!(last_key, "4C9753");
    }
}
