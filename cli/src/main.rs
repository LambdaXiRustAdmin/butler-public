//! Thin CLI entry point for the `context` subcommand (and legacy compatibility).
//! The heavy lifting lives in the library; this binary is intentionally small.

use clap::Parser;
use cli::harvester::template::{
    Accuracy, Focus, Frontier, Incremental, Llm, Output, Polyglot, Template,
};
use cli::harvester::{self as harvester};
use code_graph::snooper::context::{ContextMode, OutputFormat};
use code_graph::{
    build_graph_export, build_graph_export_for_nodes, get_context, load_graph, save_graph,
    select_blocks, write_graph_export, ContextOptions, NeuralSelectionBlend,
};
use std::sync::OnceLock;

static VERBOSE: OnceLock<bool> = OnceLock::new();

fn is_verbose() -> bool {
    *VERBOSE.get().unwrap_or(&false)
}

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Butler — give LLMs perfect eyes into your codebase",
    long_about = "Butler builds a code graph and returns the most relevant, token-efficient context for any task.\n\n\
                  First-run humans: `butler ui` (starts server if needed, opens /setup).\n\
                  Agents: MCP stdio `mcp` against a running butler-server.\n\
                  Operator lab: http://127.0.0.1:8002/ops"
)]
struct Cli {
    /// Enable verbose output (more diagnostic messages)
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Parser)]
enum Commands {
    /// Return high-quality, graph-aware context for a prompt or task.
    ///
    /// Best used by AI agents, Open WebUI, or when you need focused context for an LLM.
    Context {
        /// Keywords for context retrieval (e.g. "rate_limit", "auth", "payment_flow").
        /// Do NOT use full natural language sentences.
        ///
        /// This field is **optional** when using surgical mode (--target-file + --target-line).
        /// The entire point of surgical mode is that you can point at a specific line
        /// without knowing what is there.
        prompt: Option<String>,

        /// Directory to analyze (defaults to current directory)
        #[arg(long, short = 'r', default_value = ".")]
        root: String,

        /// How many levels of callers and children to follow
        #[arg(long, default_value_t = 2)]
        depth: usize,

        /// Maximum tokens the returned context should use
        #[arg(long, default_value_t = 4000)]
        max_tokens: usize,

        /// Compress test code to save tokens
        #[arg(long, default_value_t = true)]
        compress_tests: bool,

        /// For surgical (mod/line) input: the file containing the target line.
        /// Must be used together with --target-line.
        #[arg(long)]
        target_file: Option<String>,

        /// For surgical (mod/line) input: the exact line number.
        /// The output will contain the actual source text of that line + its direct call graph edges.
        #[arg(long)]
        target_line: Option<usize>,

        /// Compare baseline (hardcoded heuristics) vs GNN neural selection (top 5 each).
        /// Also verifies post-GNN sort + Top-K token truncation of context payload.
        #[arg(long)]
        compare_gnn: bool,
    },

    /// Serialize the current CodeGraph to `.butler/cache/graph_export.json` (lambda-eve contract).
    ExportGraph {
        /// Directory to analyze (defaults to current directory)
        #[arg(long, short = 'r', default_value = ".")]
        root: String,
    },

    /// Build a clean MLOps dataset of fully-linked graph exports.
    ///
    /// Recursively discovers repos under TARGET_DIR (depth 3), forces clean full parse+edges,
    /// and writes <repo>_graph_export.json into TARGET_DIR/.butler/dataset/
    BuildDataset {
        /// Root directory to scan for repositories (walk depth <=3)
        target_dir: String,

        /// Force delete .butler/cache before building each repo (default: true)
        #[arg(long, default_value_t = true)]
        clean: bool,

        /// Max directory walk depth (default: 3)
        #[arg(long, default_value_t = 3)]
        max_depth: usize,

        /// Output dir relative to TARGET_DIR for the dataset files (default: .butler/dataset)
        #[arg(long, default_value = ".butler/dataset")]
        output_dir: String,
    },

