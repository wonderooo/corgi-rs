# Data pipeline and accuracy measurement

Two jobs live here: turning an NHTSA vPIC release into the crate's assets, and
measuring how well the result decodes real cars.

## Refreshing the assets

`assets/*.tsv.zst` is an export of NHTSA's vPIC database, which NHTSA
republishes monthly at <https://vpic.nhtsa.dot.gov/Downloads>. The shipped
assets are from the **2026-08** release.

NHTSA publishes three formats. `*.plain.zip` is a `pg_dump` plain SQL file and
is what these instructions use; `*.custom.zip` is the same data in pg_dump's
custom format, and `*.bak.zip` is a SQL Server backup.

Use the **complete** database. Slimmed-down redistributions circulate — the one
this crate previously used carried 766,746 patterns against the 2026-08
release's 1,674,161, and it was missing `DriveType`, `TransmissionStyle`,
`EngineHP` and the whole `VehicleSpecSchema` family entirely. That is why the
crate used to return nothing at all for drive type and transmission.

Load a release into any Postgres and run the extraction from the directory you
want the files in:

```sh
curl -O https://vpic.nhtsa.dot.gov/downloads/vPICList_lite_YYYY_MM.plain.zip
unzip vPICList_lite_YYYY_MM.plain.zip

docker run -d --name vpic-pg -e POSTGRES_PASSWORD=vpic -e POSTGRES_USER=vpic \
    -e POSTGRES_DB=vpic -p 55432:5432 postgres:17
PGPASSWORD=vpic psql -h 127.0.0.1 -p 55432 -U vpic -d vpic -f vPICList_lite_YYYY_MM.sql

mkdir -p /tmp/vpic-assets && cd /tmp/vpic-assets
PGPASSWORD=vpic psql -h 127.0.0.1 -p 55432 -U vpic -d vpic \
    -f /path/to/tools/extract_assets.sql
```

Then compress the results into `assets/`:

```sh
for f in /tmp/vpic-assets/*.tsv; do
    zstd -19 -T0 -q -f "$f" -o "assets/$(basename "$f").zst"
done
```

`generation.tsv.zst` is not produced by the SQL; see below. `cargo build` picks
the change up through a stamp file and rebuilds the memory-mapped tables.

### Check the new release before trusting it

Always re-run the accuracy measurement after an upgrade. NHTSA reformats its
vocabularies without notice: the 2026-08 release rewrote
`Sport Utility Vehicle (SUV)/Multi-Purpose Vehicle (MPV)` as
`Sport Utility Vehicle [SUV]/Multipurpose Vehicle [MPV]`, which silently cost
35 points of body-style accuracy until `BodyStyle::classify` was made to ignore
punctuation. A release upgrade is a data migration, not a version bump.

### What the extraction produces

Each file is tab-separated and sorted by its first column in byte order, which
is what `fst` needs to index it.

| file | key | holds |
|---|---|---|
| `wmi.tsv` | WMI | vehicle type, truck type, manufacturer, country, and the make *only when the WMI has exactly one* |
| `wmi_schema.tsv` | WMI | schema ids with the model-year window each applies to |
| `pattern.tsv` | schema id | the VIN patterns, with lookup ids already resolved to display values |
| `model.tsv` | model id | model name and the make that owns it |
| `vspec.tsv` | make\|type\|model\|year | the vehicle-spec tables |
| `engine_model.tsv` | engine model | elements implied by an engine model name |
| `default_value.tsv` | vehicle type | per-type fallbacks |
| `element.tsv`, `vehicle_type.tsv` | — | metadata, compiled into the crate as static tables |

The extraction mirrors `spVinDecode`'s own filters: the same element exclusions,
the same `TobeQCed` and public-availability rules, and the same attribute
resolution through `felementattributevalue`. It keeps vehicle types 2, 3, 5, 7
and 10 and drops the rest, which is what holds the assets to about 5 MB.

## Generations

`tools/generations.tsv` is maintained by hand, because vPIC has no generation
element and the VIN does not reliably carry one. Columns:

```
make  model  year_from  year_to  ordinal  code  name
```

