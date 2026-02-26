# corgi-rs

`corgi-rs` is a VIN decoder built around archived lookup data that maps the VIN pattern segments to vehicle details such as make, model, trim, and body style. The crate bundles helpers for parsing/validating VINs, matching schema/pattern data, and constructing a `VehicleInfo` record with the decoded metadata.

## Highlights

- **Accurate VIN decoding** using CFR Title 49 validation rules, VIN structure checks, and model-year extraction.
- **Pattern matching pipeline** powered by `fst` + `rkyv` maps that load pre-generated schema/lookup tables at runtime.
- **Optional Rayon support** (`parallel` feature) for batch decoding over large VIN lists.
- **Field-rich output** exposed through `VehicleInfo`, including body style, fuel types, drive type, transmission, and more.

## Usage

Add the crate to your project:

```sh
cargo add corgi-rs
```

Set `MAPS_DIR` to point at a directory holding the generated `.fst`/`.bin` files (typically produced via the build pipeline that ships with the data assets). If you do not override `MAPS_DIR`, the crate defaults to `$HOME/.corgi-rs-cache`.

```sh
export MAPS_DIR=/path/to/corgi/maps
```

Decode a VIN:

```rust
use corgi_rs::{VinDecoder, VIN};

fn main() {
    let decoder = VinDecoder::new();
    let vin = "2FTEF14H8TCA73155".to_string();
    let info = decoder.decode(&vin).expect("VIN should decode");
    println!("{} {}", info.make, info.model.unwrap_or_default());
}
```

Create batch jobs with `decode_batch` or `decode_batch_owned` to run either sequential or (with `parallel` feature) Rayon-powered decoding.

## Examples

See the [`examples/`](examples) directory for runnable samples such as VIN decoding loops and filtering the decoded `VehicleInfo` results.

## Documentation

```sh
cargo doc --open
```

## Testing

```sh
cargo test
```

## Feature flags

- `parallel` – Enables Rayon to parallelize batch decoding (`decode_batch`, `decode_batch_owned`).

