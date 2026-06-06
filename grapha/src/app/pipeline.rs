use std::path::PathBuf;

use anyhow::Context;

pub(crate) use grapha_engine::pipeline::run_pipeline;

pub(crate) fn handle_analyze(
    path: PathBuf,
    output: Option<PathBuf>,
    filter: Option<String>,
    compact: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let mut graph = run_pipeline(&path, verbose, false, None)?.graph;

    if let Some(ref filter_str) = filter {
        let kinds = crate::filter::parse_filter(filter_str)?;
        graph = crate::filter::filter_graph(graph, &kinds);
    }

    let json = if compact {
        let pruned = crate::compress::prune::prune(graph, false);
        let grouped = crate::compress::group::group(&pruned);
        match &output {
            Some(_) => serde_json::to_string(&grouped)?,
            None => serde_json::to_string_pretty(&grouped)?,
        }
    } else {
        match &output {
            Some(_) => serde_json::to_string(&graph)?,
            None => serde_json::to_string_pretty(&graph)?,
        }
    };

    match output {
        Some(path) => {
            std::fs::write(&path, &json)
                .with_context(|| format!("failed to write {}", path.display()))?;
            if verbose {
                eprintln!("  \x1b[32m✓\x1b[0m wrote {}", path.display());
            }
        }
        None => println!("{json}"),
    }

    Ok(())
}
