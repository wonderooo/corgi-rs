//! Decode one VIN and print everything that came out of it.
//!
//! ```sh
//! cargo run --example decode_single_vin -- 1C6RR7LT2JS179571
//! ```

use corgi_rs::{VehicleInfo, VinDecoder};

fn main() {
    let vin = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1C6RR7LT2JS179571".to_string());

    let decoder = match VinDecoder::try_new() {
        Ok(decoder) => decoder,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };

    match decoder.decode(&vin) {
        Ok(info) => print_vehicle_info(&vin, &info),
        Err(err) => {
            eprintln!("failed to decode {vin}: {err}");
            std::process::exit(1);
        }
    }
}

fn print_vehicle_info(vin: &str, info: &VehicleInfo) {
    println!("VIN {vin}");
    println!(
        "  {} {} {}",
        info.year,
        info.make,
        info.model.as_deref().unwrap_or("<unknown model>")
    );
    println!(
        "  {} / {}",
        info.body_style
            .map(|style| style.to_string())
            .unwrap_or_else(|| "<unknown body>".to_string()),
        info.vehicle_type.as_deref().unwrap_or("<unknown type>")
    );

    if let Some(generation) = &info.generation {
        println!("  {generation}");
    }

    for warning in &info.warnings {
        println!("  warning: {warning}");
    }

    println!("\n  {} attributes decoded:", info.attribute_count());
    for (code, value) in &info.attributes {
        println!("    {code:<34} {value}");
    }
}
