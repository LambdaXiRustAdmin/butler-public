//! Single source of truth for LLM-facing Butler usage instructions and tool descriptions.
//! Prefer dense agent contracts over English tutorials — models already know code-shaped input.
//! Tutor-copy (OOD tool): pre-empt grep, mandate 2–3 iterative pulls, always leave next:.

/// Dense structural query contract for agents (not human prose).
/// Server matches Ident/Path against CodeGraph membership; NL is not interpreted.
pub const BUTLER_QUERY_CONTRACT: &str = "\
Butler.Query v1 (structural; not NL)\n\
fields: project:Path! goal?:Arch|Trace|Find target_symbol?:Ident scope_paths?:Path[] ignore_paths?:Path[] prompt?:str detail?:short|long|compact|dense\n\
goal: ArchitecturalSummary|TraceBlastRadius|FindImplementation  # Trace|Find ⇒ target_symbol!\n\
prompt: free text OPTIONAL — server keeps only strong Ident|snake|camel|Path|foo::bar that ∈ CodeGraph(names∪paths); prose→∅; ∅⇒miss(no_structural_hits)\n\
prefer: target_symbol+scope_paths ≫ prompt; never prose-only questions\n\
ok:  {project,goal:FindImplementation,target_symbol:load_graph,scope_paths:[\"src/\"]}\n\
ok:  {project,prompt:\"load_graph\"} | {project,prompt:\"src/gnn/forward.rs\"}\n\
bad: {prompt:\"how does auth work?\"} | {prompt:\"gdzie jest uwierzytelnianie?\"}  # no Ident\n\
miss: no_structural_hits ONLY when edges_full — incomplete ⇒ symbol_not_seen_yet@N% (rewalk; not dead)\n\
partial: Honest partial banner + confidence∈{inventory,index_exact,edges_partial,edges_full}; 0 callers so-far ≠ delete\n\
loop: expect 2-3 butler_ask calls (orient/pin/trace); follow content next: — do not abandon for grep after first empty/partial\n\
cold: content starts with \"=== Building Graph (cold)\" + progress bar → first open / empty cache; depending on repo size this may take a while (seconds…minutes); retry few seconds (MCP auto-retries); not stuck if %/file changes; auto-skips bundled-vendor segments (vendor/_vendor/_click/third_party/…) + docs_src; extend via analysis.extra_bundled_vendor_segments; Arch can use skeleton before edges done; scoring=inproc GNN; detail=short|long (aliases compact|dense); machine=structuredContent always";

/// Full usage guide returned by `butler_help` and embedded MCP prompts.
pub const BUTLER_ORCHESTRATE_INSTRUCTIONS: &str = "\
=== BUTLER (agent contract) ===\n\
PRIMARY: butler_ask(project, symbol?, scope_paths?, mode=auto). Power: butler_orchestrate(goal=…). Legacy context/search/map/inspect only if expert_mode.\n\
CRITICAL: structural questions (callers, blast, dual-stack, homonyms) → butler_ask first. grep/rg is blind to typed bridges (export/ipc) and floods context; do not fall back to grep after one empty butler result.\n\
\n\
Butler.Query (structural; not NL)\n\
ask: project:Path! symbol?:Ident query?:Ident|Path scope_paths?:Path[] mode?:auto|trace|find|arch detail?:short|long|compact|dense\n\
orch: project! goal∈{TraceBlastRadius,ArchitecturalSummary,FindImplementation} target_symbol?:Ident scope_paths?\n\
prefer: symbol+scope_paths ≫ prose; never prose-only questions\n\
ok:  {project,symbol:load_graph,scope_paths:[\"src/\"]}\n\
ok:  {project,mode:arch,scope_paths:[\"src/\"]}\n\
bad: {query:\"how does auth work?\"}  # no Ident\n\
miss: no_structural_hits ONLY when edges_full — incomplete ⇒ symbol_not_seen_yet@N% (rewalk; not dead)\n\
partial: Honest partial + confidence ladder; 0 callers so-far ≠ dead code\n\
cold: BUILDING + toc + wait_policy — usable partial, not a hang; soft wall → confirm_long_wait\n\
\n\
detail length (agent chooses — no mind-reading):\n\
  short|compact (default) = trust dossier + tight neighbor sample (orient/pin/bridges)\n\
  long|dense = full dump + larger neighbor sample (edit under pin)\n\
  Prefer short first; re-ask detail=long same scope_paths if sample thin. Honesty identical both ways.\n\
Never JSON.parse(content) as sole report.\n\
loop: expect 2–3 iterative butler_ask calls before concluding missing:\n\
  (1) mode=arch if map unknown → suggested_scopes\n\
  (2) symbol may disambiguate → pin scope_paths from locations\n\
  (3) re-Trace short → optional detail=long under same pin\n\
Always follow content line next: (or structured.next_action). Do not switch to bash grep after first empty/partial.\n";