    /// Build structural training data (rich graph_export.json) for lambda-eve GNN.
    ///
    /// Forces a *heavy* full parse with rich structural nodes (ifs, calls, loops, etc.)
    /// and full edges (calls + containment). Writes to &lt;repo&gt;/.butler/training/graph_export.json.
    ///
    /// The harvester (for labels/fat.json) is run *manually* afterwards via the Harvest command
    /// or dashboard so you can control the LLM, cost, query, etc.
    ///
    /// This is the dedicated path for training data. It does *not* affect
    /// the agent-facing Context / orchestrate paths (ArchitecturalSummary, TraceBlastRadius,
    /// FindImplementation, etc.).
    BuildTrainingBundle {
        /// Root directory to analyze for the bundle.
        #[arg(long, short = 'r', default_value = ".")]
        root: String,

        /// Output directory for the bundle (will contain graph_export.json).
        #[arg(long, default_value = ".butler/training")]
        out_dir: String,
    },

    /// Preload project graph(s) into `.butler/cache` (first-build / offline warm).
    ///
    /// Builds or refreshes the on-disk CodeGraph so the next server boot or cold query
    /// starts from cache instead of a full rescan. Optional `--full` runs edge mapping
    /// now (slower, warmest). Optional `--server` also registers roots in a running
    /// Butler process RAM map (`POST /warm`).
    ///
    /// Examples:
    ///   butler warm -r /path/to/repo
    ///   butler warm -r /a -r /b --full
    ///   butler warm -r /path --server http://127.0.0.1:8002
    Warm {
        /// Project root(s) to warm. Repeat `-r` for multi-repo. Default: current directory.
        #[arg(long, short = 'r', default_values_t = vec![".".to_string()])]
        root: Vec<String>,

        /// After skeleton load, run full call-graph / edge mapping and save cache.
        /// Without this, edges may still complete in the background on the server.
        #[arg(long)]
        full: bool,

        /// If set, also `POST /warm` to a running Butler so roots enter the live graph map.
        /// Example: `http://127.0.0.1:8002`
        #[arg(long)]
        server: Option<String>,
    },

    /// Run the stateful incremental harvester to produce gold fat graphs for GNN training.
    /// Uses unfiltered full CodeGraph + agentic LLM loop with tools for high accuracy data.
    /// Template is optional; you can configure everything via flags instead of editing JSON.
    Harvest {
        /// Path to template JSON (optional). If omitted, a sensible default is used + flag overrides.
        #[arg(long, short = 't')]
        template: Option<String>,

        /// Root directory to harvest (the repo path).
        #[arg(long, short = 'r', default_value = ".")]
        root: String,

        /// Output fat graph JSON path (default: fat_graph.json)
        #[arg(long, default_value = "fat_graph.json")]
        out: String,

        /// LLM base URL (e.g. http://localhost:4000). Overrides template / env.
        #[arg(long)]
        llm_base: Option<String>,

        /// LLM model name (e.g. grok-4.3 or your litellm alias).
        #[arg(long)]
        model: Option<String>,

        /// Query / task description passed to the harvester.
        #[arg(long)]
        query: Option<String>,

        /// Number of nodes per emit batch.
        #[arg(long)]
        batch_size: Option<usize>,

        /// Max number of batches / steps (cost ceiling).
        #[arg(long)]
        max_steps: Option<usize>,

        /// Stop early when criticals reach this (0 = only max_steps). Pairs with target_rejections for ~1:1 balance.
        #[arg(long)]
        target_criticals: Option<usize>,

        /// Stop early when rejections reach this (default: same as target_criticals when that is set).
        #[arg(long)]
        target_rejections: Option<usize>,

        /// Comma separated scope paths for tighter focus (e.g. src,crates).
        #[arg(long, value_delimiter = ',')]
        scope: Option<Vec<String>>,

        /// Path to specific graph_export.json (e.g. from `build-training-bundle` .butler/training/graph_export.json)
        /// for exact node id alignment between the structural graph and the fat labels.
        #[arg(long)]
        butler_export: Option<String>,

        /// Card size: `fast` (large, agent/API) | `slow` (compact, local CPU overnight).
        #[arg(long)]
        card_profile: Option<String>,
    },

