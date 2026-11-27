# **PURPOSE**

#### this is the full rule set of step 0, with real files from the test packs as examples

step 0 reads a raw option export, judges every instrument-day (one root on one date) and picks the instrument-days that step 1 cleans. it never changes, cleans or copies data. every rule below is implemented in `src/steps/step0/` and checked against the local test packs.

* an instrument-day lives in `<ROOT>/<YYYY-MM-DD>/` inside a folder or a ZIP file
* each one goes through six sieves in order. sieves 1 and 2 stop at the first failure; the later ones record every problem, and the first one is the headline
* the result is a verdict, a list of reasons, and the lists that step 1 reads

# **VERDICTS**

#### every instrument-day ends in one of three tiers

* **good** – complete, nothing missing or repaired. it can still carry notes, e.g. that nobody traded the calls of one expiration
* **conditional** – passes every sieve, but something is missing or was repaired, and later steps can work around it
* **rejected** – fails a sieve. the report names the sieve, the code and the evidence

a folder that isn't an instrument-day at all is **skipped**, e.g. `VIXW/2023-04-27 (1)`.

a tier only goes down. a day that is conditional after step 0 stays conditional to the end, and a later step can move a good day to conditional, never back.

# **FILTERS**

#### limits on what is read at all, for very large exports

* `--from` and `--to` – only day folders in this date range are audited, both ends included. the others aren't read; the log gives their count. example: `--from 2023-01-01 --to 2023-12-31` audits 33 of the 107 real instrument-days in about 11 seconds
* `--dte` – only expirations this many calendar days out, e.g. `0` for 0DTE or `0-45`. their files are read, the files of every other expiration are skipped unread – `outside_dte_window`. inside the day-level quote file and the OI file, rows of other expirations are ignored
* with `--dte`, every completeness rule below looks only at the expirations inside the window:
  * the call-put pair in every bar: if only one series expiring today is in the window, its closing bar is exempt
  * the nearest series is checked only when it lies inside the window
  * the SPXW chain: only dailies inside the window, and a reach of at least the smaller of 35 days and the window's end
* a day with data but no expiration inside the window is rejected – `no_expiration_in_window`. examples: `VIX` with `--dte 0` (VIX has no 0DTE), `SPXW/2022-01-04` with `--dte 0` (a tuesday in january 2022, before tuesday expirations)
* a window can make a day stricter. with `--dte 0` the 0DTE is the only series, so `SPXW/2022-02-22` (0DTE empty until 11:35) and `SPXW/2022-11-25` (0DTE empty 13:00-13:14) are rejected for missing pairs, while without the filter they are conditional
* without filters nothing changes: every report equals the unfiltered run

# **SIEVE 1 - FOLDER**

#### is this an instrument-day with data at all

* a folder name that isn't a date is skipped – `not_a_date_folder`. example: `VIXW/2023-04-27 (1)`, a windows-style copy
* a folder without a single `.parquet` file is rejected – `no_data_files`. examples: `SPXW/2026-07-17` holds only an empty `quote_stream/`, `VIX/2026-07-15` holds only a JSONL stream
* a requested day without a folder is rejected – `folder_not_found`. example: `--day SPX/2025-01-09`

# **SIEVE 2 - CALENDAR**

#### was there a regular session that day

* weekends, exchange holidays and unscheduled closures are rejected whatever the files contain – `market_closed`. examples: `VIX/2023-01-16` (Martin Luther King Jr. Day) has complete-looking files, `SPXW/2025-01-09` (national day of mourning), `VIX/2023-04-29` (a saturday copy of friday)
* half days close at 13:15. the day stays, but it's conditional and every bar after the close is outside the session – `early_close`. examples: 2022-11-25 and 2024-12-24
* the calendar is a table in the code covering 2021-2027, checked against the NYSE calendar. a date outside it is rejected – `calendar_not_covered`

# **SIEVE 3 - FILES**

#### is every file readable and shaped like the reference day

only `.parquet` files count, and only in the known places of a day folder:

* `quote_<bar>/<expiration>.parquet`
* `greeks_first_order_<bar>/<expiration>.parquet`
* `implied_vol_<bar>/<expiration>_<call|put>.parquet`
* `ohlc_<bar>/<expiration>_<call|put>.parquet`
* `open_interest.parquet` (SPX and SPXW)
* `quote_1m.parquet`, one day-level quote file for all expirations (older SPXW days)

the checks:

