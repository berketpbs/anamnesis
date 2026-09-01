//! `anamnesis eval`: running the retrieval suites, and reading them out.
//!
//! Almost all of this is printing, and the printing is the point. A number
//! nobody can compare to the last one is not a measurement, so every table
//! here names what moved, against what, and by how much — and the ablation
//! says which stream earned its place rather than only that the total went up.

use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use jiff::Timestamp;

/// Score retrieval against a checked-in corpus.
///
/// Prints what each suite scored, and — with `--check` — refuses to exit zero
/// when one has fallen below the bar it sets for itself. The bar lives in the
/// suite file rather than here, so a change that costs recall shows up as a
/// number someone had to edit.
/// Run the retrieval suites and print what they measured.
pub fn cmd_eval(
    suite: Option<&std::path::Path>,
    verbose: bool,
    check: bool,
    streams: bool,
    sweep: bool,
    embed: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    // Held still on purpose. Freshness is an input to nothing a suite scores,
    // and it can only be that way if two runs are handed the same instant.
    let now: Timestamp = "2026-01-01T00:00:00Z".parse()?;

    let suites: Vec<(String, anamnesis_evals::Suite)> = match suite {
        Some(path) => {
            let loaded = anamnesis_evals::Suite::load(path)?;
            vec![(path.display().to_string(), loaded)]
        }
        None => anamnesis_evals::builtin_suites()
            .into_iter()
            .map(|(name, source)| {
                anamnesis_evals::Suite::from_toml(source).map(|suite| (name.to_owned(), suite))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };

    // Built once, whatever is being scored, and only when asked: loading it
    // means a model on disk and a few seconds, which nothing about the three
    // SQL streams should have to wait for.
    let embedder = if embed {
        let data = DataDir::resolve(data_dir)?;
        let built = anamnesis_llm::EmbedConfig::enabled().build(&data.models())?;
        match &built {
            Some(embedder) => println!(
                "Embedding with {}.
",
                embedder.model()
            ),
            None => anyhow::bail!("--embed was asked for but no embedder could be built"),
        }
        built
    } else {
        None
    };
    let embed = embedder
        .as_deref()
        .map(|embedder| embedder as &dyn anamnesis_core::embedding::Embed);

    if sweep {
        let grid = anamnesis_evals::default_grid();
        println!(
            "Sweeping {} settings over {} suite(s). Nothing here changes what ships.",
            grid.len(),
            suites.len()
        );
        println!();
        print_sweep(
            &anamnesis_evals::sweep(&suites, now, &grid, embed)?,
            verbose,
        );
        return Ok(());
    }

    let mut failed = 0usize;
    for (source, suite) in &suites {
        let report = match embed {
            Some(embedder) => anamnesis_evals::run_embedded(suite, now, embedder)?,
            None => anamnesis_evals::run(suite, now)?,
        };
        print_report(&report, source, verbose);
        if streams {
            print_ablation(&anamnesis_evals::ablate_with(suite, now, embed)?);
        }
        if !report.passed() {
            failed += 1;
        }
    }

    if failed > 0 && check {
        anyhow::bail!(
            "{failed} of {} suites scored below their thresholds",
            suites.len()
        );
    }
    Ok(())
}

/// One suite's results.
fn print_report(report: &anamnesis_evals::Report, source: &str, verbose: bool) {
    println!("🎯 {} — {}", report.name, report.description);
    println!(
        "   {} · {} pages · {} cases · scored over the first {}",
        source,
        report.pages,
        report.cases.len(),
        report.limit
    );
    println!();
    println!(
        "   MRR     {:.3}  {}",
        report.mrr,
        describe_bar(report.mrr, report.thresholds.min_mrr)
    );
    println!(
        "   Recall  {:.3}  {}",
        report.recall,
        describe_bar(report.recall, report.thresholds.min_recall)
    );

    // Printed whether or not anyone asked, and in two lists rather than one.
    // A suite that passes on average while a question goes unanswered is the
    // result most likely to be read as "fine" — and so is one whose answers
    // are all technically there, at the bottom of a page nobody scrolls.
    print_cases("Nothing relevant came back for:", report.misses());
    print_cases("Answered, but not near the top:", report.ranked_low());

    if verbose {
        println!();
        println!("   rank  query");
        for case in &report.cases {
            let rank = match case.score.rank {
                Some(rank) => format!("{rank:>4}"),
                None => "   —".to_owned(),
            };
            println!("   {rank}  {}", case.query);
        }
    }

    println!();
}

/// Every setting tried, best mean rank first.
///
/// Two things the table has to make impossible to miss. The row that ships
/// today is marked wherever it lands, because a list of alternatives with no
/// baseline says nothing about what changing costs. And the rows that clear
/// the acceptance rule — rank up and recall held on *every* suite — are marked
/// separately from the rows that merely sit at the top, because sorting by a
/// mean is exactly how a gain on one corpus pays for a loss on another.
fn print_sweep(report: &anamnesis_evals::SweepReport, verbose: bool) {
    /// Rows shown when the caller did not ask for all of them.
    const SHOWN: usize = 12;

    let mut header = String::from("        k  entity  links   vect   auth  cover  depth");
    for suite in &report.suites {
        header.push_str(&format!("  {:>12}", truncate(suite, 12)));
    }
    header.push_str("     mean");
    println!("{header}");

    let baseline = report.baseline();
    let improvements = report.improvements();

    let mut shown: Vec<&anamnesis_evals::SweepPoint> = if verbose {
        report.points.iter().collect()
    } else {
        report.points.iter().take(SHOWN).collect()
    };
    // Always visible, however far down the table it sits.
    if !shown.iter().any(|point| point.is_default())
        && let Some(base) = baseline
    {
        shown.push(base);
    }

    for point in shown {
        let mut row = format!(
            "  {:>6.0}  {:>6.2}  {:>5.2}  {:>5.2}  {:>5.2}  {:>5.2}  {:>5}",
            point.tuning.rrf_k,
            point.tuning.entity,
            point.tuning.links,
            point.tuning.vectors,
            point.tuning.authority_exponent,
            point.tuning.entity_coverage,
            point.tuning.candidates
        );
        for score in &point.scores {
            row.push_str(&format!("  {:>5.3} {:>5.3}", score.mrr, score.recall));
        }
        row.push_str(&format!("  {:>7.3}", point.mean_mrr()));

        if point.is_default() {
            row.push_str("  ← ships today");
        } else if baseline.is_some_and(|base| point.improves_on(base)) {
            row.push_str("  ✓");
        }
        println!("{row}");
    }

    println!();
    match baseline {
        None => println!(
            "  The grid does not contain today's defaults, so none of this says what changing would cost."
        ),
        Some(_) => println!(
            "  {} of {} settings improve on it: nothing lower anywhere, something higher (✓).",
            improvements.len(),
            report.points.len()
        ),
    }
    println!("  A row winning here is not on its own a reason to adopt it: prefer the middle of a");
    println!(
        "  region that wins over a single spike, which this many questions cannot tell apart."
    );
    println!();
}

/// Cut a name down to fit a column, ending in an ellipsis when it is cut.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// What each stream contributes on its own.
fn print_ablation(ablation: &anamnesis_evals::Ablation) {
    println!("   stream     MRR    recall   only this stream finds");
    for stream in &ablation.streams {
        // The last column is the one that decides whether a stream stays: a
        // respectable average with nothing unique behind it means the other
        // streams already cover it.
        let unique = match stream.only_stream_to_find.len() {
            0 => "—".to_owned(),
            count => format!("{count}"),
        };
        println!(
            "   {:<9}  {:.3}  {:.3}    {}",
            stream.name, stream.mrr, stream.recall, unique
        );
    }

    for stream in &ablation.streams {
        for query in &stream.only_stream_to_find {
            println!("     only {} finds {query:?}", stream.name);
        }
    }

    if !ablation.found_by_none.is_empty() {
        println!();
        println!("   No single stream answered these — fusion is doing the work:");
        for query in &ablation.found_by_none {
            println!("     {query:?}");
        }
    }
    println!();
}

/// One list of cases, with the reason each is in the suite.
fn print_cases<'a>(heading: &str, cases: impl Iterator<Item = &'a anamnesis_evals::CaseOutcome>) {
    let cases: Vec<&anamnesis_evals::CaseOutcome> = cases.collect();
    if cases.is_empty() {
        return;
    }

    println!();
    println!("   {heading}");
    for case in cases {
        match case.score.rank {
            Some(rank) => println!("     [{rank}] {:?}", case.query),
            None => println!("     [—] {:?}", case.query),
        }
        if !case.note.is_empty() {
            println!("         {}", case.note);
        }
    }
}

/// How a measurement sits against the bar the suite set for itself.
///
/// A suite that set no bar is reported without one rather than as a pass: it
/// was never being gated, and a tick would say it was.
fn describe_bar(value: f64, bar: f64) -> String {
    if bar <= 0.0 {
        return "(no threshold)".to_owned();
    }
    if value >= bar {
        format!("(bar {bar:.3}) ok")
    } else {
        format!("(bar {bar:.3}) BELOW")
    }
}
