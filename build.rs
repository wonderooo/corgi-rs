//! Turn the compressed vPIC assets into memory-mappable lookup tables.
//!
//! `assets/*.tsv.zst` are tab-separated exports of the NHTSA vPIC database (see
//! `tools/extract_assets.sql`), sorted by their first column. For each one this
//! writes a pair of files into `$HOME/.corgi-rs-cache`:
//!
//! - `<table>.fst` — an fst map from key to a packed `(offset, length)`,
//! - `<table>.bin` — the rkyv-archived `Vec<Row>` for each key, concatenated.
//!
//! The tables are rebuilt whenever the assets change, tracked through a stamp
//! file so a fresh checkout does not silently keep a previous crate version's
//! tables.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use fst::MapBuilder;
use rkyv::rancor::Error;

use crate::build_shared::{
    DefaultValueRow, ElementMeta, EngineModelRow, GenerationRow, ModelEntry, PatternRow,
    RkyvSerialize, Saveable, SchemaRef, SpecRow, UntilNextKey, WmiEntry,
};

#[path = "src/build_shared.rs"]
mod build_shared;

/// Bump when the on-disk table layout changes in a way old caches cannot serve.
const TABLE_FORMAT_VERSION: u32 = 2;

fn main() {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let out_dir = cache_dir();
    std::fs::create_dir_all(&out_dir).expect("create the lookup table directory");

    let stamp_path = out_dir.join("assets.stamp");
    let stamp = asset_stamp(&assets);
    let up_to_date = std::fs::read_to_string(&stamp_path).is_ok_and(|found| found == stamp);

    if !up_to_date {
        // Build the tables in parallel; the largest one dominates the wall clock.
        std::thread::scope(|scope| {
            let assets = &assets;
            let out_dir = &out_dir;
            let mut handles = Vec::new();

            macro_rules! build {
                ($row:ty) => {
                    handles.push(scope.spawn(move || build_table::<$row>(assets, out_dir)));
                };
            }

            build!(WmiEntry);
            build!(SchemaRef);
            build!(PatternRow);
            build!(ModelEntry);
            build!(SpecRow);
            build!(EngineModelRow);
            build!(DefaultValueRow);
            build!(GenerationRow);

            for handle in handles {
                handle.join().expect("build a lookup table");
            }
        });

        remove_obsolete_tables(&out_dir);
        std::fs::write(&stamp_path, &stamp).expect("write the asset stamp");
    }

    generate_static_tables(&assets);

    // Track the stamp itself, so wiping the cache directory is enough to make
    // cargo re-run this script. A `rerun-if-changed` path that does not exist
    // forces a re-run, which is exactly the behaviour we want here.
    println!("cargo:rerun-if-changed={}", stamp_path.display());
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=src/build_shared.rs");
    println!("cargo:rerun-if-env-changed=CORGI_CACHE_DIR");
}

/// Where the generated tables go. Mirrors `corgi_rs::maps::maps_dir`.
fn cache_dir() -> PathBuf {
    std::env::var_os("CORGI_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".corgi-rs-cache")))
        .expect("HOME is not set; point CORGI_CACHE_DIR at a writable directory")
}

/// Tables shipped by earlier crate versions, which nothing reads any more.
const OBSOLETE_TABLES: [&str; 3] = ["schema_id_lookup", "wmi_make", "wmi_schema_id"];

/// Delete tables a previous version of the crate left in the cache directory.
/// They are tens of megabytes each and would otherwise linger forever.
fn remove_obsolete_tables(out_dir: &Path) {
    for table in OBSOLETE_TABLES {
        for extension in ["fst", "bin"] {
            let _ = std::fs::remove_file(out_dir.join(format!("{table}.{extension}")));
        }
    }
}

/// A fingerprint of the asset inputs, so stale tables get rebuilt.
fn asset_stamp(assets: &Path) -> String {
    let mut entries: Vec<String> = std::fs::read_dir(assets)
        .expect("read the assets directory")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            Some(format!("{}:{}", entry.file_name().display(), meta.len()))
        })
        .collect();
    entries.sort();
    format!("v{TABLE_FORMAT_VERSION}\n{}", entries.join("\n"))
}

