# **PURPOSE**

#### step 0 checks the raw export and picks the days that step 1 cleans

* it reads every instrument-day (one root on one date) without changing, cleaning or copying anything
* it gives each one a tier, `good` or `conditional`, or rejects it, always with a code and the evidence
* it writes the lists of picked instrument-days that step 1 reads

the full rule set, with real example files, is in [specification.md](specification.md).

# **RUN**

#### run step 0 on a whole pack

```bash
cargo run --release -- step0 --input day_range_pack
```

the outputs go to `output/step0/`: `report.json`, `selection.json` and `audit.log`. a short log is printed too. every option is listed in [docs/cli.md](../../../docs/cli.md).

#### or on part of a large export

```bash
cargo run --release -- step0 --input <EXPORT> --from 2023-01-01 --to 2023-12-31 --dte 0-45
```

files outside the dates or the expiration window aren't read at all.

#### or on the synthetic pack, with the reference day taken from the real one

```bash
cargo run --release -- step0 --input day_range_pack_synthetic --reference-input day_range_pack --output output/synthetic --selection all
```

# **HOW IT WORKS**

#### the code follows the six sieves

* `mod.rs` – finds the day folders (sieve 1), reads the reference day, audits the days in parallel and runs the selection
* `calendar.rs` – the exchange calendar: holidays, unscheduled closures and half days (sieve 2)
* `structure.rs` – where each file belongs, its column layout, and the columns expected from the reference day (sieve 3)
* `rows.rs` – one file's rows: dates, root, expiration, duplicates, values, session hours, bar grid, gaps and quote coverage (sieves 3 and 4)
* `qualification.rs` – one instrument-day: reads its files one expiration at a time, applies the repairs, checks completeness and sets the tier (sieves 1 to 5)
* `expirations.rs` – which expirations a day should list: SPXW weekdays, SPX and VIX monthlies, VIXW weeklies
* `coverage.rs` – picks the instrument-days for step 1 (sieve 6)
* `report.rs` – the report and the log, built from the same data

the readers it uses are shared: `input/source.rs` reads a folder or a ZIP file, `input/table.rs` reads parquet rows and turns every time into new york time, and `storage/` writes the outputs.

# **VERIFICATION**

#### how step 0 was checked

* `cargo test` runs 51 tests, from single rules (calendar, expiration dates, gaps, duplicates, clock repair) to the selection modes and the ZIP reader
* the real pack (107 instrument-days, about 30 seconds) agrees with the key on accept or reject for 100 of them. the 7 others follow decisions made on purpose, listed in the specification
* all 24 synthetic cases get their expected verdict
* a ZIP of a sample of the real pack gives exactly the same report as the folder

*the test packs live next to the code and will be published later, so anyone can rerun these checks.*
