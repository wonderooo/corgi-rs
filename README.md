# corgi-rs

A VIN decoder built on NHTSA's [vPIC](https://vpic.nhtsa.dot.gov/Downloads)
database. Give it a VIN, get back the make, model, year, body, powertrain and
around a hundred other attributes the manufacturer registered with NHTSA.

```rust
use corgi_rs::VinDecoder;

let decoder = VinDecoder::new();
// A 2018 Ram 1500 from a Copart listing.
let info = decoder.decode("1C6RR7LT2JS179571").expect("valid VIN");

assert_eq!(info.make, "Ram");
assert_eq!(info.model.as_deref(), Some("1500"));
assert_eq!(info.year, 2018);
assert_eq!(info.drive_type.as_deref(), Some("4WD/4-Wheel Drive/4x4"));
assert_eq!(info.displacement_l, Some(5.7));
```

## What you get

`VehicleInfo` gives a named field to every element that decodes on more than
about a fifth of real VINs — identity, body, engine, drivetrain, weight, plant
of assembly and the mainstream safety equipment, some 55 fields in all. Anything
rarer stays in `info.attributes`, keyed by NHTSA element code, which is also the
complete record:

```rust
# use corgi_rs::VinDecoder;
// A 2022 Kia Sorento.
let info = VinDecoder::new().decode("5XYRLDLC3NG097496").unwrap();

assert_eq!(info.engine_hp, Some(191.3));
assert_eq!(info.transmission.as_deref(), Some("Automatic"));
assert_eq!(info.seats, Some(7));
assert_eq!(info.seat_rows, Some(3));
assert_eq!(info.abs.as_deref(), Some("Standard"));

// The long tail has no field of its own, but it is still in the map.
assert_eq!(info.get("OtherEngineInfo"), Some("Gasoline Direct Injection"));
assert_eq!(info.get("LowerBeamHeadlampLightSource"), Some("LED"));
assert_eq!(info.get_bool("ABS"), Some(true));

for (code, value) in &info.attributes {
    println!("{code}: {value}");
}
```

A typical modern car resolves to about 40 attributes; some reach 75.
`corgi_rs::element::all()` lists the full catalogue with names and data types.

## Generations

vPIC has no generation element — NHTSA registers what a vehicle *is*, not how
its maker markets the redesign cycle, and the VIN schemas are filed per model
year rather than per generation. The VIN is not a reliable source either: Honda
puts its chassis code in positions 4-6, so a Civic's generation reads straight
off the VIN, but Ford and Toyota use those positions for cab, series and weight
rating and reuse the same codes for twenty years.

So the generation comes from a table maintained by hand in
[`tools/generations.tsv`](tools/generations.tsv): 605 models, 1,185
generations, resolving on **97.1%** of auction lots. Every boundary in it is
checked against the VIN body codes of real cars
(`corgi-validate --check-generations`).

Nine makes are kept complete, each resolving on better than 99.6% of lots:

| make | coverage | | make | coverage |
|---|---|---|---|---|
| Jeep | 100.00% | | Toyota | 99.93% |
| Chrysler | 99.99% | | Audi | 99.82% |
| Dodge | 99.99% | | BMW | 99.77% |
| Lexus | 99.97% | | Mercedes-Benz | 99.67% |
| Volvo | 99.97% | | | |

BMW needs an entry per engine-variant name, because that is how vPIC names the
model — `330i`, `M340i`, `750Li, Alpina B7` — while the generation is the
chassis code of the series it belongs to.

```rust
# use corgi_rs::VinDecoder;
let info = VinDecoder::new().decode("1HGCP26739A060971").unwrap();
let generation = info.generation.as_ref().unwrap();

assert_eq!(generation.name, "8th generation");
assert_eq!(generation.ordinal, Some(8));
assert_eq!(generation.code.as_deref(), Some("CP/CS"));
assert_eq!(generation.year_from, 2008);
assert_eq!(generation.year_to, Some(2012));
```

A model the table does not cover yields `None` rather than a guess.

## Command line

The crate ships a `corgi` binary.

```sh
cargo install corgi-rs
corgi 1C6RR7LT2JS179571
```

```text
1C6RR7LT2JS179571
  2018 Ram 1500
  Pickup / Truck
  DS (2011-2018)

  35 attributes:
    VIN                                  1C6RR7LT2JS179571
    AirBagLocFront                       1st Row (Driver and Passenger)
    BedType                              Short
    BodyCabType                          Crew/Super Crew/Crew Max
    ...
```

VINs come from the arguments or, with none or with `-`, from standard input one
per line. `--format` picks the shape — `text`, `line`, `json`, `jsonl` or `tsv`
— and `--fields` picks the columns:

```sh
# skim a list
cut -d, -f2 lots.csv | corgi --format line

# a stable TSV, one column per key, blank where a VIN did not resolve it
corgi --format tsv --fields VIN,Make,Model,ModelYear,DisplacementL,DriveType < vins.txt

# stream JSON objects into jq
corgi --format jsonl --fields VIN,Make,EngineHP < vins.txt | jq 'select(.EngineHP > 400)'
```

Keys are NHTSA element codes plus a few the decoder derives — `BodyStyle`,
`Generation`, `GenerationCode`, `GenerationOrdinal`, `MakeId`, `ModelId`,
`ModelYearConclusive`, `Warnings`. `corgi --list-fields` prints them all. Numeric elements come out as JSON numbers. A key you asked for
but that a VIN did not resolve is `null` rather than missing, so every record
has the same shape. Exit status is 1 if any VIN failed to decode.

