use std::{fs::File, marker::PhantomData, path::Path};

use fst::Map;
use memmap2::Mmap;
use rkyv::{deserialize, rancor::Error, vec::ArchivedVec};

use crate::{RkyvDeserialize, RkyvSerialize, Saveable};

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
    /// Loads the `.fst` index and `.bin` values from `MAPS_DIR` (or `$HOME/.corgi-rs-cache`).
    pub fn new<'a>() -> Self
    where
        R: Saveable<'a>,
    {
        let out_dir = std::env::var("MAPS_DIR")
            .map(|v| std::path::PathBuf::from(v))
            .unwrap_or(
                dirs::home_dir()
                    .expect("HOME env variable not set")
                    .join(".corgi-rs-cache"),
            );

        let values_path = Path::new(&out_dir).join(format!("{}.bin", R::base_file_name()));
        let values_file = File::open(&values_path).expect("values file open");
        let values_memmap = unsafe { Mmap::map(&values_file).expect("memmap create") };

        let fst_path = Path::new(&out_dir).join(format!("{}.fst", R::base_file_name()));
        let fst_file = File::open(&fst_path).expect("fst file open");
        let fst_memmap = unsafe { Mmap::map(&fst_file).expect("memmap create") };
        let fst_map = Map::new(fst_memmap).expect("fst map create");

        Self {
            fst_map,
            values_memmap,
            _phantom: PhantomData,
        }
    }

    /// Get the cached entries registered under `key`.
    ///
    /// Returns `None` when the key is missing, otherwise returns a fresh `Vec<R>`.
    pub fn get(&self, key: &str) -> Option<Vec<R>> {
        if let Some(offset_len_combined) = self.fst_map.get(key) {
            let offset = (offset_len_combined >> 32) as usize;
            let len = (offset_len_combined & 0xFFFFFFFF) as usize;

            let archived = unsafe {
                rkyv::access_unchecked::<ArchivedVec<R::Archived>>(
                    &self.values_memmap[offset..offset + len],
                )
            };

            let deserialized = deserialize::<Vec<R>, Error>(archived).ok();
            return deserialized;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use crate::{Lookup, Make, SchemaId};

    use super::*;

    #[test]
    fn test_fst_lookup_map() {
        let map = FstRkyvMap::<Lookup>::new();
        let lookup = map.get("10000").expect("map get");
        assert!(lookup.contains(&Lookup {
            pattern: "AJ".to_string(),
            element_code: "Model".to_string(),
            resolved: "L3337".to_string(),
            element_weight: Some(99)
        }));

        let lookup = map.get("9999").expect("map get");
        assert!(lookup.contains(&Lookup {
            pattern: "8Z5C5".to_string(),
            element_code: "OtherEngineInfo".to_string(),
            resolved: "Emis Std: SULEV-PZEV".to_string(),
            element_weight: None
        }));
    }

    #[test]
    fn test_fst_make_map() {
        let map = FstRkyvMap::<Make>::new();
        let make = map.get("102").expect("map get");
        assert_eq!(
            make,
            vec![Make {
                make: "CAMELOT".to_string(),
            }]
        );

        let make = map.get("ZZ3").expect("map get");
        assert_eq!(
            make,
            vec![Make {
                make: "MC MOTO".to_string(),
            }]
        );
    }

    #[test]
    fn test_fst_schema_id_map() {
        let map = FstRkyvMap::<SchemaId>::new();
        let schema_id = map.get("ZZ3").expect("map get");
        assert_eq!(
            schema_id,
            vec![SchemaId {
                schema_id: "25078".to_string()
            }]
        );

        let schema_id = map.get("101").expect("map get");
        assert_eq!(
            schema_id,
            vec![SchemaId {
                schema_id: "6746".to_string()
            }]
        );
    }
}