* not a `.parquet` file → skipped with a note – `invalid_file_type`. examples: `desktop.ini`, `quote_5m/2023-05-17.parquet.part`, `quote_1m.parquet.legacy_pre_step343`
* a `.parquet` file outside the known places → skipped as an error – `unsupported_layout`. example: the live snapshots in `VIX/2026-09-03/snapshots/` and `latest/`
* an OI file for VIX or VIXW, which never have OI → skipped with a note – `unexpected_open_interest`. example: `VIX/2026-07-30/open_interest.parquet`
* the footer can't be read → broken – `parquet_unreadable`. examples: synthetic `VIX/2023-04-18/quote_5m/2023-04-19.parquet` (cut in half), `VIXW/2023-04-18/greeks_first_order_5m/2023-04-26.parquet` (0 bytes)
* 0 rows → broken – `parquet_empty`. example: synthetic `VIX/2023-04-19/quote_5m/2023-05-17.parquet`
* columns don't match the reference day → broken – `parquet_wrong_schema`. examples: synthetic `ask_implied_vol` missing, `implied_vol` renamed to `iv`, `strike` stored as text
* the rows belong to another root, date or expiration → broken – `content_mismatch`. examples: synthetic `VIX/2023-04-25` holds VIXW rows, `VIX/2023-04-26` holds 2023-04-25 rows, `quote_5m/2023-05-03.parquet` holds the 2023-05-10 expiration

column order and the `c_` prefix never matter. the reference day (default 2023-01-10, `--reference-day`) gives the expected columns per root and file kind.

# **SIEVE 4 - ROWS**

#### are the rows themselves usable

* the same key (expiration, strike, side, bar) twice with different values → broken – `conflicting_duplicates`. example: synthetic `SPX/2022-12-05/open_interest.parquet`
* the same row twice, identical → step 1 removes the copy, the day is conditional – `exact_duplicates`. example: synthetic `VIX/2023-04-21`, 237 copies
* impossible values (negative price or size, missing strike, infinity) → those rows are excluded, the day is conditional – `invalid_values`. example: synthetic `VIXW/2023-04-24`, 5 negative bids and a missing strike
* bars outside 09:30-16:00 → broken – `outside_session`, unless the clock repair below applies
* bars off the 1-minute (SPX, SPXW) or 5-minute (VIX, VIXW) grid → broken – `wrong_bar_interval`, unless it's 1-minute VIX/VIXW, which is converted

not errors, the rows are kept as they are: the 09:30 placeholder (bid = ask = 0), zero or empty prices on bars without trades, very wide asks, one-sided quotes, and single crossed quotes on stress days such as 2025-04-07.

# **REPAIRS**

#### fixes that are allowed because they can be checked

* **timestamp text in another format** – read with a short list of formats, if every row parses the same way, every date is the folder date and the times form the session grid. the day is conditional – `timestamp_reformatted`. example: synthetic `SPXW/2023-04-18`, `04/18/2023 09:30:00`
* **a clock in utc labelled as new york** – shifted back by the new york utc offset (4 hours in summer, 5 in winter), if every file is late by the same amount, the shifted bars form the exact session grid, and the underlying matches the sister root (VIX with VIXW, SPX with SPXW). the day is conditional – `clock_shift_repaired`. example: synthetic `VIX/2023-04-24`, all 79 underlying prices match VIXW after the shift
* **1-minute VIX/VIXW** – converted to 5 minutes in step 1: quote, greeks and IV keep the rows at the 5-minute marks, trades are summed over five bars. nothing is missing, so the day stays good with a note – `interval_converted`. example: `VIX/2026-07-30`
* **the day-level quote file** – used for an expiration whose own quote file is missing or broken. the day is conditional – `day_level_quotes`. where both files exist the day-level file is only a cross-check: they must agree (prices within 1e-9), or the day is rejected – `quote_sources_conflict`. examples: `SPXW/2022-01-04` has both and they agree, so it stays good; `SPXW/2022-08-15` takes 2022-09-23 from it, `SPXW/2022-09-07` takes every expiration from it

# **SIEVE 5 - COMPLETENESS**

#### does the day hold everything later steps need

an instrument-day is like a book: each expiration is a chapter of six files, and each session minute is a page.

* **quotes are required.** an expiration without a usable quote file is unavailable, the day is conditional – `expiration_unavailable`. if no expiration is left, the day is rejected – `no_usable_expirations`. example: synthetic `SPXW/2023-04-19`, one broken quote file three weeks out
* **provider greeks and IV are optional.** missing or broken → conditional – `missing_vendor_values`. example: `SPX/2023-01-30` has no call IV for 2023-02-17
* **trades are optional.** no file for one side means nobody traded it, a note – `ohlc_side_missing`. example: `VIXW/2024-06-06`. a broken trades file → conditional – `trades_unavailable`
* **provider values that disagree** between the greeks and IV files, or quote and greeks files listing different strikes → conditional – `vendor_values_inconsistent`. examples: `SPXW/2025-01-16`, `SPXW/2025-03-14`
* **OI for SPX and SPXW is required.** missing, stale or conflicting → rejected – `open_interest_unusable`. examples: `SPXW/2026-07-09` has none, synthetic `SPX/2022-12-06` is stamped the day before
* **no gaps.** every contract has every session bar in the quote, greeks and IV files. any gap that a half day doesn't explain rejects the day – `grid_gap`. example: synthetic `VIXW/2023-04-21`, 11:00-11:55 missing
* **a call-put pair in every minute.** a pair is one strike of one expiration with a valid quote (bid > 0, ask > bid) on both the call and the put. step 2 builds the forward from pairs, so a bar without any pair can't be used. every session bar needs at least one pair. the 09:30 bar is exempt when all its quotes are placeholders. otherwise rejected – `quote_coverage_gap`. examples:
  * `SPX/2022-02-22` and `VIX/2022-02-22`: no valid quote at all from 09:31 to 11:35
  * `VIXW/2022-02-22`: about one lone quote per 5-minute bar out of 352 rows until 11:35, e.g. only `2022-03-23 30 CALL 3.05 / 3.32` at 10:30, and never a pair
