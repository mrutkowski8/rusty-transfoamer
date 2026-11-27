# **PURPOSE**

#### this page lists every command and option of rusty-transfoamer

* **available** options work today and are tested
* **planned** options are decided and arrive with the step named next to them
* **open** options aren't decided yet

the page is updated whenever an option changes.

# **USAGE**

#### the general form is

```bash
rusty-transfoamer <COMMAND> [OPTIONS]
```

while developing, run it through cargo and put the options after `--`. `--release` makes the full pack run in about 30 seconds.

```bash
cargo run --release -- step0 --input day_range_pack
```

* `-h`, `--help` shows the help of the program or of one command, e.g. `rusty-transfoamer step0 --help` (available)
* `-V`, `--version` shows the version (available)

# **COMMANDS**

#### there is one command per numbered step

* `step0` audits a raw export without changing it and picks the instrument-days for step 1 (available)
* `step1` to `step7` arrive with their steps (planned)
* a command that runs all steps in a row (open)

# **STEP0**

#### step0 reads the raw export, never changes it, and writes a report and the picked days

```bash
rusty-transfoamer step0 --input <FOLDER|ZIP> [--day <ROOT/YYYY-MM-DD> ...] [OPTIONS]
```

all of these are available:

* `--input <FOLDER|ZIP>` (required) – a folder or a `.zip` file holding the `<ROOT>/<YYYY-MM-DD>/` day folders. a ZIP made from the pack folder itself works too
* `--day <ROOT/YYYY-MM-DD>` (repeatable) – audit only these instrument-days, e.g. `SPXW/2023-01-10`. without it every day folder of the input is audited
* `--output <FOLDER>` (default `output`) – the outputs go to `<OUTPUT>/step0/`
* `--reference-day <YYYY-MM-DD>` (default `2023-01-10`) – the day whose files define the expected columns and types for each root and file kind
* `--reference-input <FOLDER|ZIP>` (default: `--input`) – where the reference day is read from, e.g. the real pack when auditing the synthetic one
* `--required <ROOTS>` (default `SPXW,VIX,VIXW`) – roots that all have to pass for a date to be picked
* `--optional <ROOTS>` (default `SPX`) – roots added on picked dates where they pass. they never remove or add a date. `""` for none
* `--tiers <TIERS>` (default `good`) – which tiers take part: `good`, `conditional` or `good,conditional`. `conditional` alone picks only conditional days, e.g. to test separately how usable they are
* `--selection <MODE>` (default `common`)
  * `common` – dates where every required root passed, gaps allowed
  * `all` – every passing instrument-day on its own
  * `contiguous` – only the longest unbroken run of common dates, the newest on a tie
* `--from <YYYY-MM-DD>`, `--to <YYYY-MM-DD>` – audit only day folders in this range, both ends included. the others aren't read, the log gives their count. `--from` after `--to` is a usage error
* `--dte <DAYS>` – only expirations this many calendar days out: `0` for 0DTE, or a range such as `0-45`. files of other expirations aren't read (`outside_dte_window`), and every completeness rule looks only at the window. a day with no expiration in the window is rejected (`no_expiration_in_window`), so VIX and VIXW never pass `--dte 0`
* `--workers <N>` (default `6`) – days audited in parallel

# **OUTPUT**

#### step0 writes three files into `<OUTPUT>/step0/` and prints the log

* `report.json` – every instrument-day with its verdict, the sieve and code that rejected it or the conditions that make it conditional, notes, expirations, quote coverage, findings and files
* `selection.json` – the options and filters used, the picked dates and the `good` and `conditional` lists for step 1
* `audit.log` – the same log as on the console: one line per instrument-day with its verdict and, in brackets, the files it skipped and why, e.g. `good (4 files skipped: invalid_file_type)`, then its errors and warnings
* fatal and usage errors go to stderr

# **EXIT STATUS**

#### scripts and the GUI can read the result from the exit code

* `0` – the audit ran and picked at least one date. rejected days don't change the code
* `1` – the input or the reference day is missing, or the outputs can't be written
* `2` – wrong usage, e.g. a malformed `--day`
* `3` – the audit ran, but no instrument-day was picked. the report is still written

# **EXAMPLES**

#### audit the whole real pack

```bash
cargo run --release -- step0 --input day_range_pack
```

#### audit a few days only

```bash
cargo run --release -- step0 --input day_range_pack --day SPXW/2023-01-10 --day SPX/2022-02-22
```

#### audit the synthetic pack into its own folder

```bash
cargo run --release -- step0 --input day_range_pack_synthetic --reference-input day_range_pack --output output/synthetic --selection all
```

#### pick only the best unbroken range, conditional days included

```bash
cargo run --release -- step0 --input day_range_pack --selection contiguous --tiers good,conditional
```

#### audit only 2023

```bash
cargo run --release -- step0 --input day_range_pack --from 2023-01-01 --to 2023-12-31
```

#### only the 0DTE of SPXW, every passing day on its own

```bash
cargo run --release -- step0 --input day_range_pack --dte 0 --required SPXW --optional "" --selection all
```

#### list every conditional instrument-day and nothing else

```bash
cargo run --release -- step0 --input day_range_pack --tiers conditional --selection all
```

*option names and defaults can still change while later steps are built. this page always shows the current state.*