/// Primary MCP façade — one tool agents should default to.
/// Aggressive pre-emption + multi-turn habit (OOD tool vs pre-trained grep).
pub const BUTLER_ASK_TOOL_DESCRIPTION: &str = "\
CRITICAL: PRIMARY tool for structural codebase questions (who calls X, blast radius, dual-stack/FFI, homonyms). \
Always prefer this over grep/rg/find for those tasks — text search misses export/ipc bridges and pollutes context. \
Expect 2–3 iterative calls before concluding a symbol is missing: (1) mode=arch if orientation needed, \
(2) symbol Trace may return disambiguate → pin scope_paths from locations/suggested_scopes, \
(3) re-ask same symbol with scope → success shows receipt: high|… in content. \
On miss/disambiguate/BUILDING/partial: follow the content line next: (or structured.next_action); \
do NOT abandon butler_ask for bash after one empty or partial result. \
Args: project! symbol|target_symbol|query? mode∈{auto,trace,find,arch,map}? scope_paths? detail∈{short,long,compact,dense}?. \
Ident/Path only (not NL chat). Prefer symbol+scope_paths for short names (App,Mutex,build,run). \
Cold: status:BUILDING + toc + wait_policy — usable partial, not a hang. Soft wall ~15min: BUILDING_SOFT_WALL → operator then confirm_long_wait=true. \
detail: short|compact (default)=dossier+tight sample; long|dense=full dump+larger sample — prefer short then long under same pin. \
content short Trace = receipt, neighbors [basis|trust], reverse CALL spine, bridges, next; Arch short = tree+hubs+coverage+next. structuredContent=full machine report.";

pub const BUTLER_ORCHESTRATE_TOOL_DESCRIPTION: &str = "\
Power tool: explicit goal routing. Prefer butler_ask for normal use. \
Same multi-turn rules: structural queries here not grep; expect 2–3 calls; follow content next: on miss/disambiguate/BUILDING. \
Args: project! goal∈{ArchitecturalSummary,TraceBlastRadius,FindImplementation} (synonyms: architect,trace,find) target_symbol? scope_paths? detail∈{short,long,compact,dense} confirm_long_wait?. \
prompt≠NL: CodeGraph Ident/Path only. Mega-homonyms: pin scope_paths. Cold: BUILDING+wait_policy; detail short=dossier+tight sample, long=larger sample under pin.";

/// Short description for `butler_help` in MCP tool lists and manifests.
pub const BUTLER_HELP_TOOL_DESCRIPTION: &str =
    "Butler.Query contract. PRIMARY butler_ask (not grep for structural). Expect 2–3 iterative asks; follow content next:. detail short|long (aliases compact|dense): short=receipt+basis+spine+bridges+next+tight sample; long=larger sample under pin. cold=BUILDING+wait_policy; soft wall→confirm_long_wait. miss=no_structural_hits only when edges_full else symbol_not_seen_yet@N%.";

