use std::io::{BufWriter, Write};
use std::str::FromStr;
use std::{fs::File, path::Path};

use fst::MapBuilder;

use build_shared::{Lookup, UntilNextKey};
use rkyv::rancor::Error;

use crate::build_shared::{Make, RkyvSerialize, Saveable, SchemaId};

#[path = "src/build_shared.rs"]
mod build_shared;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR env variable not set");

    let lookup_fst_path = Path::new(&out_dir).join(format!("{}.fst", Lookup::base_file_name()));
    let lookup_values_path = Path::new(&out_dir).join(format!("{}.bin", Lookup::base_file_name()));
    let lookup_task_handle = if !lookup_fst_path.exists() || !lookup_values_path.exists() {
        let fst_file = File::create(&lookup_fst_path).expect("fst file create");
        let values_file = File::create(&lookup_values_path).expect("values file create");

        let handle = std::thread::spawn(|| {
            generate_fst_map::<Lookup>(
                &Path::new(&format!("assets/{}.csv", Lookup::base_file_name())),
                fst_file,
                values_file,
            )
            .expect("generate fst lookup");
        });

        Some(handle)
    } else {
        None
    };

    let make_fst_path = Path::new(&out_dir).join(format!("{}.fst", Make::base_file_name()));
    let make_values_path = Path::new(&out_dir).join(format!("{}.bin", Make::base_file_name()));
    let make_task_handle = if !make_fst_path.exists() || !make_values_path.exists() {
        let fst_file = File::create(&make_fst_path).expect("fst file create");
        let values_file = File::create(&make_values_path).expect("values file create");

        let handle = std::thread::spawn(|| {
            generate_fst_map::<Make>(
                &Path::new(&format!("assets/{}.csv", Make::base_file_name())),
                fst_file,
                values_file,
            )
            .expect("generate fst make")
        });

        Some(handle)
    } else {
        None
    };

    let schema_fst_path = Path::new(&out_dir).join(format!("{}.fst", SchemaId::base_file_name()));
    let schema_values_path =
        Path::new(&out_dir).join(format!("{}.bin", SchemaId::base_file_name()));
    let schema_task_handle = if !schema_fst_path.exists() || !schema_values_path.exists() {
        let fst_file = File::create(&schema_fst_path).expect("fst file create");
        let values_file = File::create(&schema_values_path).expect("values file create");

        let handle = std::thread::spawn(|| {
            generate_fst_map::<SchemaId>(
                &Path::new(&format!("assets/{}.csv", SchemaId::base_file_name())),
                fst_file,
                values_file,
            )
            .expect("generate fst schema")
        });

        Some(handle)
    } else {
        None
    };

    lookup_task_handle.map(|h| h.join().expect("lookup task join"));
    make_task_handle.map(|h| h.join().expect("make task join"));
    schema_task_handle.map(|h| h.join().expect("schema task join"));

    println!("cargo:rerun-if-changed=src/build_shared.rs");
}

fn generate_fst_map<V>(
    csv_path: &Path,
    fst_file: File,
    values_file: File,
) -> Result<(), Box<dyn std::error::Error>>
where
    V: RkyvSerialize + FromStr,
{
    let csv = std::fs::read_to_string(csv_path)?;

    let fst_writer = BufWriter::new(fst_file);
    let mut fst_builder = MapBuilder::new(fst_writer)?;

    let mut values_writer = BufWriter::new(values_file);

    let mut current_offset = 0 as u64;

    // Skip header line
    let mut csv_lines = csv.lines().skip(1).peekable();
    while let Some((key, values)) = csv_lines.next_key() {
        let values = values
            .into_iter()
            .map(|v| {
                V::from_str(v)
                    .map_err(|_| std::io::Error::other("value from str"))
                    .expect("str parse")
            })
            .collect::<Vec<_>>();
        let bytes = rkyv::to_bytes::<Error>(&values)?;
        values_writer.write_all(&bytes)?;

        let offset_len_combined = (current_offset << 32) | bytes.len() as u64;
        fst_builder.insert(key, offset_len_combined)?;

        current_offset += bytes.len() as u64;
    }

    fst_builder.finish()?;
    values_writer.flush()?;

    Ok(())
}