`year_to` of `0` means the generation is still current; `ordinal` of `0` means
it has no number people actually use. Audi, BMW, Chrysler, Dodge, Jeep, Lexus,
Mercedes-Benz, Toyota and Volvo are kept complete; the rest of the table
follows auction volume. Model years, inclusive, in the spelling
corgi-rs decodes — `Mazda3`, `F-150`, `CR-V`. Ranges for one model must not
overlap: where two generations were sold side by side in the changeover year,
the year goes to the volume seller.

After editing, rebuild the asset:

```sh
grep -v '^#' tools/generations.tsv \
  | awk -F'\t' 'NF==7 {printf "%s\t%s\t%s\t%s\t%s\t%s\n", tolower($1 "|" $2), $3,$4,$5,$6,$7}' \
  | LC_ALL=C sort -t$'\t' -k1,1 -s \
  | zstd -19 -T0 -q -o assets/generation.tsv.zst
```

Then check it against real cars:

```sh
cargo run --release -p corgi-validate -- --limit 700000 --check-generations
```

That reports which declared boundaries are visible in the VIN body codes and
which models the corpus has plenty of but the table does not cover. Read the
output with its blind spots in mind: Ford encodes cab, series and weight rating
in positions 4-8 and carries them across a redesign, so an F-150 boundary shows
as "not visible" even though it is right. A flag is something to look at, not a
proven error.

## Measuring accuracy

`tools/validate` scores the decoder against the Copart and IAAI listings in the
auction database. Each lot carries a VIN *and* the make, model, year, body,
fuel, drive, transmission and engine the auction house published, so it is
ground truth the decoder had no hand in.

```sh
export DATABASE_URL='postgresql://…/neondb?sslmode=require'
cargo run --release -p corgi-validate -- --limit 700000 --baseline
```

| flag | effect |
|---|---|
| `--limit N` / `--offset N` | how many rows, and where to start |
| `--source NAME` | one auction source only |
| `--cars-only` | drop lots that decode as non-passenger vehicles |
| `--baseline` | also score the `nhtsa_*` columns already on each row, and print the before/after |
| `--mismatches N` | the N worst disagreements per field |
| `--sample-failures N` | VINs that did not decode at all |
| `--dump PATH` | every disagreeing row, as CSV |
| `--allow-derived` | see below |
| `--check-generations` | check `tools/generations.tsv` against the VIN body codes |
| `--write` | write the decode back into the `nhtsa_*` columns |

`--write` mutates the live auction table. It is off by default and there is no
dry-run flag, so point `DATABASE_URL` at a copy first if you are unsure.

### Ground truth is not automatically trustworthy

Migration `0008_normalize_filters` rewrote four columns of `lot_vehicle` in
place, and two of them — `vehicle_type` and `fuel_type` — were filled from the
`nhtsa_*` columns, which is to say from a previous run of this decoder. Scoring
against those measures agreement with the old decoder, not accuracy, and it
flatters both: the old decoder called about 10,000 petrol Ford 3.5 EcoBoosts
"Diesel", and `fuel_type` now says `DIESEL` for every one of them.

The harness therefore reads body and powertrain ground truth from
`lot_vehicle_pre0008_cols`, which holds the values as the auctions published
them, and leaves those four fields unscored on the 21% of rows that snapshot
does not cover. `--allow-derived` scores them anyway; the numbers it produces
are not accuracy.

Two further caveats the report cannot fix:

- **Body style.** Copart files roughly half of all lots as `AUTOMOBILE`, which
  says nothing about the shape, and `MEDIUM DUTY/BOX TRUCKS` is a weight class
  rather than a body. Both are treated as no ground truth.
- **Transmission.** Copart lists 98% of lots as `AUTOMATIC`, which is higher
  than the real fleet, so some of the remaining disagreement is the listing
  defaulting rather than the decoder erring.

### Reading the output

- **coverage** — how often the decoder produced a value at all.
- **accuracy** — how often it agreed, *among rows where both sides had a value*.
  Rows the listing left blank are excluded, so a decoder cannot score well by
  staying silent.
- **relaxed** — accuracy once the distinctions the two vocabularies do not share
  are merged. Never lower than strict accuracy.
