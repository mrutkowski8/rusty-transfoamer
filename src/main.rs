use std::path::PathBuf;
use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand, ValueEnum};
use rusty_transfoamer::steps::step0::coverage::{Mode, SelectionOptions};
use rusty_transfoamer::steps::step0::expirations::DteWindow;
use rusty_transfoamer::steps::step0::report::Verdict;
use rusty_transfoamer::steps::step0::{self, InstrumentDay, is_root};
use rusty_transfoamer::storage;

#[derive(Parser)]
#[command(
    name = "rusty-transfoamer",
    version,
    about = "audits and transforms raw option data exports"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// step 0: audit raw instrument-day folders and pick the days for step 1, without changing anything
    Step0(Step0Args),
}

#[derive(Clone, Copy, ValueEnum)]
enum SelectionMode {
    /// dates where every required root passed, gaps allowed
    Common,
    /// every passing instrument-day on its own
    All,
    /// only the longest unbroken run of common dates
    Contiguous,
}

#[derive(Args)]
struct Step0Args {
    /// folder or .zip file that holds the <ROOT>/<YYYY-MM-DD>/ day folders
    #[arg(long)]
    input: PathBuf,

    /// instrument-day to audit, e.g. SPXW/2023-01-10. repeat for more days. without it every day folder is audited
    #[arg(long = "day", value_name = "ROOT/YYYY-MM-DD")]
    days: Vec<InstrumentDay>,

    /// folder for the outputs, written to <OUTPUT>/step0/
    #[arg(long, default_value = "output")]
    output: PathBuf,

    /// day whose files define the expected columns and types for each root and file kind
    #[arg(long, default_value = "2023-01-10", value_parser = parse_date)]
    reference_day: String,

    /// where the reference day is read from, if not from --input
    #[arg(long)]
    reference_input: Option<PathBuf>,

    /// roots that all have to pass for a date to be picked
    #[arg(long, default_value = "SPXW,VIX,VIXW", value_parser = parse_roots)]
    required: Roots,

    /// roots added on picked dates where they pass. use "" for none
    #[arg(long, default_value = "SPX", value_parser = parse_roots)]
    optional: Roots,

    /// tiers that take part in the selection: good, conditional, or good,conditional
    #[arg(long, default_value = "good", value_parser = parse_tiers)]
    tiers: Tiers,

    /// how dates are picked
    #[arg(long, value_enum, default_value = "common")]
    selection: SelectionMode,

    /// first date audited. day folders before it are not read
    #[arg(long, value_name = "YYYY-MM-DD", value_parser = parse_day)]
    from: Option<NaiveDate>,

    /// last date audited. day folders after it are not read
    #[arg(long, value_name = "YYYY-MM-DD", value_parser = parse_day)]
    to: Option<NaiveDate>,

    /// only expirations this many calendar days out: 0 for 0DTE, or a range such as 0-45. files of other expirations are not read
    #[arg(long, value_name = "DAYS")]
    dte: Option<DteWindow>,

    /// days audited in parallel
    #[arg(long, default_value_t = 6)]
    workers: usize,
}

#[derive(Clone)]
struct Roots(Vec<String>);

fn parse_roots(text: &str) -> Result<Roots, String> {
    let roots: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_owned)
        .collect();
    match roots.iter().find(|r| !is_root(r)) {
        Some(bad) => Err(format!(
            "root must be upper-case letters such as SPXW, got '{bad}'"
        )),
        None => Ok(Roots(roots)),
    }
}

#[derive(Clone)]
struct Tiers(Vec<Verdict>);

fn parse_tiers(text: &str) -> Result<Tiers, String> {
    let mut tiers: Vec<&str> = text.split(',').map(str::trim).collect();
    tiers.sort();
    match tiers.as_slice() {
        ["good"] => Ok(Tiers(vec![Verdict::Good])),
        ["conditional"] => Ok(Tiers(vec![Verdict::Conditional])),
        ["conditional", "good"] => Ok(Tiers(vec![Verdict::Good, Verdict::Conditional])),
        _ => Err(format!(
            "tiers must be good, conditional or good,conditional, got '{text}'"
        )),
    }
}

fn parse_date(text: &str) -> Result<String, String> {
    if step0::is_iso_date(text) {
        Ok(text.to_owned())
    } else {
        Err(format!("date must look like YYYY-MM-DD, got '{text}'"))
    }
}

fn parse_day(text: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .filter(|_| step0::is_iso_date(text))
        .ok_or_else(|| format!("date must be a real YYYY-MM-DD date, got '{text}'"))
}

const EXIT_EMPTY_SELECTION: u8 = 3;

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Step0(args) => run_step0(args),
    }
}

fn run_step0(args: Step0Args) -> ExitCode {
    if args.required.0.is_empty() {
        eprintln!("error: --required needs at least one root");
        return ExitCode::from(2);
    }
    if let (Some(from), Some(to)) = (args.from, args.to)
        && from > to
    {
        eprintln!("error: --from {from} is after --to {to}");
        return ExitCode::from(2);
    }
    let request = step0::Request {
        input: args.input,
        days: args.days,
        reference_day: args.reference_day,
        reference_input: args.reference_input,
        selection: SelectionOptions {
            required: args.required.0,
            optional: args.optional.0,
            tiers: args.tiers.0,
            mode: match args.selection {
                SelectionMode::Common => Mode::Common,
                SelectionMode::All => Mode::All,
                SelectionMode::Contiguous => Mode::Contiguous,
            },
        },
        filters: step0::Filters {
            from: args.from,
            to: args.to,
            dte: args.dte,
        },
        workers: args.workers,
    };
    let report = match step0::run(&request) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let dir = args.output.join("step0");
    let mut log = Vec::new();
    step0::report::write_log(&report, &dir, &mut log).expect("writing to memory cannot fail");
    let log = String::from_utf8_lossy(&log).into_owned();
    let selection = serde_json::json!({
        "options": report.options,
        "filters": report.filters,
        "dates": report.selection.dates,
        "good": report.selection.good,
        "conditional": report.selection.conditional,
    });
    let writes = [
        storage::write_json_atomic(&dir.join("report.json"), &report),
        storage::write_json_atomic(&dir.join("selection.json"), &selection),
        storage::write_text_atomic(&dir.join("audit.log"), &log),
    ];
    if let Some(Err(e)) = writes.into_iter().find(Result::is_err) {
        eprintln!("error: cannot write the outputs to {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    print!("{log}");
    if report.selection.dates.is_empty() {
        return ExitCode::from(EXIT_EMPTY_SELECTION);
    }
    ExitCode::SUCCESS
}