/// Read and decompress `assets/<name>.tsv.zst`.
fn read_asset(assets: &Path, name: &str) -> String {
    let path = assets.join(format!("{name}.tsv.zst"));
    let file =
        File::open(&path).unwrap_or_else(|err| panic!("open the asset {}: {err}", path.display()));

    let mut text = String::new();
    zstd::Decoder::new(file)
        .unwrap_or_else(|err| panic!("decompress {}: {err}", path.display()))
        .read_to_string(&mut text)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));

    text
}

/// Build one `.fst`/`.bin` pair from the asset that names it.
fn build_table<'a, R>(assets: &Path, out_dir: &Path)
where
    R: RkyvSerialize + FromStr + Saveable<'a>,
    <R as FromStr>::Err: std::fmt::Display,
{
    let table = R::base_file_name();
    let text = read_asset(assets, &table);

    let fst_path = out_dir.join(format!("{table}.fst"));
    let values_path = out_dir.join(format!("{table}.bin"));

    let mut fst_builder = MapBuilder::new(BufWriter::new(
        File::create(&fst_path).expect("create the fst file"),
    ))
    .expect("start the fst map");
    let mut values = BufWriter::new(File::create(&values_path).expect("create the values file"));

    let mut offset = 0u64;
    let mut lines = text.lines().peekable();

    while let Some((key, rows)) = lines.next_key() {
        let rows: Vec<R> = rows
            .into_iter()
            .map(|row| {
                R::from_str(row)
                    .unwrap_or_else(|err| panic!("parse a row of {table}.tsv (`{row}`): {err}"))
            })
            .collect();

        let bytes = rkyv::to_bytes::<Error>(&rows)
            .unwrap_or_else(|err| panic!("archive the rows for {table} key `{key}`: {err}"));
        values.write_all(&bytes).expect("write archived rows");

        // fst values are u64, so pack the record's location into one: offset in
        // the high half, length in the low half.
        assert!(
            bytes.len() <= u32::MAX as usize,
            "{table} key `{key}` archives to more than 4 GiB"
        );
        fst_builder
            .insert(key, (offset << 32) | bytes.len() as u64)
            .unwrap_or_else(|err| {
                panic!("insert {table} key `{key}` (are the asset rows sorted by key?): {err}")
            });

        offset += bytes.len() as u64;
        assert!(
            offset <= u32::MAX as u64,
            "{table}.bin grew past 4 GiB, which the packed fst value cannot address"
        );
    }

    fst_builder.finish().expect("finish the fst map");
    values.flush().expect("flush the values file");
}

/// Emit the element and vehicle-type tables as Rust source, so the decoder can
/// name its output without a runtime lookup.
fn generate_static_tables(assets: &Path) {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let mut source = String::from(
        "// @generated by build.rs from assets/element.tsv.zst and \
         assets/vehicle_type.tsv.zst. Do not edit.\n\n",
    );

    let elements = read_asset(assets, "element");
    let mut rows: Vec<ElementMeta> = elements
        .lines()
        .map(|line| {
            ElementMeta::from_str(line)
                .unwrap_or_else(|err| panic!("parse a row of element.tsv (`{line}`): {err}"))
        })
        .collect();
    rows.sort_by_key(|element| element.id);

    source.push_str("static ELEMENTS: &[Element] = &[\n");
    for element in &rows {
        source.push_str(&format!(
            "    Element {{ id: {}, code: {}, name: {}, data_type: {}, group: {} }},\n",
            element.id,
            quote(&element.code),
            quote(&element.name),
            quote(&element.data_type),
            quote(element.group.as_deref().unwrap_or("")),
        ));
    }
    source.push_str("];\n\n");

    let vehicle_types = read_asset(assets, "vehicle_type");
    source.push_str("static VEHICLE_TYPES: &[(u16, &str)] = &[\n");
    for line in vehicle_types.lines() {
        let mut columns = line.split('\t');
        let (Some(id), Some(name)) = (columns.next(), columns.next()) else {
            continue;
        };
        source.push_str(&format!("    ({id}, {}),\n", quote(name)));
    }
    source.push_str("];\n");

    std::fs::write(out_dir.join("elements.rs"), source).expect("write the generated element table");
}

/// Render `value` as a Rust string literal.
fn quote(value: &str) -> String {
    format!("{value:?}")
}
