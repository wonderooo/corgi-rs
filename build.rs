use std::collections::HashMap;
use std::io::Write;
use std::{fs::File, path::Path};

use flate2::read::GzDecoder;

static DB_ARCHIVE: &str = "https://corgi.cardog.io/vpic.lite.db.gz";
static DB_FILE: &str = ".corgi-rs-cache/vpic.lite.db";

fn main() {
    let home_dir = dirs::home_dir().expect("home directory env variable not set");
    let db_file_path = home_dir.join(DB_FILE);

    if !db_file_path.exists() {
        if let Some(parent) = db_file_path.parent() {
            std::fs::create_dir_all(parent).expect("db directory create failure");
        }
        let mut file_writer = std::fs::File::create(db_file_path).expect("db file create failure");

        let mut archive_response = ureq::get(DB_ARCHIVE)
            .call()
            .expect("db archive download failure");
        let body_reader = archive_response.body_mut().as_reader();

        let mut gz_decoder = GzDecoder::new(body_reader);
        std::io::copy(&mut gz_decoder, &mut file_writer).expect("reader writer copy failure");
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR env variable not set");
    let gen_path = Path::new(&out_dir).join("gen.rs");
    if !gen_path.exists() {
        let mut gen_file = File::create(&gen_path).expect("gen file create failure");
        generate_wmi_make_map(&mut gen_file).expect("gen map failure");
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR env variable not set");
    let gen_path = Path::new(&out_dir).join("gen_wmi_schema_id.rs");
    if !gen_path.exists() {
        let mut gen_file = File::create(&gen_path).expect("gen file create failure");
        generate_wmi_schema_id_map(&mut gen_file).expect("gen map failure");
    }
}

fn generate_wmi_make_map(file: &mut File) -> Result<(), std::io::Error> {
    let wmi_make_csv = include_str!("assets/wmi_make.csv");
    let wmi_make_inner_map = wmi_make_csv
        .lines()
        .skip(1)
        .map(|line| line.split_once(',').expect("must have comma"))
        .collect::<HashMap<_, _>>()
        .into_iter()
        .map(|(k, v)| format!(r#""{k}" => "{}","#, v.replace('"', "")))
        .collect::<Vec<_>>()
        .join("\n");

    let wmi_make_map = format!(
        r#"pub static WMI_MAKE_MAP: phf::Map<&'static str, &'static str> = phf::phf_map!{{{wmi_make_inner_map}}};"#
    );
    writeln!(file, "{}", wmi_make_map)?;
    Ok(())
}

fn generate_wmi_schema_id_map(file: &mut File) -> Result<(), std::io::Error> {
    let wmi_schema_id_csv = include_str!("assets/wmi_schema_id.csv");

    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for line in wmi_schema_id_csv.lines().skip(1) {
        let (k, v) = line.split_once(',').expect("must have comma");
        map.entry(k.to_string())
            .or_default()
            .push(v.replace('"', ""));
    }

    let wmi_schema_id_inner_map = map
        .into_iter()
        .map(|(k, values)| {
            let values = values
                .into_iter()
                .map(|v| format!(r#""{v}""#))
                .collect::<Vec<_>>();
            let values = values.join(", ");
            format!(r#""{k}" => &[{}],"#, values)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let wmi_schema_id_map = format!(
        r#"pub static WMI_SCHEMA_ID_MAP: phf::Map<&'static str, &'static [&'static str]> = phf::phf_map!{{{wmi_schema_id_inner_map}}};"#
    );
    writeln!(file, "{}", wmi_schema_id_map)?;
    Ok(())
}
