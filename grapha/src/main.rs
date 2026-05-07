mod annotation_sync;
mod annotations;
mod app;
mod assets;
mod cache;
mod changes;
mod classify;
mod compress;
mod concepts;
mod config;
mod data_paths;
mod delta;
mod extract;
mod fields;
mod filter;
mod history;
mod index_status;
mod inferred;
mod localization;
mod maintenance;
mod mcp;
mod migration;
mod polyglot_plugin;
mod progress;
mod query;
mod recall;
mod render;
mod rust_plugin;
mod search;
mod serve;
mod snippet;
mod store;
mod symbol_locator;
mod watch;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "grapha",
    version,
    about = "Structural code graph for LLM consumption"
)]
struct Cli {
    /// ANSI color mode for tree output
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum QueryOutputFormat {
    Json,
    Tree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum BriefOutputFormat {
    Json,
    Tree,
    Brief,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ContextOutputFormat {
    Json,
    Tree,
    Brief,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RepoArchOutputFormat {
    Json,
    Brief,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RepoSmellsOutputFormat {
    Json,
    Brief,
}

impl RepoSmellsOutputFormat {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Brief => "brief",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RepoInferenceOutputFormat {
    Json,
    Brief,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RepoDoctorOutputFormat {
    Json,
    Brief,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum TraceDirection {
    Forward,
    Reverse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OriginTerminalFilter {
    Network,
    Persistence,
    Cache,
    Event,
    Keychain,
    Search,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum HistoryKind {
    Commit,
    Build,
    Test,
    Deploy,
    Incident,
}

impl From<HistoryKind> for history::HistoryEventKind {
    fn from(value: HistoryKind) -> Self {
        match value {
            HistoryKind::Commit => Self::Commit,
            HistoryKind::Build => Self::Build,
            HistoryKind::Test => Self::Test,
            HistoryKind::Deploy => Self::Deploy,
            HistoryKind::Incident => Self::Incident,
        }
    }
}

impl OriginTerminalFilter {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Persistence => "persistence",
            Self::Cache => "cache",
            Self::Event => "event",
            Self::Keychain => "keychain",
            Self::Search => "search",
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze source files and output graph
    Analyze {
        /// File or directory to analyze
        path: PathBuf,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Filter node kinds (comma-separated: fn,struct,enum,trait,impl,mod,field,variant)
        #[arg(long)]
        filter: Option<String>,
        /// Output in compact grouped format (optimized for LLM consumption)
        #[arg(long)]
        compact: bool,
    },
    /// Index a project into persistent storage
    Index {
        /// Project directory to index
        path: PathBuf,
        /// Storage format: "json" or "sqlite" (default: sqlite)
        #[arg(long, default_value = "sqlite")]
        format: String,
        /// Storage directory (default: .grapha/ in project root)
        #[arg(long)]
        store_dir: Option<PathBuf>,
        /// Force a full store/search rebuild instead of using incremental sync
        #[arg(long)]
        full_rebuild: bool,
        /// Show per-phase timing breakdown for performance profiling
        #[arg(long)]
        timing: bool,
    },
    /// Bootstrap this worktree from another local Grapha store
    Migrate {
        /// Project directory to receive the temporary migrated store
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Source project root or .grapha store directory (default: newest sibling worktree)
        #[arg(long)]
        from: Option<PathBuf>,
        /// Replace an existing non-temporary target Grapha index
        #[arg(long)]
        force: bool,
    },
    /// Launch web UI for interactive graph exploration
    Serve {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Run as MCP server over stdio (instead of HTTP)
        #[arg(long)]
        mcp: bool,
        /// Watch for file changes and auto-update the graph
        #[arg(long)]
        watch: bool,
    },
    /// Serve and sync the local-first annotation store
    Annotation {
        #[command(subcommand)]
        command: AnnotationCommands,
    },
    /// Query symbol relationships and search indexed symbols
    Symbol {
        #[command(subcommand)]
        command: SymbolCommands,
    },
    /// Inspect dataflow between symbols, entries, and effects
    Flow {
        #[command(subcommand)]
        command: FlowCommands,
    },
    /// Inspect localization references and usage sites
    #[command(name = "l10n")]
    L10n {
        #[command(subcommand)]
        command: L10nCommands,
    },
    /// Inspect image asset catalogs and usage sites
    Asset {
        #[command(subcommand)]
        command: AssetCommands,
    },
    /// Resolve business concepts to likely code scopes and manage concept bindings
    Concept {
        #[command(subcommand)]
        command: ConceptCommands,
    },
    /// Run repository-scoped analysis over the indexed graph
    Repo {
        #[command(subcommand)]
        command: RepoCommands,
    },
}

#[derive(Subcommand)]
enum AnnotationCommands {
    /// Launch the Grapha HTTP service with annotation API routes enabled
    Serve {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Watch for file changes and auto-update the graph
        #[arg(long)]
        watch: bool,
    },
    /// Bidirectionally sync annotations with a Grapha annotation service
    Sync {
        /// Annotation service base URL, e.g. http://192.168.1.10:8080.
        /// Defaults to GRAPHA_ANNOTATION_SERVER or grapha.toml [annotations].server.
        #[arg(long)]
        server: Option<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// List locally stored annotations for this project identity
    List {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum SymbolCommands {
    /// Search symbols by name or file
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Filter by symbol kind (function, struct, enum, trait, etc.)
        #[arg(long)]
        kind: Option<String>,
        /// Filter by module name
        #[arg(long)]
        module: Option<String>,
        /// Filter by repo name
        #[arg(long)]
        repo: Option<String>,
        /// Filter by file path glob
        #[arg(long)]
        file: Option<String>,
        /// Filter by role (entry_point, terminal, internal)
        #[arg(long)]
        role: Option<String>,
        /// Enable fuzzy matching (tolerates typos)
        #[arg(long)]
        fuzzy: bool,
        /// Require an exact declaration-name match (e.g. "foo" matches "foo(x:)")
        #[arg(long)]
        exact_name: bool,
        /// Exclude synthetic nodes and accessor functions from results
        #[arg(long)]
        declarations_only: bool,
        /// Keep only public symbols
        #[arg(long)]
        public_only: bool,
        /// Include source snippet and relationships in results
        #[arg(long)]
        context: bool,
        /// Fields to display (comma-separated: file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
    /// Query symbol context (callers, callees, implementors)
    Context {
        /// Symbol name or ID
        symbol: String,
        /// Project directory (reads from .grapha/)
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = ContextOutputFormat::Json)]
        format: ContextOutputFormat,
        /// Fields to display (comma-separated: file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        /// Limit items per result section (callers, callees, etc.). Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Analyze blast radius of changing a symbol
    Impact {
        /// Symbol name or ID
        symbol: String,
        /// Maximum traversal depth
        #[arg(long, default_value = "3")]
        depth: usize,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = BriefOutputFormat::Json)]
        format: BriefOutputFormat,
        /// Fields to display (comma-separated: file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        /// Limit items per depth bucket (depth_1, depth_2, depth_3_plus). Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Analyze structural complexity of a type (properties, dependencies, invalidation surface)
    Complexity {
        /// Type name or ID to analyze
        symbol: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// List all declarations in a file, ordered by source position
    File {
        /// File name or path suffix (e.g. "RoomPage.swift" or "src/main.rs")
        file: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Attach an agent-written annotation to a symbol
    Annotate {
        /// Symbol name, locator, ID, or Swift USR
        symbol: String,
        /// Annotation text to store for this symbol
        annotation: String,
        /// Agent or author label
        #[arg(long)]
        by: Option<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show the stored annotation for a symbol
    Annotation {
        /// Symbol name, locator, ID, or Swift USR
        symbol: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum FlowCommands {
    /// Trace dataflow forward to terminals or backward to entry points
    Trace {
        /// Symbol name or ID
        symbol: String,
        /// Trace direction
        #[arg(long, value_enum, default_value_t = TraceDirection::Forward)]
        direction: TraceDirection,
        /// Maximum traversal depth
        #[arg(long)]
        depth: Option<usize>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = BriefOutputFormat::Json)]
        format: BriefOutputFormat,
        /// Fields to display in tree output (comma-separated: file; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        /// Limit flows (forward) or affected entries (reverse). Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Derive a semantic effect graph from a symbol
    Graph {
        /// Symbol name or ID
        symbol: String,
        /// Maximum traversal depth
        #[arg(long, default_value = "10")]
        depth: usize,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        /// Limit nodes and edges in the dataflow result. Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Trace backward to likely API/data origins for a UI symbol
    Origin {
        /// Symbol name or ID
        symbol: String,
        /// Maximum traversal depth
        #[arg(long, default_value = "10")]
        depth: usize,
        /// Keep only origins whose terminal kind matches
        #[arg(long, value_enum)]
        terminal_kind: Option<OriginTerminalFilter>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in output (comma-separated: file,snippet; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        /// Limit reported origins. Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// List auto-detected entry points
    Entries {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Filter entry points by module name
        #[arg(long)]
        module: Option<String>,
        /// Filter entry points by file path or suffix
        #[arg(long)]
        file: Option<String>,
        /// Limit the number of shown entries. Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Subcommand)]
enum L10nCommands {
    /// Resolve localization records reachable from a SwiftUI symbol subtree
    Symbol {
        /// Symbol name or ID
        symbol: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
    /// Find SwiftUI usage sites for a localization key or translated value
    Usages {
        /// Localization key or translated string value
        key: String,
        /// Optional table/catalog name
        #[arg(long)]
        table: Option<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Subcommand)]
enum AssetCommands {
    /// List image assets from indexed catalogs
    List {
        /// Only show assets with no references in source code
        #[arg(long)]
        unused: bool,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Find source code usage sites for an image asset
    Usages {
        /// Asset name (e.g., "icon_gift" or "Room/voiceWave")
        name: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConceptCommands {
    /// Search for likely scopes related to a business concept
    Search {
        /// Business concept text
        term: String,
        /// Max results
        #[arg(long, default_value_t = concepts::DEFAULT_CONCEPT_SEARCH_LIMIT)]
        limit: usize,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
    /// Show a stored concept mapping and its bound symbols
    Show {
        /// Business concept text
        term: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = QueryOutputFormat::Json)]
        format: QueryOutputFormat,
        /// Fields to display in tree output (comma-separated: file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
    },
    /// Bind a business concept to one or more symbols
    Bind {
        /// Business concept text
        term: String,
        /// One or more symbols to bind
        #[arg(long = "symbol", required = true)]
        symbols: Vec<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Add aliases for an existing or new concept
    Alias {
        /// Business concept text
        term: String,
        /// One or more aliases to add
        #[arg(long = "add", required = true)]
        aliases: Vec<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Remove a concept from the project concept store
    Remove {
        /// Business concept text
        term: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Remove bindings whose symbols no longer exist in the graph
    Prune {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum RepoCommands {
    /// Show index freshness and repository snapshot metadata
    Status {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Detect code changes and analyze their impact
    Changes {
        /// Scope: "unstaged", "staged", "all", or a git ref (e.g., "main")
        #[arg(default_value = "all")]
        scope: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Limit affected symbols and per-symbol impact buckets. Pass a large value to disable.
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show file/symbol map for orientation in large projects
    Map {
        /// Filter by module name
        #[arg(long)]
        module: Option<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Check configured architecture dependency rules
    Arch {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = RepoArchOutputFormat::Json)]
        format: RepoArchOutputFormat,
    },
    /// Detect code smells across the graph (god types, deep nesting, wide invalidation, etc.)
    Smells {
        /// Filter to a specific module
        #[arg(long)]
        module: Option<String>,
        /// Limit smell analysis to symbols declared in a matching file
        #[arg(long)]
        file: Option<String>,
        /// Limit smell analysis to a specific symbol and its local neighborhood
        #[arg(long)]
        symbol: Option<String>,
        /// Bypass both cached graph loads and cached smell results
        #[arg(long)]
        no_cache: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = RepoSmellsOutputFormat::Json)]
        format: RepoSmellsOutputFormat,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Show per-module metrics (symbol counts, coupling, entry points)
    Modules {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Build opt-in inferred metadata for modules, ownership, and doc-code links
    Infer {
        /// Output format
        #[arg(long, value_enum, default_value_t = RepoInferenceOutputFormat::Json)]
        format: RepoInferenceOutputFormat,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Check graph integrity, inferred links, and relation provenance
    Doctor {
        /// Output format
        #[arg(long, value_enum, default_value_t = RepoDoctorOutputFormat::Json)]
        format: RepoDoctorOutputFormat,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Record or list commit/build/test/deploy/incident history
    History {
        #[command(subcommand)]
        command: HistoryCommands,
    },
}

#[derive(Subcommand)]
enum HistoryCommands {
    /// Add a typed history event linked to files, modules, or symbols
    Add {
        /// Event kind
        #[arg(long, value_enum)]
        kind: HistoryKind,
        /// Event title
        #[arg(long)]
        title: String,
        /// Event timestamp (defaults to current Unix milliseconds)
        #[arg(long)]
        at: Option<String>,
        /// Optional status label, such as passed, failed, deployed, or mitigated
        #[arg(long)]
        status: Option<String>,
        /// Related commit SHA
        #[arg(long)]
        commit: Option<String>,
        /// Related branch name
        #[arg(long)]
        branch: Option<String>,
        /// Free-form event detail
        #[arg(long)]
        detail: Option<String>,
        /// Link a source file path or suffix
        #[arg(long = "file")]
        files: Vec<String>,
        /// Link a module name
        #[arg(long = "module")]
        modules: Vec<String>,
        /// Link a symbol query, resolved to the current graph symbol ID
        #[arg(long = "symbol")]
        symbols: Vec<String>,
        /// Metadata key/value pair, formatted as key=value
        #[arg(long = "meta")]
        metadata: Vec<String>,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// List typed history events
    List {
        /// Filter by event kind
        #[arg(long, value_enum)]
        kind: Option<HistoryKind>,
        /// Filter by linked source file substring
        #[arg(long)]
        file: Option<String>,
        /// Filter by linked module name
        #[arg(long)]
        module: Option<String>,
        /// Filter by linked symbol query, resolved to the current graph symbol ID
        #[arg(long)]
        symbol: Option<String>,
        /// Maximum number of events to return (0 means unlimited)
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let render_options = app::query::tree_render_options(cli.color);

    match cli.command {
        Commands::Analyze {
            path,
            output,
            filter,
            compact,
        } => app::pipeline::handle_analyze(path, output, filter, compact)?,
        Commands::Index {
            path,
            format,
            store_dir,
            full_rebuild,
            timing,
        } => app::index::handle_index(path, format, store_dir, full_rebuild, timing)?,
        Commands::Migrate { path, from, force } => app::migrate::handle_migrate(path, from, force)?,
        Commands::Serve {
            path,
            port,
            mcp,
            watch,
        } => app::serve::handle_serve(path, port, mcp, watch)?,
        Commands::Annotation { command } => app::annotation::handle_annotation_command(command)?,
        Commands::Symbol { command } => app::query::handle_symbol_command(command, render_options)?,
        Commands::Flow { command } => app::query::handle_flow_command(command, render_options)?,
        Commands::L10n { command } => app::query::handle_l10n_command(command, render_options)?,
        Commands::Asset { command } => app::query::handle_asset_command(command, render_options)?,
        Commands::Concept { command } => {
            app::query::handle_concept_command(command, render_options)?
        }
        Commands::Repo { command } => app::query::handle_repo_command(command)?,
    }

    Ok(())
}