## Accuracy

Measured against 627,103 Copart and IAAI listings, comparing each decode with
what the auction house published for that lot. Only rows where both sides had a
value count toward accuracy; see [`tools/validate`](tools/validate) for the
harness and its caveats.

| field | coverage | accuracy | relaxed | accuracy before |
|---|---|---|---|---|
| make | 99.98% | 99.74% | 99.74% | 87.84% |
| model | 99.92% | 83.06% | 99.02% | 72.53% (86.10% relaxed) |
| year | 100.00% | 99.91% | 99.94% | 99.87% |
| body style | 99.93% | 88.90% | 99.10% | 87.99% (97.95% relaxed) |
| fuel | 98.37% | 94.00% | 99.97% | 84.73% (91.82% relaxed) |
| drive type | 76.77% | 85.87% | 99.31% | not decoded at all |
| transmission | 39.63% | 98.21% | — | not decoded at all |
| cylinders | 86.61% | 99.70% | — | not decoded at all |
| displacement | 97.35% | 99.47% | — | not decoded at all |

"Accuracy before" is the same measurement against the decoder this replaced.
Drive type, transmission, cylinders and displacement were absent from its data
entirely, so it returned nothing for them on all 627,103 lots.

"Relaxed" folds together distinctions the two vocabularies do not share: a
listing calling a Mercedes CLA a coupé where vPIC calls it a sedan, or calling
an E85-capable F-150 flex-fuel where vPIC records plain petrol. Most of the
model gap between strict and relaxed is the listing carrying the trim along
with the model — `G70 BASE` against vPIC's `G70` — rather than a wrong decode.

0.03% of VINs do not decode, almost all of them motorcycles, trailers and
equipment whose WMIs are deliberately not shipped.

## Scope

The bundled tables cover passenger cars, trucks (which is where every pickup
lives), multipurpose passenger vehicles, incomplete vehicles, and the 12- and
15-seat passenger vans NHTSA files as buses. Motorcycles, trailers, low-speed
and off-road vehicles are excluded — they are about 85% of the WMI table and
none of them are cars. Decoding one returns `WmiErrorCode::UnknownWmi`.

## How it decodes

The pipeline follows NHTSA's own `spVinDecode` stored procedure:

1. Resolve the WMI, and with it the vehicle type and manufacturer.
2. Read the model year from position 10 — this selects *which* VIN schemas apply.
3. Match every pattern of every applicable schema against VIN positions 4-8 and
   10-17.
4. Rank the matches per element and keep one winner each.
5. Derive the make from the winning model, since `Make_Model` is one-to-one and
   a WMI often is not.
6. Fill gaps from the engine-model, vehicle-spec and default-value tables.

Two of those steps are easy to get wrong, and both were:

- **Without the model-year window**, every schema a WMI ever used competes at
  once. `1HG` has over eighty schemas; only a handful apply to any given car.
- **Without deriving the make from the model**, shared WMIs collapse. `1C4` is
  registered to Jeep, Ram, Dodge, Chrysler *and* Fiat, and no VIN pattern names
  the make. Picking the first make listed turns every Jeep Renegade into a
  Dodge.

The decoder also improves on NHTSA in one place. Position 10 repeats every 30
years, and NHTSA disambiguates it from position 7 only for cars, MPVs and light
trucks; elsewhere it just rejects years more than two in the future. That reads
a 1997 Ford E-150 as a 2027 one, and — because Volvo and Mercedes put digits in
position 7 on cars built well after 2010 — a 2020 XC90 as a 1990 one. corgi-rs
resolves the ambiguity against the schema year windows instead: a WMI only has
schemas for years it actually built vehicles in.

## Installation

```sh
cargo add corgi-rs
```

`build.rs` expands `assets/*.tsv.zst` into memory-mapped `fst` tables under
`$HOME/.corgi-rs-cache` (about 55 MB) the first time the crate is built. Set
`CORGI_CACHE_DIR` to build them elsewhere, and `MAPS_DIR` to read them from
elsewhere at runtime.

`VinDecoder::new()` panics if the tables are missing; `VinDecoder::try_new()`
returns the reason instead.

## Validation and check digits

A wrong check digit does not stop a decode — salvage and grey-import VINs carry
them routinely, and NHTSA decodes those too. It is reported as
`Warning::CheckDigitMismatch`. `VinDecoder::require_check_digit(true)` makes it
an error instead.

```rust
# use corgi_rs::{VinDecoder, Warning};
let info = VinDecoder::new().decode("1C6RR7LT0JS179571").unwrap();
assert!(info.warnings.contains(&Warning::CheckDigitMismatch));
```

## Batch decoding

`decode_batch` and `decode_batch_owned` decode a slice of VINs, parallelised
with Rayon under the default `parallel` feature. Build one `VinDecoder` and
share it: construction memory-maps the tables, decoding allocates only the
result.

## Regenerating the data

`assets/` is a compressed export of the vPIC database, currently NHTSA's
**2026-08** release. See [`tools/README.md`](tools/README.md) for how to refresh
it and how to re-measure accuracy afterwards.

## Feature flags

- `parallel` (default) — Rayon-parallel batch decoding.

## Testing

```sh
cargo test
```

`tests/decode_real_vins.rs` decodes VINs from real auction listings and checks
them against what the auction house published.
