//! Memory-mapped lookup tables.
//!
//! Each asset is an `fst` map from key to a `(offset, length)` pair packed into
//! a `u64`, addressing a `rkyv`-archived `Vec<R>` inside a companion `.bin`
//! file. Both files are mapped, never read into the heap, so opening every
//! table costs a few page faults rather than the 55 MB the tables occupy.

use std::{fmt::Display, fs::File, marker::PhantomData, path::Path, path::PathBuf};

use fst::Map;
use memmap2::Mmap;
use rkyv::{deserialize, rancor::Error, vec::ArchivedVec};

use crate::{RkyvDeserialize, RkyvSerialize, Saveable};

/// Where the generated `.fst`/`.bin` tables live.
///
/// `MAPS_DIR` overrides it; otherwise `$HOME/.corgi-rs-cache`, which is where
/// `build.rs` writes them.
pub fn maps_dir() -> PathBuf {
    std::env::var_os("MAPS_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".corgi-rs-cache")))
        .unwrap_or_else(|| PathBuf::from(".corgi-rs-cache"))
}

/// A lookup table failed to open.
#[derive(Debug)]
pub struct MapError {
    /// Base name of the table, e.g. `pattern`.
    pub table: String,
    pub path: PathBuf,
    pub source: std::io::Error,
}

impl Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not open the `{}` lookup table at {}: {}. \
             Run `cargo build` to regenerate it, or point MAPS_DIR at a directory that has it.",
            self.table,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for MapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Wrapper around `fst::Map` + `rkyv` arrays that lazily read serialized lookup data.
pub struct FstRkyvMap<R>
where
    R: RkyvSerialize,
    R::Archived: RkyvDeserialize<R>,
{
    fst_map: Map<Mmap>,
    values_memmap: Mmap,
    _phantom: PhantomData<fn() -> R>,
}

impl<R> FstRkyvMap<R>
where
    R: RkyvSerialize,
    R::Archived: RkyvDeserialize<R>,
{
    /// Open the table for `R` from [`maps_dir`].
    ///
    /// # Panics
    ///
    /// Panics if the table is missing or unreadable. Use [`FstRkyvMap::open`]
    /// to handle that yourself.
    pub fn new<'a>() -> Self
    where
        R: Saveable<'a>,
    {
        Self::open().unwrap_or_else(|err| panic!("{err}"))
    }

    /// Open the table for `R` from [`maps_dir`].
    pub fn open<'a>() -> Result<Self, MapError>
    where
        R: Saveable<'a>,
    {
        let dir = maps_dir();
        let table = R::base_file_name().into_owned();

        let values_memmap = map_file(&dir, &table, "bin")?;
        let fst_memmap = map_file(&dir, &table, "fst")?;
        let fst_map = Map::new(fst_memmap).map_err(|err| MapError {
            table: table.clone(),
            path: dir.join(format!("{table}.fst")),
            source: std::io::Error::other(err),
        })?;

        Ok(Self {
            fst_map,
            values_memmap,
            _phantom: PhantomData,
        })
    }

    /// Get the entries registered under `key`, or `None` when the key is absent.
    pub fn get(&self, key: &str) -> Option<Vec<R>> {
        let offset_len = self.fst_map.get(key)?;
        let offset = (offset_len >> 32) as usize;
        let len = (offset_len & 0xFFFF_FFFF) as usize;

        let bytes = self.values_memmap.get(offset..offset + len)?;
        // Safety: build.rs wrote these bytes with the same rkyv version and
        // layout, and the fst only ever addresses records it wrote.
        let archived = unsafe { rkyv::access_unchecked::<ArchivedVec<R::Archived>>(bytes) };

        deserialize::<Vec<R>, Error>(archived).ok()
    }

    /// Whether `key` is present, without deserializing its entries.
    pub fn contains_key(&self, key: &str) -> bool {
        self.fst_map.get(key).is_some()
    }

    /// Number of distinct keys in the table.
    pub fn len(&self) -> usize {
        self.fst_map.len()
    }

    /// Whether the table holds no keys at all.
    pub fn is_empty(&self) -> bool {
        self.fst_map.is_empty()
    }
}

fn map_file(dir: &Path, table: &str, extension: &str) -> Result<Mmap, MapError> {
    let path = dir.join(format!("{table}.{extension}"));
    let file = File::open(&path).map_err(|source| MapError {
        table: table.to_string(),
        path: path.clone(),
        source,
    })?;

    // Safety: the tables are written once by build.rs and only read afterwards.
    unsafe { Mmap::map(&file) }.map_err(|source| MapError {
        table: table.to_string(),
        path,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelEntry, PatternRow, SchemaRef, WmiEntry};

    #[test]
    fn wmi_table_resolves_a_shared_wmi_without_committing_to_a_make() {
        let map = FstRkyvMap::<WmiEntry>::new();
        let entries = map.get("1C4").expect("1C4 is a known WMI");
        let wmi = &entries[0];
        // 1C4 is shared by Jeep, Ram, Dodge, Chrysler and Fiat, so it must not
        // name a make on its own.
        assert_eq!(wmi.make_id, 0);
        assert!(wmi.make.is_empty());
        assert_eq!(wmi.manufacturer, "FCA US LLC");
        assert_eq!(wmi.vehicle_type_id, 7);
    }

    #[test]
    fn wmi_table_names_the_make_when_it_is_unambiguous() {
        let map = FstRkyvMap::<WmiEntry>::new();
        let entries = map.get("1HG").expect("1HG is a known WMI");
        assert_eq!(entries[0].make, "Honda");
        assert_ne!(entries[0].make_id, 0);
    }

    #[test]
    fn schema_table_carries_year_windows() {
        let map = FstRkyvMap::<SchemaRef>::new();
        let schemas = map.get("1C4").expect("1C4 has schemas");
        assert!(schemas.len() > 1, "a long-lived WMI has many schemas");
        assert!(schemas.iter().any(|s| s.covers(2015)));
        assert!(
            schemas.iter().any(|s| !s.covers(2015)),
            "and some of them must not apply to 2015"
        );
    }

    #[test]
    fn model_table_maps_a_model_to_exactly_one_make() {
        let map = FstRkyvMap::<ModelEntry>::new();
        let entries = map.get("1685").expect("model 1685");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "Model S");
        assert_eq!(entries[0].make, "Tesla");
    }

    #[test]
    fn pattern_table_returns_rows_for_a_schema() {
        let map = FstRkyvMap::<PatternRow>::new();
        let schemas = FstRkyvMap::<SchemaRef>::new()
            .get("1HG")
            .expect("1HG has schemas");
        let schema = schemas
            .iter()
            .find(|s| s.covers(2009))
            .expect("2009 schema");
        let rows = map
            .get(&schema.schema_id.to_string())
            .expect("schema has patterns");
        assert!(!rows.is_empty());
    }

    #[test]
    fn unknown_keys_return_none() {
        let map = FstRkyvMap::<WmiEntry>::new();
        assert!(map.get("ZZZZZZ").is_none());
        assert!(!map.contains_key("ZZZZZZ"));
    }
}
