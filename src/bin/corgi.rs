//! `corgi` — decode VINs from the command line.
//!
//! ```sh
//! corgi 1C6RR7LT2JS179571
//! corgi --format json 5XYRLDLC3NG097496 | jq .Make
//! cut -d, -f2 lots.csv | corgi --format tsv --fields Make,Model,ModelYear,EngineHP
//! corgi --input vins.txt --format jsonl
//! ```

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, IsTerminal, Write};
use std::process::ExitCode;

use corgi_rs::{VehicleInfo, VinDecoder, element};

/// Keys the decoder derives rather than reading from a vPIC element. They share
/// the element-code namespace so `--fields` can name them like anything else.
const SYNTHETIC_KEYS: [&str; 9] = [
    "VIN",
    "BodyStyle",
    "Generation",
    "GenerationCode",
    "GenerationOrdinal",
    "MakeId",
    "ModelId",
    "ModelYearConclusive",
    "Warnings",
];

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("corgi: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let vins = match args.collect_vins() {
        Ok(vins) if vins.is_empty() => {
            eprintln!("corgi: no VINs given\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Ok(vins) => vins,
        Err(err) => {
            eprintln!("corgi: could not read VINs: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut decoder = match VinDecoder::try_new() {
        Ok(decoder) => decoder,
        Err(err) => {
            eprintln!("corgi: {err}");
            return ExitCode::FAILURE;
        }
    };
    decoder = decoder.require_check_digit(args.strict);
    if let Some(year) = args.year {
        decoder = decoder.with_current_year(year);
    }

    let results: Vec<(String, Result<VehicleInfo, String>)> = decoder
        .decode_all(&vins)
        .into_iter()
        .map(|result| result.map_err(|err| err.to_string()))
        .zip(&vins)
        .map(|(decoded, vin)| (vin.clone(), decoded))
        .collect();

    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    if let Err(err) = args.write(&mut out, &results) {
        // A closed pipe is how `| head` ends a stream, not a failure.
        if err.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("corgi: {err}");
            return ExitCode::FAILURE;
        }
    }
    let _ = out.flush();

    if results.iter().any(|(_, result)| result.is_err()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Read VINs from `reader`, one per line. Blank lines and anything after a `#`
/// are ignored, so a list can carry comments and a CSV comment header.
fn read_vins(reader: impl BufRead, out: &mut Vec<String>) -> std::io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        let vin = line.split('#').next().unwrap_or_default().trim();
        if !vin.is_empty() {
            out.push(vin.to_string());
        }
    }
    Ok(())
}

const USAGE: &str = "\
corgi — decode vehicle identification numbers

USAGE
    corgi [OPTIONS] [VIN]...

VINs are taken from the arguments and from `--input` files. If neither names
one, or if `-` is given, they are read from standard input. In a file or on
standard input they go one per line; blank lines and anything after a `#` are
ignored.

OPTIONS
    -i, --input FILE      read VINs from FILE, one per line (repeatable, `-` is stdin)
    -f, --format FORMAT   text (default), line, json, jsonl or tsv
        --fields LIST     comma-separated keys to output, e.g. Make,Model,EngineHP
        --list-fields     print every key that can be requested, and exit
        --strict          reject a VIN whose check digit does not match
        --year YEAR       the year to treat as current, for model-year decoding
        --no-warnings     leave decode warnings out of the output
    -h, --help            print this help
    -V, --version         print the version

FORMATS
    text    a block per VIN: the headline, then every decoded attribute
    line    one line per VIN, for skimming a list
    json    a single JSON array
    jsonl   one JSON object per line, for streaming
    tsv     a header row and one row per VIN

Keys are NHTSA element codes (`Make`, `EngineHP`, `PlantCity`, ...) plus VIN,
BodyStyle, Generation, GenerationCode, GenerationOrdinal, MakeId, ModelId,
ModelYearConclusive and Warnings. Matching is case-insensitive. Exit status is
1 if any VIN failed to decode.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Line,
    Json,
    JsonLines,
    Tsv,
}

struct Args {
    format: Format,
    fields: Option<Vec<String>>,
    vins: Vec<String>,
    /// Files to read VINs from; `-` means standard input.
    inputs: Vec<String>,
    read_stdin: bool,
    strict: bool,
    year: Option<i32>,
    warnings: bool,
}

impl Args {
    fn parse(argv: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = Args {
            format: Format::Text,
            fields: None,
            vins: Vec::new(),
            inputs: Vec::new(),
            read_stdin: false,
            strict: false,
            year: None,
            warnings: true,
        };

        let mut argv = argv.peekable();
        while let Some(arg) = argv.next() {
            let mut value = |flag: &str| argv.next().ok_or_else(|| format!("{flag} needs a value"));

            match arg.as_str() {
                "-h" | "--help" => {
                    println!("{USAGE}");
                    return Ok(None);
                }
                "-V" | "--version" => {
                    println!("corgi {}", env!("CARGO_PKG_VERSION"));
                    return Ok(None);
                }
                "--list-fields" => {
                    print_available_fields();
                    return Ok(None);
                }
                "-f" | "--format" => {
                    args.format = match value(&arg)?.to_ascii_lowercase().as_str() {
                        "text" => Format::Text,
                        "line" | "lines" => Format::Line,
                        "json" => Format::Json,
                        "jsonl" | "ndjson" => Format::JsonLines,
                        "tsv" => Format::Tsv,
                        other => return Err(format!("unknown format `{other}`")),
                    }
                }
                "--fields" => {
                    let list = value(&arg)?;
                    let fields: Vec<String> = list
                        .split(',')
                        .map(|field| field.trim().to_string())
                        .filter(|field| !field.is_empty())
                        .collect();
                    if fields.is_empty() {
                        return Err("--fields needs at least one key".to_string());
                    }
                    args.fields = Some(fields);
                }
                "--year" => {
                    args.year = Some(
                        value(&arg)?
                            .parse()
                            .map_err(|_| "--year needs a number".to_string())?,
                    )
                }
                "-i" | "--input" => args.inputs.push(value(&arg)?),
                "--strict" => args.strict = true,
                "--no-warnings" => args.warnings = false,
                "-" => args.read_stdin = true,
                other if other.starts_with('-') => {
                    return Err(format!("unknown option `{other}`"));
                }
                other => args.vins.push(other.to_string()),
            }
        }

        Ok(Some(args))
    }

    /// The VINs to decode, from the arguments, from `--input` files, or from
    /// standard input. Sources are concatenated in that order.
    fn collect_vins(&self) -> std::io::Result<Vec<String>> {
        let mut vins = self.vins.clone();

        for path in &self.inputs {
            if path == "-" {
                read_vins(std::io::stdin().lock(), &mut vins)?;
            } else {
                let file = File::open(path)
                    .map_err(|err| std::io::Error::new(err.kind(), format!("{path}: {err}")))?;
                read_vins(BufReader::new(file), &mut vins)?;
            }
        }

        // Arguments and files are explicit; only fall back to stdin when
        // neither named anything, or when `-` asked for it.
        if !vins.is_empty() && !self.read_stdin {
            return Ok(vins);
        }
        // Reading from an interactive terminal would just hang; treat that as
        // "no input" so the caller gets the usage message.
        if std::io::stdin().is_terminal() && !self.read_stdin {
            return Ok(vins);
        }
        if !self.inputs.is_empty() && !self.read_stdin {
            return Ok(vins);
        }

        read_vins(std::io::stdin().lock(), &mut vins)?;
        Ok(vins)
    }

    fn write(
        &self,
        out: &mut impl Write,
        results: &[(String, Result<VehicleInfo, String>)],
    ) -> std::io::Result<()> {
        match self.format {
            Format::Text => self.write_text(out, results),
            Format::Line => self.write_lines(out, results),
            Format::Json | Format::JsonLines => self.write_json(out, results),
            Format::Tsv => self.write_tsv(out, results),
        }
    }

    fn write_text(
        &self,
        out: &mut impl Write,
        results: &[(String, Result<VehicleInfo, String>)],
    ) -> std::io::Result<()> {
        for (index, (vin, result)) in results.iter().enumerate() {
            if index > 0 {
                writeln!(out)?;
            }

            let info = match result {
                Ok(info) => info,
                Err(err) => {
                    writeln!(out, "{vin}\n  cannot decode: {err}")?;
                    continue;
                }
            };

            writeln!(out, "{vin}")?;
            writeln!(
                out,
                "  {} {} {}",
                info.year,
                info.make,
                info.model.as_deref().unwrap_or("<unknown model>")
            )?;
            writeln!(
                out,
                "  {} / {}",
                info.body_style
                    .map(|style| style.to_string())
                    .unwrap_or_else(|| "<unknown body>".to_string()),
                info.vehicle_type.as_deref().unwrap_or("<unknown type>")
            )?;
            if let Some(generation) = &info.generation {
                writeln!(out, "  {generation}")?;
            }

            if self.warnings {
                for warning in &info.warnings {
                    writeln!(out, "  warning: {warning}")?;
                }
            }

            let record = self.select(vin, info);
            writeln!(out, "\n  {} attributes:", record.len())?;
            for (key, value) in &record {
                writeln!(out, "    {key:<36} {}", value.as_text())?;
            }
        }

        Ok(())
    }

    fn write_lines(
        &self,
        out: &mut impl Write,
        results: &[(String, Result<VehicleInfo, String>)],
    ) -> std::io::Result<()> {
        for (vin, result) in results {
            match result {
                Ok(info) => {
                    let engine = match (info.displacement_l, info.engine_cylinders) {
                        (Some(litres), Some(cylinders)) => format!("{litres:.1}L {cylinders}cyl"),
                        (Some(litres), None) => format!("{litres:.1}L"),
                        _ => "-".to_string(),
                    };
                    writeln!(
                        out,
                        "{vin}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        info.year,
                        info.make,
                        info.model.as_deref().unwrap_or("-"),
                        info.generation
                            .as_ref()
                            .map(|generation| generation.name.as_str())
                            .unwrap_or("-"),
                        info.trim.as_deref().unwrap_or("-"),
                        info.body_style
                            .map(|style| style.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                        engine,
                    )?;
                }
                Err(err) => writeln!(out, "{vin}\terror\t{err}")?,
            }
        }

        Ok(())
    }

    fn write_json(
        &self,
        out: &mut impl Write,
        results: &[(String, Result<VehicleInfo, String>)],
    ) -> std::io::Result<()> {
        let array = self.format == Format::Json;
        if array {
            writeln!(out, "[")?;
        }

        for (index, (vin, result)) in results.iter().enumerate() {
            let mut object = String::from("{");
            match result {
                Ok(info) => {
                    for (position, (key, value)) in self.select(vin, info).iter().enumerate() {
                        if position > 0 {
                            object.push(',');
                        }
                        object.push_str(&format!("{}:{}", json_string(key), value.as_json()));
                    }
                }
                Err(err) => {
                    object.push_str(&format!(
                        "{}:{},{}:{}",
                        json_string("VIN"),
                        json_string(vin),
                        json_string("Error"),
                        json_string(err)
                    ));
                }
            }
            object.push('}');

            if array {
                let comma = if index + 1 == results.len() { "" } else { "," };
                writeln!(out, "  {object}{comma}")?;
            } else {
                writeln!(out, "{object}")?;
            }
        }

        if array {
            writeln!(out, "]")?;
        }

        Ok(())
    }

    fn write_tsv(
        &self,
        out: &mut impl Write,
        results: &[(String, Result<VehicleInfo, String>)],
    ) -> std::io::Result<()> {
        // Explicit `--fields` fixes the columns. Otherwise take the union of
        // everything decoded, so no value is silently dropped.
        let columns: Vec<String> = match &self.fields {
            Some(fields) => fields.clone(),
            None => {
                let mut seen: BTreeMap<String, ()> = BTreeMap::new();
                for (vin, result) in results {
                    if let Ok(info) = result {
                        for (key, _) in self.select(vin, info) {
                            seen.insert(key, ());
                        }
                    }
                }
                let mut columns = vec!["VIN".to_string()];
                columns.extend(seen.into_keys().filter(|key| key != "VIN"));
                columns
            }
        };

        writeln!(out, "{}", columns.join("\t"))?;

        for (vin, result) in results {
            let record: BTreeMap<String, Value> = match result {
                Ok(info) => self.select(vin, info).into_iter().collect(),
                Err(_) => BTreeMap::from([("VIN".to_string(), Value::Text(vin.clone()))]),
            };

            let row: Vec<String> = columns
                .iter()
                .map(|column| {
                    record
                        .get(column)
                        .map(|value| tsv_cell(&value.as_text()))
                        .unwrap_or_default()
                })
                .collect();
            writeln!(out, "{}", row.join("\t"))?;
        }

        Ok(())
    }

    /// The key/value pairs to output for one decode, in the order the user
    /// asked for, or alphabetically with `VIN` first.
    fn select(&self, vin: &str, info: &VehicleInfo) -> Vec<(String, Value)> {
        let all = record(vin, info, self.warnings);

        match &self.fields {
            // An explicit request gets an explicit answer, even when the VIN
            // did not resolve the element, so every row has the same shape.
            Some(fields) => fields
                .iter()
                .map(|field| {
                    all.iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(field))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .unwrap_or_else(|| (field.clone(), Value::Missing))
                })
                .collect(),
            None => all,
        }
    }
}

/// A decoded value, typed well enough to render as JSON.
#[derive(Debug, Clone)]
enum Value {
    Text(String),
    Number(f64),
    Bool(bool),
    List(Vec<String>),
    /// Explicitly asked for with `--fields`, but this VIN did not resolve it.
    Missing,
}

impl Value {
    fn as_text(&self) -> String {
        match self {
            Value::Text(text) => text.clone(),
            Value::Number(number) => format_number(*number),
            Value::Bool(flag) => flag.to_string(),
            Value::List(items) => items.join("; "),
            Value::Missing => String::new(),
        }
    }

    fn as_json(&self) -> String {
        match self {
            Value::Text(text) => json_string(text),
            Value::Number(number) => format_number(*number),
            Value::Bool(flag) => flag.to_string(),
            Value::List(items) => {
                let items: Vec<String> = items.iter().map(|item| json_string(item)).collect();
                format!("[{}]", items.join(","))
            }
            Value::Missing => "null".to_string(),
        }
    }
}

/// Flatten a decode into the element-code namespace.
fn record(vin: &str, info: &VehicleInfo, warnings: bool) -> Vec<(String, Value)> {
    let mut fields = vec![("VIN".to_string(), Value::Text(vin.to_string()))];

    for (code, value) in &info.attributes {
        // Emit numbers as numbers where vPIC says the element is numeric, so
        // `jq` and spreadsheets do not have to guess.
        let numeric = element::by_code(code)
            .is_some_and(|element| matches!(element.data_type, "int" | "decimal"));
        let parsed = numeric.then(|| value.trim().parse::<f64>().ok()).flatten();

        fields.push((
            (*code).to_string(),
            match parsed {
                Some(number) => Value::Number(number),
                None => Value::Text(value.clone()),
            },
        ));
    }

    if let Some(style) = info.body_style {
        fields.push(("BodyStyle".to_string(), Value::Text(style.to_string())));
    }
    if let Some(generation) = &info.generation {
        fields.push((
            "Generation".to_string(),
            Value::Text(generation.name.clone()),
        ));
        if let Some(code) = &generation.code {
            fields.push(("GenerationCode".to_string(), Value::Text(code.clone())));
        }
        if let Some(ordinal) = generation.ordinal {
            fields.push((
                "GenerationOrdinal".to_string(),
                Value::Number(f64::from(ordinal)),
            ));
        }
    }
    if info.make_id != 0 {
        fields.push(("MakeId".to_string(), Value::Number(info.make_id as f64)));
    }
    if info.model_id != 0 {
        fields.push(("ModelId".to_string(), Value::Number(info.model_id as f64)));
    }
    fields.push((
        "ModelYearConclusive".to_string(),
        Value::Bool(info.year_conclusive),
    ));
    if warnings && !info.warnings.is_empty() {
        fields.push((
            "Warnings".to_string(),
            Value::List(info.warnings.iter().map(|w| w.to_string()).collect()),
        ));
    }

    // VIN first, then alphabetical, so columns are stable between runs.
    fields[1..].sort_by(|(a, _), (b, _)| a.cmp(b));
    fields
}

fn print_available_fields() {
    println!("Derived keys:");
    for key in SYNTHETIC_KEYS {
        println!("  {key}");
    }
    println!("\nvPIC elements:");
    for element in element::all() {
        println!(
            "  {:<36} {:<8} {}",
            element.code, element.data_type, element.name
        );
    }
}

/// Render a float without a trailing `.0`, so counts stay counts.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        let text = format!("{value:.4}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Quote and escape a JSON string.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Keep a value on one TSV cell.
fn tsv_cell(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(argv: &[&str]) -> Args {
        Args::parse(argv.iter().map(|arg| arg.to_string()))
            .expect("parses")
            .expect("not an early exit")
    }

    #[test]
    fn bare_vins_are_positional_arguments() {
        let parsed = args(&["1C6RR7LT2JS179571", "5XYRLDLC3NG097496"]);
        assert_eq!(parsed.vins.len(), 2);
        assert_eq!(parsed.format, Format::Text);
        assert!(!parsed.read_stdin);
    }

    #[test]
    fn input_files_parse_and_repeat() {
        let parsed = args(&["-i", "a.txt", "--input", "b.txt", "1C6RR7LT2JS179571"]);
        assert_eq!(parsed.inputs, vec!["a.txt", "b.txt"]);
        assert_eq!(parsed.vins, vec!["1C6RR7LT2JS179571"]);
        // A path is never mistaken for a VIN.
        assert!(!parsed.vins.iter().any(|vin| vin.ends_with(".txt")));
    }

    #[test]
    fn read_vins_skips_blanks_and_comments() {
        let listing = "\
1C6RR7LT2JS179571
# a comment line

5XYRLDLC3NG097496   # trailing note
   1HGCP26739A060971
";
        let mut vins = Vec::new();
        read_vins(listing.as_bytes(), &mut vins).expect("reads");
        assert_eq!(
            vins,
            vec![
                "1C6RR7LT2JS179571",
                "5XYRLDLC3NG097496",
                "1HGCP26739A060971"
            ]
        );
    }

    #[test]
    fn collect_vins_reads_a_file_after_the_arguments() {
        let dir = std::env::temp_dir().join("corgi-cli-collect-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("vins.txt");
        std::fs::write(&path, "5XYRLDLC3NG097496\n1HGCP26739A060971\n").expect("write");

        let parsed = args(&[
            "1C6RR7LT2JS179571",
            "--input",
            path.to_str().expect("utf-8 path"),
        ]);
        let vins = parsed.collect_vins().expect("collects");

        // Positional arguments first, then the file, in file order.
        assert_eq!(
            vins,
            vec![
                "1C6RR7LT2JS179571",
                "5XYRLDLC3NG097496",
                "1HGCP26739A060971"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_input_file_names_itself() {
        let parsed = args(&["--input", "/definitely/not/here.txt"]);
        let err = parsed.collect_vins().expect_err("should fail");
        assert!(err.to_string().contains("/definitely/not/here.txt"));
    }

    #[test]
    fn formats_and_fields_parse() {
        let parsed = args(&["--format", "jsonl", "--fields", "Make, Model ,EngineHP"]);
        assert_eq!(parsed.format, Format::JsonLines);
        assert_eq!(
            parsed.fields.as_deref(),
            Some(
                [
                    "Make".to_string(),
                    "Model".to_string(),
                    "EngineHP".to_string()
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn unknown_options_and_formats_are_rejected() {
        assert!(Args::parse(["--nope".to_string()].into_iter()).is_err());
        assert!(Args::parse(["--format".to_string(), "yaml".to_string()].into_iter()).is_err());
        assert!(Args::parse(["--fields".to_string(), " , ".to_string()].into_iter()).is_err());
        assert!(Args::parse(["--year".to_string()].into_iter()).is_err());
    }

    #[test]
    fn help_and_version_exit_without_arguments_to_process() {
        assert!(
            Args::parse(["--help".to_string()].into_iter())
                .unwrap()
                .is_none()
        );
        assert!(
            Args::parse(["--version".to_string()].into_iter())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn json_strings_escape_what_would_break_the_document() {
        assert_eq!(json_string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(json_string("a\\b"), r#""a\\b""#);
        assert_eq!(json_string("a\nb"), r#""a\nb""#);
        assert_eq!(
            json_string("Class 1D: 5,001 - 6,000 lb"),
            "\"Class 1D: 5,001 - 6,000 lb\""
        );
    }

    #[test]
    fn numbers_render_without_spurious_decimals() {
        assert_eq!(format_number(8.0), "8");
        assert_eq!(format_number(5.7), "5.7");
        assert_eq!(format_number(152.559), "152.559");
    }

    #[test]
    fn tsv_cells_never_contain_a_separator() {
        assert_eq!(tsv_cell("a\tb\nc"), "a b c");
    }

    #[test]
    fn a_record_is_vin_first_then_alphabetical() {
        let mut info = VehicleInfo::default();
        info.make = "Ram".to_string();
        info.attributes.insert("Model", "1500".to_string());
        info.attributes.insert("ABS", "Standard".to_string());

        let keys: Vec<String> = record("1C6RR7LT2JS179571", &info, true)
            .into_iter()
            .map(|(key, _)| key)
            .collect();

        assert_eq!(keys[0], "VIN");
        assert!(keys.windows(2).skip(1).all(|pair| pair[0] <= pair[1]));
        assert!(keys.contains(&"ModelYearConclusive".to_string()));
    }

    #[test]
    fn numeric_elements_become_json_numbers_and_text_stays_quoted() {
        let mut info = VehicleInfo::default();
        info.attributes.insert("Doors", "4".to_string());
        info.attributes.insert("Make", "Ram".to_string());

        let fields: BTreeMap<String, Value> = record("X", &info, false).into_iter().collect();
        assert_eq!(fields["Doors"].as_json(), "4");
        assert_eq!(fields["Make"].as_json(), "\"Ram\"");
    }

    #[test]
    fn selecting_fields_is_case_insensitive_and_keeps_the_requested_order() {
        let parsed = args(&["--fields", "enginehp,make"]);
        let mut info = VehicleInfo::default();
        info.attributes.insert("Make", "Ram".to_string());
        info.attributes.insert("EngineHP", "395".to_string());

        let selected: Vec<String> = parsed
            .select("X", &info)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(selected, vec!["EngineHP", "Make"]);
    }

    #[test]
    fn an_explicitly_requested_field_stays_in_the_row_even_when_it_is_absent() {
        let parsed = args(&["--fields", "Make,EngineHP"]);
        let mut info = VehicleInfo::default();
        info.attributes.insert("Make", "Ram".to_string());

        let selected = parsed.select("X", &info);
        assert_eq!(selected.len(), 2, "every row keeps the same shape");
        assert_eq!(selected[1].0, "EngineHP");
        assert_eq!(selected[1].1.as_json(), "null");
        assert_eq!(selected[1].1.as_text(), "");
    }
}