* **pairs in the nearest expiration.** if the nearest usable expiration has bars without a pair, the day is conditional – `nearest_without_pairs`. the 09:30 bar doesn't count, and neither does the closing bar of a 0DTE, which expires then. examples: `SPXW/2022-02-22`, whose 0DTE has no pair from 09:31 to 11:35 while 13 other expirations do; `SPXW/2022-11-25`, whose 0DTE has none from 13:00 to 13:14
* **the nearest series.** every day should list the series that expires first. missing, or present with a broken quote file → conditional – `nearest_expiration_unavailable`. example: synthetic `VIX/2023-04-18`, the quote file of the next day's monthly is cut in half
  * SPXW: the 0DTE, on every weekday that had one (mon, wed, fri; tuesday from 2022-04-18; thursday from 2022-05-11)
  * SPX: the nearest monthly (third friday, the day before a holiday). on its expiration day it's already gone, so `SPX/2023-01-20` correctly lists only february
  * VIX: the nearest monthly (30 days before the third friday of the next month). `VIX/2024-06-17` correctly expects tuesday 06-18
  * VIXW: the nearest wednesday outside the monthly VIX week (the day before a holiday)
* **the SPXW chain.** every daily expiration of the next two weeks after the 0DTE has usable quotes, and the farthest one is at least 35 days out. otherwise conditional – `expirations_missing`, with the missing dates in the message. a missing expiration never rejects a day on its own: whoever uses the data decides which expirations it needs. example: `SPXW/2026-07-30`, whose farthest expiration is 18 days out

# **SIEVE 6 - SELECTION**

#### which instrument-days go to step 1

* `--required` (default `SPXW,VIX,VIXW`) – roots that all have to pass for a date
* `--optional` (default `SPX`) – added on picked dates where it passes, never removes or adds a date
* `--tiers` (default `good`) – `good,conditional` also lets conditional days in, `conditional` alone picks only them
* `--selection` (default `common`)
  * `common` – every date where all required roots passed, gaps allowed
  * `all` – every passing instrument-day on its own
  * `contiguous` – only the longest unbroken run of common dates, the newest on a tie
* no date picked → a clear message, the report is still written, exit code 3

# **OUTPUT**

#### step 0 writes three files into `<OUTPUT>/step0/`

* `report.json` – every instrument-day with its verdict, rejection or conditions, notes, expirations, quote coverage, findings and files
* `selection.json` – the picked dates and the `good` and `conditional` lists that step 1 reads, with notes, day-level sources and unavailable expirations
* `audit.log` – the console log: one line per instrument-day with the files it skipped, its errors and warnings, a summary and the selection

# **RESULTS ON THE TEST PACKS**

#### the real pack, 107 instrument-days

* 61 good, 16 conditional, 30 rejected
* the default selection picks 11 dates: 2022-01-04, 2023-01-04, 2023-01-09 to 2023-01-13, 2023-01-20, 2024-10-14, 2024-10-15 and 2025-04-07
* `--selection contiguous` picks 2023-01-09 to 2023-01-13, the range the key calls the best
* step 0 agrees with the key on accept or reject for 100 of 107 instrument-days. the other 7 follow decisions made on purpose:
  * SPX, VIX and VIXW 2022-02-22 are rejected, because a morning without call-put pairs is a gap. SPXW of that day is conditional, because only its nearest expirations are empty
  * SPX 2023-01-30 and VIX 2026-10-12 are conditional, because missing provider IV can be computed later
  * VIX and VIXW 2026-07-30 are converted from 1 minute instead of rejected, and conditional because their provider IV disagrees between files

#### the synthetic pack, 24 cases

* every case gets its expected verdict: 4 good, 12 conditional, 7 rejected, 1 skipped
* a ZIP of a sample of the real pack gives exactly the same report as the folder

# **LIMITS**

#### what step 0 can't see

* **a wrong provider rate.** on 2024-10-14 the provider computed its greeks with a fallback rate. nothing in the files shows it, so the day is good. step 3 computes its own IV anyway
* **forward quality.** one call-put pair per bar proves that a forward can be built, not that it is good. wide quotes like those of `VIXW/2025-04-10`, three days after the april 2025 sell-off, are real market and pass. how good the forward is gets checked in step 2
* **the calendar ends in 2027.** later dates are rejected until the table grows

*if you want to see the sieves in action, run step 0 on the synthetic pack: every case there breaks exactly one rule.*