/// Dense inline nudge when free-text looks like prose (prepended to results / guidance).
pub fn dense_nl_nudge(reason: &str) -> String {
    format!(
        "[Butler.Query] prose_detected({reason}) → prompt must carry Ident|Path ∈ graph; prefer target_symbol. \
next: butler_ask(project, symbol:<Ident>, scope_paths?) — not NL. contract: butler_help"
    )
}

/// Dense miss body when structural membership is empty **and** the edge inventory is complete.
pub fn dense_structural_miss(project: &str, prompt: &str, unmatched: &[String]) -> String {
    let u = if unmatched.is_empty() {
        String::new()
    } else {
        format!(" unmatched_strong={:?}", unmatched)
    };
    format!(
        "Butler.Query miss no_structural_hits project={project:?} prompt={prompt:?}{u}\n\
         do_not: fall back to grep as first alternative — verify Ident/project first\n\
         next: butler_ask(project={project:?}, symbol:<exact Ident>, scope_paths:[\"src/\"?]) or mode=arch to orient; check project root\n\
         {contract}",
        contract = BUTLER_QUERY_CONTRACT
            .lines()
            .take(6)
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Provisional miss while the warehouse is still building — **never** `no_structural_hits`.
///
/// Agents must rewalk, not assume the symbol is absent/dead.
pub fn symbol_not_seen_yet_miss(
    project: &str,
    prompt: &str,
    percent: usize,
    unmatched: &[String],
) -> String {
    let p = percent.min(99);
    let u = if unmatched.is_empty() {
        String::new()
    } else {
        format!(" unmatched_strong={:?}", unmatched)
    };
    format!(
        "Butler.Query provisional_miss symbol_not_seen_yet@{p}% project={project:?} prompt={prompt:?}{u}\n\
         do_not: treat as missing/dead code; do_not: abandon for grep — graph edges incomplete ({p}%)\n\
         next: retry same butler_ask in ~30s (or when /mcp/health edge_builds percent climbs); optional tighter scope_paths\n\
         reserved: no_structural_hits is only for edges_full + zero membership\n\
         {contract}",
        contract = BUTLER_QUERY_CONTRACT
            .lines()
            .take(8)
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisional_miss_forbids_no_structural_hits_token() {
        let m = symbol_not_seen_yet_miss("/p", "App", 15, &[]);
        assert!(m.contains("symbol_not_seen_yet@15%"), "{m}");
        assert!(m.contains("provisional_miss"), "{m}");
        // Body may mention the reserved token as documentation, but warning path uses provisional only.
        assert!(m.contains("reserved: no_structural_hits"), "{m}");
        assert!(m.contains("next:"), "{m}");
        assert!(m.contains("do_not:"), "{m}");
    }

    #[test]
    fn complete_miss_uses_no_structural_hits() {
        let m = dense_structural_miss("/p", "Nope", &[]);
        assert!(m.contains("no_structural_hits"), "{m}");
        assert!(!m.contains("symbol_not_seen_yet"), "{m}");
        assert!(m.contains("next:"), "{m}");
        assert!(m.contains("butler_ask"), "{m}");
    }

    #[test]
    fn ask_tool_description_preempts_grep_and_mandates_loop() {
        let d = BUTLER_ASK_TOOL_DESCRIPTION;
        assert!(d.contains("CRITICAL"), "{d}");
        assert!(d.contains("grep") || d.contains("rg"), "{d}");
        assert!(d.contains("2–3") || d.contains("2-3"), "{d}");
        assert!(d.contains("next:"), "{d}");
        assert!(d.contains("do NOT") || d.contains("do not") || d.contains("Do not"), "{d}");
    }

    #[test]
    fn instructions_loop_mentions_iterative_asks() {
        assert!(BUTLER_ORCHESTRATE_INSTRUCTIONS.contains("2–3") || BUTLER_ORCHESTRATE_INSTRUCTIONS.contains("2-3"));
        assert!(BUTLER_ORCHESTRATE_INSTRUCTIONS.contains("next:"));
    }
}