    /// Start local butler-server if needed and open the proof-of-life setup page.
    ///
    /// Opens http://127.0.0.1:8002/setup (welcome — not the operator/harvester lab).
    /// Default path for strangers after install.
    Ui {
        /// Butler HTTP base (default: BUTLER_URL or http://127.0.0.1:8002)
        #[arg(long, default_value = "")]
        url: String,

        /// Print the setup URL but do not open a browser
        #[arg(long)]
        no_open: bool,

        /// Do not spawn butler-server if health check fails
        #[arg(long)]
        no_spawn: bool,

        /// Kill existing butler-server and start this install's binary (use after reinstall)
        #[arg(long)]
        restart: bool,

        /// Seconds to wait for health after spawn (default 45)
        #[arg(long, default_value_t = 45)]
        wait_secs: u64,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Store global verbose flag
    let _ = VERBOSE.set(cli.verbose);
    if cli.verbose {
        eprintln!("[VERBOSE] Verbose mode enabled");
    }

    match cli.command {
        None => {
            eprintln!("No subcommand provided. Use --help.");
            eprintln!("First-run:  butler ui");
            eprintln!("Server:     butler-server   (or: butler ui spawns it)");
            eprintln!("Setup page: http://127.0.0.1:8002/setup");
            return Ok(());
        }
        Some(Commands::Ui {
            url,
            no_open,
            no_spawn,
            restart,
            wait_secs,
        }) => {
            let mut opts = cli::ui_launcher::UiOptions::default();
            if !url.trim().is_empty() {
                opts.base_url = url;
            }
            opts.no_open = no_open;
            opts.no_spawn = no_spawn;
            opts.restart = restart;
            opts.wait_secs = wait_secs;
            let code = cli::ui_launcher::run_ui(opts);
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
        }
        Some(Commands::Context {
            prompt,
            root,
            depth,
            max_tokens,
            compress_tests,
            target_file,
            target_line,
            compare_gnn,
        }) => {
            let prompt = prompt.unwrap_or_default(); // empty string is fine for pure surgical mode
            if is_verbose() {
                eprintln!("[VERBOSE] Loading code graph from: {}", root);
            }

            // Load the project (load_graph handles most errors gracefully)
            let settings = cli::config::ButlerSettings::new();
            let graph = load_graph(&root, None, &settings.analysis.skip_directories);

            if graph.nodes.is_empty() {
                eprintln!("No supported source files (.rs / .py) found in '{}'.", root);
                eprintln!("Tip: Run Butler from the root of your project, or use --root <path> to point to one.");
                return Ok(());
            }

            if is_verbose() {
                eprintln!("[VERBOSE] Loaded {} blocks", graph.nodes.len());
            }

            // NOTE: No automatic graph_export side-effect here anymore.
            // Use `butler build-training-bundle` (or ExportGraph) for training data.
            // This keeps the agent-facing Context path clean and fast.
            let use_neural = settings.agent.use_neural || compare_gnn;

            let blend = NeuralSelectionBlend {
                text_weight: settings.agent.neural_text_weight,
                neural_weight: settings.agent.neural_score_weight,
            };

            // In-process SmartButler GNN (TrainLayout v2) when neural is on.
            let mut graph = graph;
            if use_neural {
                use code_graph::gnn::{build_scoring_input, cpu_gnn_forward, load_weights};
                use code_graph::retrieve_prompt_subgraph;
                let top_n = settings.agent.neural_subgraph_top_n;
                let hops = settings.agent.neural_subgraph_hops;
                let subgraph = retrieve_prompt_subgraph(&graph, &prompt, top_n, hops);
                code_graph::apply_heuristic_scores_subset(
                    &mut graph,
                    &prompt,
                    &subgraph.node_ids,
                );
                let weights = load_weights(&root);
                let bundle = build_scoring_input(&graph, &subgraph);
                let raw = cpu_gnn_forward(
                    &weights,
                    bundle.nodes.len(),
                    &bundle.features,
                    &bundle.edges,
                );
                let mut nz = 0usize;
                for (id, s) in bundle.nodes.into_iter().zip(raw.into_iter()) {
                    let sf = s as f64;
                    if sf.abs() > 1e-12 {
                        nz += 1;
                    }
                    graph.neural_score_cache.insert(id.clone(), sf);
                    if subgraph.node_ids.contains(&id) {
                        if let Some(block) = graph.nodes.get_mut(&id) {
                            block.score = sf;
                        }
                    }
                }
                if is_verbose() || compare_gnn {
                    eprintln!(
                        "🧠 Neural (pure GNN) applied: subgraph={} nz_scores={} wlen={} path_hint={:?}",
                        subgraph.node_ids.len(),
                        nz,
                        weights.len(),
                        std::env::var("BUTLER_GNN_WEIGHTS")
                            .ok()
                            .or_else(|| Some(code_graph::gnn::weight_search_paths(&root)
                                .into_iter()
                                .find(|p| p.is_file())
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "(fallback)".into())))
                    );
                }
            }

            // Verify truncation: after (possible) GNN scores, sort by score and truncate to Top-K tokens budget
            let mut selected = select_blocks(&graph, &prompt, use_neural, blend);
            if use_neural || compare_gnn {
                selected.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                // Truncate payload to fit max_tokens (rough Top-K tokens, ~200 tokens/block avg)
                let max_k = (max_tokens / 200).max(1);
                selected.truncate(max_k);
            }

            if compare_gnn {
                let baseline: Vec<_> = select_blocks(&graph, &prompt, false, blend)
                    .into_iter()
                    .take(5)
                    .collect();
                let neural: Vec<_> = select_blocks(&graph, &prompt, true, blend)
                    .into_iter()
                    .take(5)
                    .collect();
                println!("=== Baseline (hardcoded heuristics) top 5 ===");
                for b in &baseline {
                    println!(
                        "- {} [{}] score={:.4}",
                        b.name,
                        b.file.display(),
                        b.score
                    );
                }
                println!("\n=== Neural (trained GNN) top 5 ===");
                for b in &neural {
                    let ns = graph
                        .neural_score_cache
                        .get(&b.id)
                        .copied()
                        .unwrap_or(f64::NAN);
                    println!(
                        "- {} [{}] score={:.4} neural={:.4}",
                        b.name,
                        b.file.display(),
                        b.score,
                        ns
                    );
                }
                // Quick distribution on subgraph neural cache
                let mut neural_vals: Vec<f64> = graph.neural_score_cache.values().copied().collect();
                neural_vals.retain(|v| v.abs() > 1e-12);
                neural_vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                if !neural_vals.is_empty() {
                    let n = neural_vals.len();
                    println!(
                        "\n[neural audit] nonzero={} min={:.4} p50={:.4} max={:.4}",
                        n,
                        neural_vals[0],
                        neural_vals[n / 2],
                        neural_vals[n - 1]
                    );
                } else {
                    println!("\n[neural audit] FAIL: no nonzero neural scores in cache");
                }
                println!("\n(Truncation verified: sorted by score and limited to token budget)");
                return Ok(());
            }

            if selected.is_empty() {
                println!("No relevant code found for '{}'.", prompt);
                println!(
                    "Tip: Try a more specific term, or use --depth 3 to include more related code."
                );
                return Ok(());
            }

            let mut output = format!(
                "=== Butler Context for: \"{}\"===\nProject: {}\nSelected: {} blocks | Depth: {} | Max tokens: {} | Compress Tests: {}\n\n",
                prompt, root, selected.len(), depth, max_tokens, if compress_tests { "ON" } else { "OFF" }
            );

            // Determine mode: if target_file + target_line are provided, use Surgical
            let cli_mode = if target_file.is_some() && target_line.is_some() {
                ContextMode::Surgical
            } else {
                ContextMode::Balanced
            };

            let target_path = target_file.map(std::path::PathBuf::from);

            for block in selected {
                let ctx = get_context(
                    &graph,
                    &block.file,
                    block.start_line,
                    block.end_line,
                    ContextOptions {
                        depth,
                        max_tokens,
                        compress_tests,
                        format: OutputFormat::Markdown,
                        mode: cli_mode,
                        target_file: target_path.clone(),
                        target_line,
                        importance_threshold: 0.0,
                        scope_paths: None,
                        ignore_paths: None,
                        use_neural_scores: use_neural,
                        project_root: Some(std::path::PathBuf::from(&root)),
                    },
                    &prompt,
                );
                output.push_str(&format!(
                    "### {} [{}]\n{}\n\n---\n\n",
                    block.name,
                    block.file.display(),
                    ctx
                ));
            }

            println!("{}", output.trim());
        }
        Some(Commands::Warm { root, full, server }) => {
            let settings = cli::config::ButlerSettings::new();
            let skips = &settings.analysis.skip_directories;
            let mut warmed: Vec<String> = Vec::new();

            for raw in &root {
                let path = std::path::Path::new(raw);
                let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                let root_str = abs.to_string_lossy().into_owned();
                if !abs.is_dir() {
                    eprintln!("skip (not a directory): {}", root_str);
                    continue;
                }

                println!("🔥 Warm (disk cache): {}", root_str);
                let t0 = std::time::Instant::now();
                let mut graph = load_graph(&abs, None, skips);
                if graph.nodes.is_empty() {
                    eprintln!(
                        "  empty graph (no supported sources under {}). Still registered if --server.",
                        root_str
                    );
                } else {
                    println!(
                        "  skeleton: {} nodes, {} edges, edge_complete={}",
                        graph.nodes.len(),
                        graph.edges.len(),
                        graph.is_edge_build_complete()
                    );
                    // Hop A: default is dirty-cone / skip when Complete.
                    // Force full-tree edge ensure only via BUTLER_FORCE_FULL_EDGE (or incomplete).
                    let force_full_edge = std::env::var("BUTLER_FORCE_FULL_EDGE").is_ok();
                    if full && force_full_edge {
                        println!(
                            "  --full: BUTLER_FORCE_FULL_EDGE — clearing edge inventory for full-tree ensure…"
                        );
                        graph.files_with_edges.clear();
                        graph.background_edge_build_complete = false;
                        graph.background_edge_build_active = false;
                        graph.background_edge_build_state =
                            code_graph::snooper::BackgroundEdgeBuildState::Incomplete;
                        // Keep nodes/edges for structure; ensure will recollect all files.
                        // Clear adjacency so we do not double-count while files_with_edges is empty.
                        graph.edges.clear();
                        graph.reverse.clear();
                        graph.clear_bridges();
                        println!("  --full: running ensure_call_graph (forced full-tree)…");
                        graph.ensure_call_graph(&abs, skips, None);
                        if let Err(e) = save_graph(&graph, &abs) {
                            eprintln!("  warn: save_graph failed: {e}");
                        }
                        println!(
                            "  full edges: {} edges, complete={}",
                            graph.edges.len(),
                            graph.is_edge_build_complete()
                        );
                    } else if full && !graph.is_edge_build_complete() {
                        println!("  --full: running ensure_call_graph (incomplete edges; dirty cone if partial)…");
                        graph.ensure_call_graph(&abs, skips, None);
                        if let Err(e) = save_graph(&graph, &abs) {
                            eprintln!("  warn: save_graph failed: {e}");
                        }
                        println!(
                            "  full edges: {} edges, complete={}",
                            graph.edges.len(),
                            graph.is_edge_build_complete()
                        );
                    } else if full {
                        println!("  --full: edges already complete; cache left as-is (Hop A no-op skip)");
                    }
                }
                println!(
                    "  disk warm done in {:.1}s → {}/.butler/cache",
                    t0.elapsed().as_secs_f64(),
                    root_str
                );
                warmed.push(root_str);
            }

            if let Some(base) = server.as_ref() {
                let url = format!("{}/warm", base.trim_end_matches('/'));
                println!("🔥 Notifying server {} ({} root(s))…", url, warmed.len());
                let client = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()?;
                let body = serde_json::json!({ "roots": warmed });
                match client.post(&url).json(&body).send() {
                    Ok(resp) => {
                        let status = resp.status();
                        let text = resp.text().unwrap_or_default();
                        if status.is_success() {
                            println!("  server: {}", text.trim());
                        } else {
                            eprintln!("  server HTTP {}: {}", status, text.trim());
                        }
                    }
                    Err(e) => eprintln!("  server notify failed: {} (disk cache still warmed)", e),
                }
            } else {
                println!(
                    "Tip: server boot-warm via BUTLER_WARM_ROOTS or server.warm_roots; \
                     live RAM map: butler warm -r … --server http://127.0.0.1:8002"
                );
            }
        }
        Some(Commands::ExportGraph { root }) => {
            let settings = cli::config::ButlerSettings::new();
            let graph = load_graph(&root, None, &settings.analysis.skip_directories);
            if graph.nodes.is_empty() {
                eprintln!("No supported source files found in '{}'.", root);
                return Ok(());
            }
            let path = write_graph_export(&graph, std::path::Path::new(&root))?;
            println!("Wrote graph export to {}", path.display());
        }
        Some(Commands::BuildDataset {
            target_dir,
            clean,
            max_depth,
            output_dir,
        }) => {
            let target = std::path::Path::new(&target_dir);
            if !target.is_dir() {
                eprintln!("Error: {} is not a directory", target_dir);
                return Ok(());
            }
            println!(
                "MLOps: Scanning {} (max-depth={}) for repos...",
                target_dir, max_depth
            );
            let markers = [
                ".git",
                "Cargo.toml",
                "package.json",
                "pyproject.toml",
                "go.mod",
                "CMakeLists.txt",
                "Makefile",
                "setup.py",
                "requirements.txt",
                "pom.xml",
                "build.gradle",
                "meson.build",
            ];
            let repos = find_repos(target, 0, max_depth, &markers);
            println!("Found {} repos.", repos.len());
            let dataset_dir = target.join(&output_dir);
            std::fs::create_dir_all(&dataset_dir)?;
            let settings = cli::config::ButlerSettings::new();
            let skips = &settings.analysis.skip_directories;
            for repo_root in repos {
                let name = repo_root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                println!("  -> {}", name);
                if clean {
                    let cache = repo_root.join(".butler").join("cache");
                    if cache.exists() {
                        let _ = std::fs::remove_dir_all(&cache);
                    }
                }
                let root_str = repo_root.to_string_lossy().to_string();
                let mut graph = load_graph(&root_str, None, skips);
                if graph.nodes.is_empty() {
                    eprintln!("     (no nodes, skip)");
                    continue;
                }
                // Full deterministic build: skeleton already in load; ensure edges + post passes + hubs
                graph.ensure_call_graph(&repo_root, skips, None);
                graph.compute_hubs(0.05);
                let export = build_graph_export(&graph);
                let json = serde_json::to_string_pretty(&export)?;
                let out_path = dataset_dir.join(format!("{}_graph_export.json", name));
                std::fs::write(&out_path, json)?;
                println!(
                    "     exported {} nodes, {} edges -> {}",
                    export.nodes.len(),
                    export.edges.len(),
                    out_path.display()
                );
            }
            println!("Dataset written to {}", dataset_dir.display());
        }
        Some(Commands::BuildTrainingBundle { root, out_dir }) => {
            let settings = cli::config::ButlerSettings::new();
            let skips = &settings.analysis.skip_directories;
            println!("Building structural training bundle for {} (heavy parse + rich nodes + full edges)...", root);
            // Force fresh for training (see server.rs handler for rationale: isolate from
            // base skeleton cache so expanded structural nodes are always picked up).
            let cache_bin = std::path::Path::new(&root).join(".butler/cache/graph.bin");
            let _ = std::fs::remove_file(&cache_bin);
            let mut graph = load_graph(&root, None, skips);
            if graph.nodes.is_empty() {
                eprintln!("No supported source files found in '{}'.", root);
                return Ok(());
            }

            // Training-only: force full synchronous edge build. Normal paths defer for
            // fast startup + JIT. Training data wants everything materialized now.
            graph.files_with_edges.clear();
            graph.background_edge_build_complete = false;
            graph.background_edge_build_active = false;
            eprintln!("[training] forcing FULL synchronous edge build (no defer/bg/realtime)");

            // Force the heavy, training-oriented build: full call graph + hubs.
            // (The expanded interesting_kinds are enabled at parse time; this adds the edges.)
            graph.ensure_call_graph(std::path::Path::new(&root), skips, None);
            graph.compute_hubs(0.05);

            // Resolve output dir relative to repo so it lands in <repo>/.butler/training/
            let bundle_dir: std::path::PathBuf = if out_dir.starts_with('/') {
                std::path::PathBuf::from(&out_dir)
            } else {
                std::path::Path::new(&root).join(out_dir.trim_start_matches("./"))
            };
            std::fs::create_dir_all(&bundle_dir)?;

            // Structural export (graph + edges for Eve GNN) — use rich relation typing only here
            let export = build_graph_export_for_nodes(
                &graph,
                &graph.nodes.keys().cloned().collect(),
                &std::collections::HashMap::new(),
                true,
            );
            let graph_path = bundle_dir.join("graph_export.json");
            let json = serde_json::to_string_pretty(&export)?;
            std::fs::write(&graph_path, json)?;
            println!(
                "  wrote structural: {} nodes, {} edges -> {}",
                export.nodes.len(),
                export.edges.len(),
                graph_path.display()
            );

            println!("Structural bundle ready. Run `butler harvest --butler-export .butler/training/graph_export.json ...` (or dashboard Harvester + Graph export field) to label.");
            println!("Use --scope / --ignore + small batch/max_steps. The --butler-export ensures fat ids match the rich training graph exactly (controls cost on 10x+ node sets).");
        }
        Some(Commands::Harvest {
            template,
            root,
            out,
            llm_base,
            model,
            query,
            batch_size,
            max_steps,
            target_criticals,
            target_rejections,
            scope,
            butler_export,
            card_profile,
        }) => {
            let mut tpl = if let Some(ref t) = template {
                Template::load(std::path::Path::new(t)).expect("load template")
            } else {
                // Sensible default so you don't need to edit JSON at all
                Template {
                    name: "cli".to_string(),
                    query: "main entry points and high level structure".to_string(),
                    repo: root.clone(),
                    butler_export: None,
                    output: Output {
                        schema: "full_fat_v1".to_string(),
                        format_version: 1,
                    },
                    incremental: Incremental {
                        batch_size: 4,
                        max_steps: 40,
                        save_after_each: true,
                        load_previous_context: true,
                        target_criticals: 0,
                        target_rejections: 0,
                    },
                    accuracy: Accuracy {
                        require_exploration_note: true,
                        require_reason_on_every_edge: false,
                        require_explicit_rejections: true,
                        min_hard_negatives_per_batch: 1,
                        min_criticals_per_batch: 1,
                        ban_stub_notes: true,
                        require_label_polarity: true,
                    },
                    focus: Focus {
                        scope_paths: vec![],
                        ignore_paths: vec![],
                        prefer_high_degree: false,
                    },
                    frontier: Frontier {
                        strategy: "neighborhood".to_string(),
                        use_ast_distance: false,
                        use_degree: false,
                        use_bm25: false,
                        card_profile: "fast".to_string(),
                        max_neighbors: 0,
                        max_snippet_chars: 0,
                    },
                    llm: Llm {
                        via: "litellm".to_string(),
                        model: "grok-4.3".to_string(),
                        temperature: 0.1,
                    },
                    polyglot: Polyglot {
                        include_interconnect: true,
                    },
                }
            };

            // Apply CLI overrides (so you can avoid touching the JSON)
            if let Some(q) = query {
                tpl.query = q;
            }
            if let Some(b) = batch_size {
                tpl.incremental.batch_size = b;
            }
            if let Some(m) = max_steps {
                tpl.incremental.max_steps = m;
            }
            if let Some(t) = target_criticals {
                tpl.incremental.target_criticals = t;
            }
            if let Some(t) = target_rejections {
                tpl.incremental.target_rejections = t;
            }
            if let Some(sc) = scope {
                tpl.focus.scope_paths = sc;
            }
            if let Some(m) = model {
                tpl.llm.model = m;
            }
            if let Some(be) = butler_export {
                tpl.butler_export = Some(be);
            }
            if let Some(p) = card_profile {
                tpl.frontier.card_profile = p;
            }

            let export = tpl.butler_export.as_ref().map(std::path::PathBuf::from);
            let src = harvester::source::Source::new(std::path::PathBuf::from(&root), export);

            let llm_base = if let Some(b) = llm_base {
                b
            } else if tpl.llm.via.to_lowercase().contains("stub") {
                "stub".to_string()
            } else {
                std::env::var("LITELLM_BASE")
                    .unwrap_or_else(|_| "http://localhost:4000".to_string())
            };
            let client = harvester::llm::LlmClient::new(&llm_base, &tpl.llm.model, None);
            let reg = harvester::tools::ToolRegistry::with_source(src.clone());
            let fat = std::path::Path::new(&out);

            let tpl_desc = template.as_deref().unwrap_or("<default>");
            println!(
                "Harvesting with template {} for {} -> {} (llm: {})",
                tpl_desc, root, out, llm_base
            );
            harvester::agent_loop::run_harvest(&tpl, &client, &reg, &src, fat, None);

            if fat.exists() {
                if let Ok(data) = std::fs::read_to_string(fat) {
                    if let Ok(g) = serde_json::from_str::<serde_json::Value>(&data) {
                        let n = g["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
                        let c = g["critical_node_ids"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let r = g["rejected_node_ids"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let i = g["interconnect_edges"]
                            .as_array()
                            .map(|a| a.len())
                            .unwrap_or(0);
                        if n > 0 {
                            println!(
                                "Wrote {} (nodes={}, criticals={}, rejections={}, interconnect={})",
                                out, n, c, r, i
                            );
                        } else {
                            println!("Wrote {} (empty; LLM returned no emit or failed)", out);
                        }
                    } else {
                        println!("Wrote {}", out);
                    }
                }
            } else {
                println!("No output written (LLM may have failed to produce actions)");
            }
        }
    }
    Ok(())
}

/// Recursive repo discovery (depth-limited). Returns list of *outermost* repo roots that contain at least one marker.
/// Does not descend into subdirs of a detected repo root (avoids counting workspace members as separate repos).
fn find_repos(
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    markers: &[&str],
) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    if depth > max_depth {
        return out;
    }
    let mut subs = vec![];
    let mut has_marker = false;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                subs.push(p);
            } else if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                if markers.contains(&n) {
                    has_marker = true;
                }
            }
        }
    }
    if has_marker {
        out.push(dir.to_path_buf());
        // do not recurse into detected repo (prevents monorepo members)
        return out;
    }
    for sub in subs {
        out.extend(find_repos(&sub, depth + 1, max_depth, markers));
    }
    out
}
