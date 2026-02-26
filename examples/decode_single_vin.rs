use corgi_rs::{VIN, VinDecoder, decoder::extractors::VehicleInfo};
use std::env;

fn main() {
    if env::var("MAPS_DIR").is_err() {
        eprintln!(
            "Set MAPS_DIR to point at the cached .fst/.bin files before running this example."
        );
        return;
    }

    let decoder = VinDecoder::new();
    let vin: VIN = "2FTEF14H8TCA73155".to_string();

    match decoder.decode(&vin) {
        Ok(info) => print_vehicle_info(&vin, &info),
        Err(err) => eprintln!("failed to decode {vin}: {err:?}"),
    }
}

fn print_vehicle_info(vin: &VIN, info: &VehicleInfo) {
    println!(
        "VIN {vin} -> Make: {make}, Model: {model:?}, Year: {year}",
        make = info.make,
        model = info.model.as_deref().unwrap_or("<unknown>"),
        year = info.year
    );
}
