mod annotation_sync;
mod annotations;
mod app;
mod assets;
mod cache;
mod changes;
mod classify;
mod cluster;
mod compress;
mod concepts;
mod config;
mod data_paths;
mod delta;
mod extract;
mod fields;
mod filter;
mod history;
mod http_client;
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
mod remote;
mod render;
mod rust_plugin;
mod search;
mod serve;
mod snippet;
mod store;
mod symbol_locator;
mod watch;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "grapha",
    version,
    about = "Fast code intelligence CLI and MCP server for Swift, Rust, and tree-sitter languages",
    long_about = "Grapha indexes source into a symbol graph for search, context, impact analysis, dataflow tracing, repository health checks, and MCP-based agent workflows.\n\nSwift uses Xcode index stores when available, then falls back through SwiftSyntax and tree-sitter. Rust and other supported languages use tree-sitter extraction with name-based relationships.",
    after_help = "Typical workflow:\n  grapha index .\n  grapha repo status\n  grapha symbol search ViewModel --context\n  grapha symbol impact GiftPanelViewModel --format tree\n  grapha mcp --watch\n\nUse `grapha <command> --help` for task-specific examples."
)]
struct Cli {
    /// Show progress, timing, and other diagnostic logs
    #[arg(long, global = true)]
    verbose: bool,
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

#[derive(Clone, Debug, Default, Args)]
struct ClusterArgs {
    /// Return score-band clusters instead of the default list JSON
    #[arg(long)]
    cluster: bool,
    /// Score-band cluster to page: excellent, strong, possible, weak, or unknown
    #[arg(long)]
    cluster_id: Option<String>,
    /// 1-based page number within the selected cluster
    #[arg(long, default_value = "1")]
    cluster_page: usize,
    /// Items returned in the selected cluster page
    #[arg(long, default_value = "20")]
    cluster_per_page: usize,
    /// Candidates fetched before score-band clustering
    #[arg(long, default_value = "200")]
    cluster_candidate_limit: usize,
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
    #[command(
        long_about = "Build or refresh the persistent Grapha store for a project. Indexing is incremental by default, writes graph/search/localization/asset snapshots under the project store, and can be forced into a full rebuild when inputs or schemas need a clean pass.",
        after_help = "Examples:\n  grapha index .\n  grapha index . --timing\n  grapha index /path/to/project --full-rebuild"
    )]
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
    /// Publish the current local index to a remote Grapha service
    #[command(
        long_about = "Upload the current local graph index as a project revision bundle. CI should run `grapha index .` first, then publish the resulting graph to the shared Grapha service.",
        after_help = "Examples:\n  grapha index .\n  grapha publish --server http://HOST:8080 --channel default"
    )]
    Publish {
        /// Project directory whose .grapha store should be published
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Remote Grapha service base URL, e.g. http://HOST:8080
        #[arg(long)]
        server: String,
        /// Channel to promote after upload. `default` and `release/*` are durable.
        #[arg(long, default_value = remote::DEFAULT_CHANNEL)]
        channel: String,
    },
    /// Launch the HTTP graph explorer
    #[command(
        long_about = "Serve an indexed project as an HTTP graph explorer.",
        after_help = "Examples:\n  grapha index .\n  grapha serve --port 8080"
    )]
    Serve {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Host/interface to bind. Defaults to [serve].host or 0.0.0.0.
        #[arg(long)]
        host: Option<String>,
        /// Port to listen on. Defaults to [serve].port or 8080.
        #[arg(long)]
        port: Option<u16>,
        /// Deprecated compatibility alias for `grapha mcp`
        #[arg(long, hide = true)]
        mcp: bool,
        /// Deprecated compatibility alias for `grapha mcp --watch`
        #[arg(long, hide = true, default_missing_value = "true", num_args = 0..=1, require_equals = true)]
        watch: Option<bool>,
    },
    /// Run the MCP server over stdio for AI agents
    #[command(
        long_about = "Run an indexed project as a JSON-RPC MCP server over stdio for AI agents. Use --watch to keep the graph fresh while files change.",
        after_help = "Examples:\n  grapha index .\n  grapha mcp --watch\n  grapha mcp --watch -p ."
    )]
    Mcp {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Watch for file changes and auto-update the graph. Defaults to [serve].watch or false.
        #[arg(long, default_missing_value = "true", num_args = 0..=1, require_equals = true)]
        watch: Option<bool>,
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
    /// Launch the standalone Grapha HTTP annotation service
    Serve {
        /// Deprecated; annotation service is global and ignores project path
        #[arg(short, long, default_value = ".", hide = true)]
        path: PathBuf,
        /// Port to listen on
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Log file path. Defaults to the Grapha config directory's annotation-service.log.
        #[arg(long)]
        log_file: Option<PathBuf>,
        /// Start the annotation service in the background.
        #[arg(long)]
        daemon: bool,
        /// Deprecated; annotation service has no project watcher
        #[arg(long, hide = true)]
        watch: bool,
    },
    /// Bidirectionally sync annotations with a Grapha annotation service
    #[command(
        long_about = "Sync this project's local annotation records with a standalone Grapha annotation service. The project identity comes from grapha.toml, Git metadata, or the project path so notes survive normal branch switches.",
        after_help = "Examples:\n  grapha annotation sync\n  grapha annotation sync --server http://192.168.1.10:8080\n  GRAPHA_ANNOTATION_SERVER=http://192.168.1.10:8080 grapha annotation sync"
    )]
    Sync {
        /// Annotation service base URL, e.g. http://192.168.1.10:8080.
        /// Defaults to GRAPHA_ANNOTATION_SERVER, project grapha.toml, or global Grapha config.
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
    #[command(
        long_about = "Search the indexed graph by symbol name, locator, module, repository, or file. Add --context when you want snippets and immediate relationships in the same result set.",
        after_help = "Examples:\n  grapha symbol search ViewModel\n  grapha symbol search send --kind function --module Room --fuzzy\n  grapha symbol search ProfileAPI --repo FrameUI --fields file,repo,locator\n  grapha symbol search RoomPage --context --fields full"
    )]
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
        /// Fields to display (comma-separated: score,file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        #[command(flatten)]
        cluster: ClusterArgs,
    },
    /// Query symbol context (callers, callees, implementors)
    #[command(
        long_about = "Show a 360-degree neighborhood for a symbol: callers, callees, reads, writes, implementors, containing scopes, and contained declarations. Tree and brief formats are designed for quick terminal inspection.",
        after_help = "Examples:\n  grapha symbol context RoomPage --format tree\n  grapha symbol context File.swift::helper --fields full\n  grapha symbol context sendGift --format brief --limit 50"
    )]
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
        #[command(flatten)]
        cluster: ClusterArgs,
    },
    /// Analyze blast radius of changing a symbol
    #[command(
        long_about = "Traverse outbound relationships from a symbol to estimate the blast radius of changing it. Increase --depth for a wider search, or use tree/brief formats when reading the result directly.",
        after_help = "Examples:\n  grapha symbol impact GiftPanelViewModel\n  grapha symbol impact GiftPanelViewModel --depth 2 --format tree\n  grapha symbol impact RoomPage --fields file,module,repo"
    )]
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
        #[command(flatten)]
        cluster: ClusterArgs,
    },
    /// Analyze structural complexity of a type (properties, dependencies, invalidation surface)
    Complexity {
        /// Type name or ID to analyze
        symbol: String,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// List the API-like members declared on a type and its extensions
    Api {
        /// Type name or ID to analyze
        symbol: String,
        /// Include private members as well as public/crate-visible members
        #[arg(long)]
        include_private: bool,
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Find usage sites for a symbol, or for every member on a type surface
    Usages {
        /// Symbol, type name, locator, or ID to inspect
        symbol: String,
        /// Source file substring to exclude; repeat for generated/export wrappers
        #[arg(long = "exclude")]
        exclude_files: Vec<String>,
        /// Maximum usage sites returned per target group
        #[arg(long, default_value = "20")]
        limit: usize,
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
        #[command(flatten)]
        cluster: ClusterArgs,
    },
    /// List declarations for multiple files in one JSON payload
    Files {
        /// File names or path suffixes (e.g. "RoomPage.swift" or "src/main.rs")
        #[arg(required = true)]
        files: Vec<String>,
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
    #[command(
        long_about = "Trace execution/dataflow paths through the graph. Forward traces start at an entry or symbol and look for terminal operations; reverse traces start at a symbol or terminal and find entry points that can reach it.",
        after_help = "Examples:\n  grapha flow trace RoomPage --format tree\n  grapha flow trace sendGift --direction reverse\n  grapha flow trace CheckoutView --depth 12 --limit 100"
    )]
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
    #[command(
        long_about = "Find likely API, persistence, cache, event, keychain, or search origins that feed a UI-facing symbol by walking backward through calls, reads, and type relationships.",
        after_help = "Examples:\n  grapha flow origin UserProfileView --terminal-kind network --format tree\n  grapha flow origin GiftBannerPage --fields full\n  grapha flow origin RoomPage --depth 15 --limit 50"
    )]
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
    #[command(
        long_about = "Search for code that likely implements a product/business concept by combining confirmed bindings, aliases, localization text, asset names, and symbol search signals.",
        after_help = "Examples:\n  grapha concept search \"gift banner\" --format tree\n  grapha concept search \"送礼横幅\" --limit 10\n  grapha concept bind \"gift banner\" --symbol GiftBannerPage --symbol GiftBannerViewModel"
    )]
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
        /// Fields to display (comma-separated: file,id,locator,module,repo,span,snippet,visibility,signature,doc_comment,annotation,role; or "full"/"all"/"none")
        #[arg(long)]
        fields: Option<String>,
        #[command(flatten)]
        cluster: ClusterArgs,
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
    #[command(
        long_about = "Validate configured layer dependency rules from grapha.toml against the indexed graph. Use this to catch forbidden module or layer dependencies before they spread.",
        after_help = "Examples:\n  grapha repo arch\n  grapha repo arch --format brief\n\nConfigure layers and deny rules in grapha.toml under [[architecture.layers]] and [[architecture.deny]]."
    )]
    Arch {
        /// Project directory
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Output format
        #[arg(long, value_enum, default_value_t = RepoArchOutputFormat::Json)]
        format: RepoArchOutputFormat,
        #[command(flatten)]
        cluster: ClusterArgs,
    },
    /// Detect code smells across the graph (god types, deep nesting, wide invalidation, etc.)
    #[command(
        long_about = "Scan the indexed graph for structural code smells such as oversized types, deep nesting, broad invalidation surfaces, and suspicious dependency concentration. Scope the scan to keep large repositories readable.",
        after_help = "Examples:\n  grapha repo smells --format brief\n  grapha repo smells --module Room\n  grapha repo smells --file Modules/Room/Sources/Room/View/RoomPage+Layout.swift\n  grapha repo smells --symbol RoomPageCenterContentView --no-cache"
    )]
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
        #[command(flatten)]
        cluster: ClusterArgs,
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
    let verbose = cli.verbose;

    match cli.command {
        Commands::Analyze {
            path,
            output,
            filter,
            compact,
        } => app::pipeline::handle_analyze(path, output, filter, compact, verbose)?,
        Commands::Index {
            path,
            format,
            store_dir,
            full_rebuild,
            timing,
        } => app::index::handle_index(path, format, store_dir, full_rebuild, timing)?,
        Commands::Migrate { path, from, force } => app::migrate::handle_migrate(path, from, force)?,
        Commands::Publish {
            path,
            server,
            channel,
        } => app::publish::handle_publish(path, server, channel)?,
        Commands::Serve {
            path,
            host,
            port,
            mcp,
            watch,
        } => {
            if mcp {
                app::mcp::handle_mcp(path, watch, verbose)?
            } else {
                app::serve::handle_serve(path, host, port, verbose)?
            }
        }
        Commands::Mcp { path, watch } => app::mcp::handle_mcp(path, watch, verbose)?,
        Commands::Annotation { command } => {
            app::annotation::handle_annotation_command(command, verbose)?
        }
        Commands::Symbol { command } => {
            app::query::handle_symbol_command(command, render_options, verbose)?
        }
        Commands::Flow { command } => app::query::handle_flow_command(command, render_options)?,
        Commands::L10n { command } => app::query::handle_l10n_command(command, render_options)?,
        Commands::Asset { command } => app::query::handle_asset_command(command, render_options)?,
        Commands::Concept { command } => {
            app::query::handle_concept_command(command, render_options, verbose)?
        }
        Commands::Repo { command } => app::query::handle_repo_command(command)?,
    }

    Ok(())
}
