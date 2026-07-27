//! Trace/seed filters: noise, role tiers, homonym rank, module shells, cites.
//!
//! # Concern map
//!
//! | Module | Owns |
//! |--------|------|
//! | [`cap`] | Score/string payload caps |
//! | [`noise`] | Path noise + [`application_path_priority`] |
//! | [`seed_tier`] | [`seed_role_tier`], relative [`filter_seed_candidates`] |
//! | [`homonym`] | Homonym ladder, qualification, trace-noise names |
//! | [`module_shell`] | Module shell → interior resolution |
//! | [`entry`] | Entry landmarks, structural multipliers, type-trace |
//! | [`cite`] | Cite pack + CallerCallee stamping |
//! | [`partition`] | Trace core/utility split + degenerate responses |
//!
//! # Filter semantics (silent vs hard vs scored)
//!
//! | Mechanism | Mode | Meaning |
//! |-----------|------|---------|
//! | [`filter_seed_candidates`] | **Relative silent drop** | If a better class exists in the *set*, drop worse (tier/test/shell/ctor). Same block may keep or drop depending on peers. |
//! | [`is_trace_noise_name`] | **Hard reject** | Name is always noise (`Box`, `HashMap`, `next`, …) regardless of set. |
//! | [`qualification_evidence`] | **Scored** | Higher = stronger namespace/path match; callers pick winners. |
//!
//! # Homonym ladder (high → low) — [`pick_best_homonym_with_in_degree`]
//!
//! 1. Basename symbol match (Type.h convention hard filter when any match)  
//! 2. Role tier (+ forward-shell / ctor demotion)  
//! 3. C/C++ definition-body preference  
//! 4. Directed in-degree **when both basename-match** (topology before path)  
//! 5. Path context score (`homonym_context_score`)  
//! 6. Application path priority  
//! 7. Non-test preferred  
//! 8. In-degree (all pairs)  
//! 9. Line span, then source length  
//!
//! No global hub `score` (celebrity steal). Do not reorder without dual-stack keepers.

mod cap;
mod noise;
mod seed_tier;
mod homonym;
mod module_shell;
mod entry;
mod cite;
mod partition;

pub use cap::*;
pub use noise::*;
pub use seed_tier::*;
pub use homonym::*;
pub use module_shell::*;
pub use entry::*;
pub use cite::*;
pub use partition::*;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use code_graph::{BlockInfo, CodeGraph};
    use code_graph::snooper::Id;

    use super::*;

    /// Zero-degree wrapper for unit tests (no warehouse reverse map).
    fn pick_best_homonym<'a>(
        candidates: impl IntoIterator<Item = &'a BlockInfo>,
    ) -> Option<&'a BlockInfo> {
        pick_best_homonym_with_in_degree(candidates, |_| 0)
    }

    fn test_block(file: &str) -> BlockInfo {
        BlockInfo {
            id: Id::new(file, "function_item", "testhash123456"),
            name: "helper".into(),
            file: Path::new(file).to_path_buf(),
            kind: "function_item".into(),
            lang: "go".into(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            parent_id: None,
            children: vec![],
            content_hash: "test".into(),
            sig_hash: "sig".into(),
            git_blame_recency: None,
            git_author: None,
            has_cycle: false,
            is_macro_expanded: false,
            source: String::new(),
            score: 1.0,
            usages: vec![],
            external_crates: HashSet::new(),
            is_highly_connected: false,
        }
    }

    #[test]
    fn application_path_priority_prefers_crates_over_tools_src() {
        let bevy = application_path_priority("crates/bevy_app/src/app.rs");
        let tools = application_path_priority("tools/export-content/src/app.rs");
        assert!(bevy > tools, "bevy={bevy} should outrank tools={tools}");
    }

    #[test]
    fn application_path_priority_root_src_beats_peripheral_nested_src() {
        let root_src = application_path_priority("src/main.rs");
        let tools_src = application_path_priority("tools/foo/src/bar.rs");
        assert!(root_src > tools_src);
    }

    #[test]
    fn benchmarks_demoted_vs_package_src() {
        // A′.10: rich/benchmarks must not outrank rich/console.py spines.
        let prod = application_path_priority("rich/console.py");
        let bench = application_path_priority("benchmarks/benchmarks.py");
        assert!(prod > bench, "prod={prod} bench={bench}");
        let bench_block = test_block("rich/benchmarks/benchmarks.py");
        assert!(
            is_testish_seed_block(&bench_block),
            "benchmarks/ segment is testish for seed ranking"
        );
        let prod_block = test_block("rich/console.py");
        assert!(!is_testish_seed_block(&prod_block));
    }

    #[test]
    fn cap_blocks_by_score_keeps_top_n() {
        let mk = |name: &str, score: f64| {
            let mut b = test_block(&format!("src/{name}.rs"));
            b.name = name.into();
            b.score = score;
            b
        };
        let (capped, omitted) =
            cap_blocks_by_score(vec![mk("low", 1.0), mk("high", 99.0), mk("mid", 50.0)], 2);
        assert_eq!(omitted, 1);
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].name, "high");
        assert_eq!(capped[1].name, "mid");
    }

    #[test]
    fn pick_best_homonym_prefers_struct_over_impl() {
        let mut struct_app = test_block("crates/bevy_app/src/app.rs");
        struct_app.name = "App".into();
        struct_app.kind = "struct_item".into();
        struct_app.score = 50.0;
        struct_app.source = "pub struct App { }".into();

        let mut impl_app = test_block("crates/bevy_app/src/app.rs");
        impl_app.id = Id::new("crates/bevy_app/src/app.rs", "impl_item", "implhash123456");
        impl_app.name = "App".into();
        impl_app.kind = "impl_item".into();
        impl_app.score = 100.0;
        impl_app.source = "impl App { fn x() {} }".into();

        let best = pick_best_homonym([&struct_app, &impl_app]).unwrap();
        assert_eq!(best.kind, "struct_item");
    }

    #[test]
    fn pick_best_homonym_prefers_tensorimpl_h_over_forward_decl() {
        // pytorch bug: MemoryOverlap.h `struct TensorImpl;` beat the real type file.
        let mut forward = test_block("aten/src/ATen/MemoryOverlap.h");
        forward.name = "TensorImpl".into();
        forward.kind = "struct_specifier".into();
        forward.lang = "cpp".into();
        forward.score = 999_999.0; // hub celebrity must not win
        forward.source = "struct TensorImpl;".into();
        forward.start_line = 6;
        forward.end_line = 6;

        let mut real = test_block("c10/core/TensorImpl.h");
        real.id = Id::new("c10/core/TensorImpl.h", "struct_specifier", "realhash1234567");
        real.name = "TensorImpl".into();
        real.kind = "struct_specifier".into();
        real.lang = "cpp".into();
        real.score = 1.0;
        real.source = "struct TensorImpl {\n  void* data;\n  int64_t numel;\n};\n".into();
        real.start_line = 64;
        real.end_line = 900;

        let best = pick_best_homonym([&forward, &real]).unwrap();
        assert!(
            best.file.to_string_lossy().contains("TensorImpl.h"),
            "got {:?}",
            best.file
        );
        assert!(!best.source.contains("struct TensorImpl;"));
        assert!(best.source.contains('{'));
    }

    #[test]
    fn class_specifier_is_type_hub_tier() {
        // tree-sitter-cpp uses class_specifier — must not fall to tier 20 and lose to ctors.
        assert_eq!(seed_role_tier("class_specifier"), 100);
        assert!(is_type_trace_target("class_specifier"));
    }

    #[test]
    fn pick_best_homonym_prefers_class_body_over_constructor() {
        // Constructor Trap: no tree-sitter "constructor" kind — emulated via heuristics.
        let mut class_body = test_block("c10/core/TensorImpl.h");
        class_body.id = Id::new("c10/core/TensorImpl.h", "class_specifier", "classhash123456");
        class_body.name = "TensorImpl".into();
        class_body.kind = "class_specifier".into();
        class_body.lang = "cpp".into();
        class_body.source = "class TensorImpl {\n public:\n  TensorImpl();\n  void* data_;\n};\n".into();
        class_body.start_line = 100;
        class_body.end_line = 800;
        class_body.score = 1.0;

        let mut ctor = test_block("c10/core/TensorImpl.h");
        ctor.id = Id::new("c10/core/TensorImpl.h", "function_definition", "ctorhash1234567");
        ctor.name = "TensorImpl".into();
        ctor.kind = "function_definition".into();
        ctor.lang = "cpp".into();
        ctor.source = "TensorImpl::TensorImpl() {}\n".into();
        ctor.start_line = 574;
        ctor.end_line = 576;
        ctor.score = 999_999.0; // hub celebrity must not win

        assert!(is_likely_constructor_or_destructor(&ctor));
        assert!(!is_likely_constructor_or_destructor(&class_body));
        assert!(is_type_seed_hub(&class_body));

        let best = pick_best_homonym([&ctor, &class_body]).unwrap();
        assert_eq!(best.kind, "class_specifier");
        assert!(best.source.contains("class TensorImpl"));
        assert!(best.end_line - best.start_line > 10);
    }

    #[test]
    fn pick_best_homonym_ctor_only_still_selectable() {
        // No type hub in the set → constructor remains a valid seed.
        let mut ctor = test_block("c10/core/TensorImpl.cpp");
        ctor.name = "TensorImpl".into();
        ctor.kind = "function_definition".into();
        ctor.lang = "cpp".into();
        ctor.source = "TensorImpl::TensorImpl(int x) { (void)x; }\n".into();

        let best = pick_best_homonym([&ctor]).unwrap();
        assert_eq!(best.kind, "function_definition");
        assert!(is_likely_constructor_or_destructor(best));
    }

    #[test]
    fn pick_best_homonym_in_degree_prefers_hub_header_over_leaf_cpp() {
        // P3: same exact name; high reverse edges → interface door; leaf cpp loses.
        let mut leaf = test_block("src/nsCOMArray.cpp");
        leaf.name = "nsISupports".into();
        leaf.kind = "class_specifier".into();
        leaf.source = "class nsISupports { void f(); };".into();
        leaf.start_line = 10;
        leaf.end_line = 20;
        leaf.score = 99.0;

        let mut hub = test_block("src/nsISupports.h");
        hub.id = Id::new("src/nsISupports.h", "class_specifier", "nsihubhash123456");
        hub.name = "nsISupports".into();
        hub.kind = "class_specifier".into();
        hub.source = "class nsISupports {\n public:\n  virtual void AddRef() = 0;\n};\n".into();
        hub.start_line = 1;
        hub.end_line = 40;
        hub.score = 1.0;

        // Without gravity, door ext (.h) already helps; with gravity, hub wins hard.
        let best = pick_best_homonym_with_in_degree([&leaf, &hub], |b| {
            if b.file.to_string_lossy().ends_with("nsISupports.h") {
                500
            } else {
                0
            }
        })
        .unwrap();
        assert!(
            best.file.to_string_lossy().ends_with("nsISupports.h"),
            "got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_basename_still_beats_high_in_degree_wrong_file() {
        // Basename hard rule must not be overridden by celebrity in-degree.
        let mut wrong = test_block("other/OwningNonNull.h");
        wrong.name = "nsCOMPtr".into();
        wrong.kind = "function_definition".into();
        wrong.source = "void nsCOMPtr();".into();

        let mut right = test_block("base/nsCOMPtr.h");
        right.id = Id::new("base/nsCOMPtr.h", "function_definition", "nsright12hash456");
        right.name = "nsCOMPtr".into();
        right.kind = "function_definition".into();
        right.source = "void nsCOMPtr() {}".into();

        let best = pick_best_homonym_with_in_degree([&wrong, &right], |b| {
            if b.file.to_string_lossy().contains("Owning") {
                10_000
            } else {
                1
            }
        })
        .unwrap();
        assert!(
            best.file.to_string_lossy().contains("nsCOMPtr.h"),
            "got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_basename_hard_rule_beats_other_header_fn() {
        // Gecko-class: free fn nsCOMPtr in OwningNonNull.h must not ★ over nsCOMPtr.h.
        let mut wrong = test_block("xpcom/base/OwningNonNull.h");
        wrong.name = "nsCOMPtr".into();
        wrong.kind = "function_definition".into();
        wrong.lang = "c".into();
        wrong.source = "nsCOMPtr() { return nullptr; }".into();
        wrong.start_line = 192;
        wrong.end_line = 193;
        wrong.score = 9_999_999.0;

        let mut right = test_block("xpcom/base/nsCOMPtr.h");
        right.id = Id::new("xpcom/base/nsCOMPtr.h", "function_definition", "rightnscomptr12");
        right.name = "nsCOMPtr".into();
        right.kind = "function_definition".into();
        right.lang = "c".into();
        right.source = "nsCOMPtr() {\n  // primary door\n}\n".into();
        right.start_line = 384;
        right.end_line = 390;
        right.score = 1.0;

        let best = pick_best_homonym([&wrong, &right]).unwrap();
        assert!(
            best.file.to_string_lossy().contains("nsCOMPtr.h"),
            "basename hard rule failed: got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_drops_virtual_noise_when_product_exists() {
        let mut virt = test_block("t/virtual/source.cpp");
        virt.name = "nsISupports".into();
        virt.kind = "class_specifier".into();
        virt.source = "class nsISupports { virtual void x(); };".into();
        virt.start_line = 132;
        virt.end_line = 137;

        let mut product = test_block("xpcom/base/nsISupports.h");
        product.id = Id::new("xpcom/base/nsISupports.h", "class_specifier", "nsihash12345678");
        product.name = "nsISupports".into();
        product.kind = "class_specifier".into();
        product.source = "class nsISupports {\n public:\n  virtual nsrefcnt AddRef() = 0;\n};\n".into();
        product.start_line = 10;
        product.end_line = 40;

        assert!(is_testish_seed_block(&virt));
        let best = pick_best_homonym([&virt, &product]).unwrap();
        assert!(
            best.file.to_string_lossy().contains("nsISupports.h"),
            "got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_filename_match_beats_unrelated_path() {
        let mut other = test_block("aten/src/ATen/NestedTensorImpl.h");
        other.name = "TensorImpl".into();
        other.kind = "struct_specifier".into();
        other.source = "struct TensorImpl { int x; };".into();

        let mut primary = test_block("c10/core/TensorImpl.h");
        primary.id = Id::new("c10/core/TensorImpl.h", "struct_specifier", "primhash123456");
        primary.name = "TensorImpl".into();
        primary.kind = "struct_specifier".into();
        primary.source = "struct TensorImpl { int y; };".into();

        let best = pick_best_homonym([&other, &primary]).unwrap();
        assert_eq!(
            best.file.file_name().and_then(|s| s.to_str()),
            Some("TensorImpl.h")
        );
    }

    #[test]
    fn filename_match_demotes_cousin_nested_prefix() {
        let mut nested = test_block("aten/src/ATen/NestedTensorImpl.h");
        nested.name = "TensorImpl".into();
        assert!(filename_matches_symbol(&nested) < 0);
        let mut primary = test_block("c10/core/TensorImpl.h");
        primary.name = "TensorImpl".into();
        assert_eq!(filename_matches_symbol(&primary), 120);
    }

    #[test]
    fn pick_best_homonym_prefers_function_over_call_and_mod() {
        let mut def = test_block("src/gnn/projection.rs");
        def.name = "load_weights".into();
        def.kind = "function_item".into();
        def.score = 0.1;
        def.source = "pub fn load_weights(project_root: &str) -> Vec<f32> {\n    vec![]\n}".into();
        def.start_line = 154;
        def.end_line = 170;

        let mut call = test_block("src/gnn/projection.rs");
        call.id = Id::new("src/gnn/projection.rs", "call_expression", "callhash123456");
        call.name = "load_weights".into();
        call.kind = "call_expression".into();
        call.score = 99.0;
        call.source = "load_weights(\".\")".into();
        call.start_line = 407;
        call.end_line = 407;

        let mut mod_shell = test_block("src/lib.rs");
        mod_shell.id = Id::new("src/lib.rs", "mod_item", "modhash1234567");
        mod_shell.name = "load_weights".into();
        mod_shell.kind = "mod_item".into();
        mod_shell.score = 50.0;
        mod_shell.source = "mod load_weights;".into();

        let best = pick_best_homonym([&call, &mod_shell, &def]).unwrap();
        assert_eq!(best.kind, "function_item");
        assert!(best.source.contains("pub fn load_weights"));
    }

    #[test]
    fn pick_best_homonym_prefers_python_def_over_call() {
        let mut def = test_block("pkg/api.py");
        def.name = "connect".into();
        def.kind = "function_definition".into();
        def.score = 1.0;
        def.source = "def connect(url):\n    return Client(url)\n".into();

        let mut call = test_block("pkg/api.py");
        call.id = Id::new("pkg/api.py", "call", "callhash12345678");
        call.name = "connect".into();
        call.kind = "call".into();
        call.score = 50.0;
        call.source = "connect(url)".into();

        let best = pick_best_homonym([&call, &def]).unwrap();
        assert_eq!(best.kind, "function_definition");
    }

    #[test]
    fn pick_best_homonym_prefers_prod_over_test_file() {
        let mut prod = test_block("src/walk.rs");
        prod.name = "scan".into();
        prod.kind = "function_item".into();
        prod.score = 1.0;
        prod.source = "pub fn scan() {}".into();

        let mut testf = test_block("src/walk_test.rs");
        testf.id = Id::new("src/walk_test.rs", "function_item", "testhash9999999");
        testf.name = "scan".into();
        testf.kind = "function_item".into();
        testf.score = 100.0;
        testf.source = "fn scan() { assert!(true) }".into();

        let best = pick_best_homonym([&testf, &prod]).unwrap();
        assert!(best.file.to_string_lossy().contains("walk.rs"));
        assert!(!best.file.to_string_lossy().contains("_test"));
    }

    #[test]
    fn pick_best_homonym_prefers_c_def_over_header_prototype() {
        let mut def = test_block("server.c");
        def.name = "server_start".into();
        def.kind = "function_definition".into();
        def.lang = "cpp".into();
        def.source = "int server_start(...) {\n  return 0;\n}".into();

        let mut decl = test_block("tmux.h");
        decl.id = Id::new("tmux.h", "function_declaration", "hdrhash11111111");
        decl.name = "server_start".into();
        decl.kind = "function_declaration".into();
        decl.lang = "cpp".into();
        decl.source = "int server_start(struct tmuxproc *, uint64_t, ...);".into();

        let best = pick_best_homonym([&decl, &def]).unwrap();
        assert_eq!(best.kind, "function_definition");
        assert!(best.file.to_string_lossy().ends_with("server.c"));
    }

    #[test]
    fn pick_best_homonym_prefers_package_root_over_binding() {
        // gin.Default (package API) over binding.Default
        let mut root_api = test_block("gin.go");
        root_api.name = "Default".into();
        root_api.kind = "function_declaration".into();
        root_api.score = 1.0;
        root_api.source = "func Default() *Engine { return New() }".into();

        let mut nested = test_block("binding/binding.go");
        nested.id = Id::new("binding/binding.go", "function_declaration", "bindhash11111111");
        nested.name = "Default".into();
        nested.kind = "function_declaration".into();
        nested.score = 50.0;
        nested.source = "func Default(method, contentType string) Binding { return Form }".into();

        let best = pick_best_homonym([&nested, &root_api]).unwrap();
        assert!(
            best.file.to_string_lossy().ends_with("gin.go"),
            "got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_prefers_torch_nn_module_over_csrc() {
        // Mega-homonym: public Python nn.Module beats dense C++ API header.
        let mut py = test_block("torch/nn/modules/module.py");
        py.name = "Module".into();
        py.kind = "class_definition".into();
        py.lang = "python".into();
        py.score = 10.0;
        py.source = "class Module:\n    def forward(self, x):\n        return x\n".into();
        py.start_line = 30;
        py.end_line = 200;

        let mut cxx = test_block("torch/csrc/api/include/torch/nn/module.h");
        cxx.id = Id::new(
            "torch/csrc/api/include/torch/nn/module.h",
            "class_specifier",
            "cxxmodhash123456",
        );
        cxx.name = "Module".into();
        cxx.kind = "class_specifier".into();
        cxx.lang = "c".into();
        cxx.score = 800.0; // celebrity degree must not steal seed
        cxx.source = "class Module {\n public:\n  void register_module();\n};\n".repeat(40);
        cxx.start_line = 63;
        cxx.end_line = 629;

        let best = pick_best_homonym([&cxx, &py]).unwrap();
        assert!(
            best.file.to_string_lossy().ends_with("module.py"),
            "expected torch/nn Module, got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_prefers_public_tensor_over_tensorexpr() {
        let mut public = test_block("torch/_tensor.py");
        public.name = "Tensor".into();
        public.kind = "class_definition".into();
        public.lang = "python".into();
        public.source = "class Tensor:\n    def add(self, other):\n        pass\n".into();
        public.start_line = 1;
        public.end_line = 80;

        let mut expr = test_block("torch/csrc/jit/tensorexpr/tensor.h");
        expr.id = Id::new(
            "torch/csrc/jit/tensorexpr/tensor.h",
            "class_specifier",
            "texprhash1234567",
        );
        expr.name = "Tensor".into();
        expr.kind = "class_specifier".into();
        expr.lang = "c".into();
        expr.score = 999.0;
        expr.source = "class Tensor { int dims; };\n".repeat(20);
        expr.start_line = 13;
        expr.end_line = 200;

        let best = pick_best_homonym([&expr, &public]).unwrap();
        assert!(
            best.file.to_string_lossy().contains("_tensor.py"),
            "got {:?}",
            best.file
        );
    }

    #[test]
    fn pick_best_homonym_keeps_csrc_when_only_impl_twin() {
        // No public spine twin → impl tree remains selectable (Engine / TensorImpl class).
        let mut only = test_block("torch/csrc/autograd/engine.h");
        only.name = "Engine".into();
        only.kind = "class_specifier".into();
        only.source = "struct Engine { void execute(); };".into();
        let best = pick_best_homonym([&only]).unwrap();
        assert!(best.file.to_string_lossy().contains("engine.h"));
    }

    #[test]
    fn is_trace_noise_name_drops_generics_keeps_apis() {
        assert!(is_trace_noise_name("next"));
        assert!(is_trace_noise_name("numel"));
        assert!(is_trace_noise_name("Box"));
        assert!(!is_trace_noise_name("register_module"));
        assert!(!is_trace_noise_name("load_state_dict"));
        assert!(!is_trace_noise_name("DistributedDataParallel"));
        assert_eq!(trace_name_weak_penalty("insert"), 40);
        assert_eq!(trace_name_weak_penalty("register_module"), 0);
    }

    #[test]
    fn path_has_ns_token_segment_exact() {
        use std::path::Path;
        assert!(path_has_ns_token(Path::new("foo/mozilla/bar.h"), "mozilla"));
        assert!(path_has_ns_token(Path::new("a/b/c.h"), "a::b")); // last token `b`
        assert!(!path_has_ns_token(Path::new("xpcom/threads/Mutex.h"), "mozilla"));
        assert!(!path_has_ns_token(Path::new("notmozilla/x.h"), "mozilla"));
    }

    #[test]
    fn basename_tied_prefers_higher_in_degree_over_src_path() {
        // Both Type.h basename; src/ has application_path_priority 150, xpcom/ does not.
        // Gravity (in-degree) must win among basename ties — portable, not mozilla-specific.
        let mut xpcom = test_block("xpcom/threads/Mutex.h");
        xpcom.name = "Mutex".into();
        xpcom.kind = "class_specifier".into();
        xpcom.source = "class Mutex { void Lock(); };".into();
        xpcom.start_line = 1;
        xpcom.end_line = 50;

        let mut js = test_block("src/threading/Mutex.h");
        js.name = "Mutex".into();
        js.kind = "class_specifier".into();
        js.source = "class Mutex { void lock(); };".into();
        js.start_line = 1;
        js.end_line = 40;

        let deg = |b: &BlockInfo| -> usize {
            if b.file.to_string_lossy().contains("xpcom") {
                500
            } else {
                12
            }
        };
        let best = pick_best_homonym_with_in_degree([&js, &xpcom], deg).unwrap();
        assert!(
            best.file.to_string_lossy().contains("xpcom"),
            "gravity should beat src/ path priority, got {:?}",
            best.file
        );
    }

    #[test]
    fn seed_qualified_prefers_namespace_source_over_higher_degree_twin() {
        // Trust sniper: mozilla::Mutex must not star-steal to js Mutex.h via bare degree.
        // xpcom path has no /mozilla/ segment; evidence is `namespace mozilla` + include guard.
        let mut xpcom = test_block("xpcom/threads/Mutex.h");
        xpcom.name = "Mutex".into();
        xpcom.kind = "class_specifier".into();
        xpcom.source = "\
#ifndef mozilla_Mutex_h\n#define mozilla_Mutex_h\n\
namespace mozilla {\nclass Mutex { void Lock(); };\n}\n#endif\n"
            .into();
        xpcom.start_line = 10;
        xpcom.end_line = 80;

        let mut js = test_block("js/src/threading/Mutex.h");
        js.name = "Mutex".into();
        js.kind = "class_specifier".into();
        js.source = "\
namespace js {\nclass Mutex { void lock(); };\n}\n"
            .into();
        js.start_line = 10;
        js.end_line = 60;

        let mut noise = test_block("third_party/parking_lot/mutex.rs");
        noise.name = "Mutex".into();
        noise.kind = "struct_item".into();
        noise.source = "struct Mutex;".into();

        let scoped = [&js, &xpcom, &noise];
        let g = code_graph::CodeGraph::new();
        // Even if JS has higher CALL in-degree on a real graph, qualification wins.
        let best = seed_qualified_symbol(&g, &scoped, "mozilla::Mutex").expect("seed");
        assert!(
            best.file.to_string_lossy().contains("xpcom"),
            "mozilla::Mutex must seed xpcom Mutex.h, got {:?}",
            best.file
        );
    }

    #[test]
    fn seed_qualified_rejects_unrelated_namespace_twin() {
        let mut js = test_block("js/src/threading/Mutex.h");
        js.name = "Mutex".into();
        js.kind = "class_specifier".into();
        js.source = "namespace js { class Mutex {}; }\n".into();
        let g = code_graph::CodeGraph::new();
        // Only JS candidate — no mozilla evidence → honest None (no wrong ★).
        assert!(seed_qualified_symbol(&g, &[&js], "mozilla::Mutex").is_none());
    }

    #[test]
    fn seed_qualified_path_token_still_works() {
        let mut in_ns_dir = test_block("foo/mozilla/bar/Mutex.h");
        in_ns_dir.name = "Mutex".into();
        in_ns_dir.kind = "class_specifier".into();
        in_ns_dir.source = "class Mutex {};".into();
        let g = code_graph::CodeGraph::new();
        let best = seed_qualified_symbol(&g, &[&in_ns_dir], "mozilla::Mutex").expect("seed");
        assert!(best.file.to_string_lossy().contains("mozilla"));
    }

    #[test]
    fn qualification_evidence_include_guard() {
        let mut b = test_block("xpcom/threads/Mutex.h");
        b.name = "Mutex".into();
        b.source = "#ifndef mozilla_Mutex_h\n#define mozilla_Mutex_h\n".into();
        assert!(qualification_evidence(&b, "mozilla", "Mutex", None) >= 160);
        let mut js = test_block("js/src/threading/Mutex.h");
        js.name = "Mutex".into();
        js.source = "namespace js { class Mutex {}; }\n".into();
        assert_eq!(qualification_evidence(&js, "mozilla", "Mutex", None), 0);
    }

    // =========================================================================
    // Ladder snipers — Gem's four Structural Homonym Collision archetypes.
    // Paths mirror real test_repos layout; evidence is portable (no product spines).
    // =========================================================================

    /// #1 Mock/Test shadow: test suite in-degree can exceed prod — prod still wins.
    #[test]
    fn ladder_sniper_mock_test_shadow_client() {
        let mut prod = test_block("src/api/Client.h");
        prod.name = "Client".into();
        prod.kind = "class_specifier".into();
        prod.source = "class Client { void connect(); };\n".into();
        prod.start_line = 1;
        prod.end_line = 40;

        let mut mock = test_block("tests/mock/Client.h");
        mock.name = "Client".into();
        mock.kind = "class_specifier".into();
        mock.source = "class Client { void connect(); };\n".into();
        mock.start_line = 1;
        mock.end_line = 40;

        // Mock used more in tests → higher reverse degree.
        let deg = |b: &BlockInfo| -> usize {
            if b.file.to_string_lossy().contains("tests/") {
                900
            } else {
                40
            }
        };
        let best = pick_best_homonym_with_in_degree([&mock, &prod], deg).unwrap();
        assert!(
            best.file.to_string_lossy().contains("src/api"),
            "prod Client must beat test mock despite lower in-degree, got {:?}",
            best.file
        );
    }

    /// #2 Vendored/shadow dependency: product wrapper vs vendor twin.
    #[test]
    fn ladder_sniper_vendored_message_shadow() {
        let mut product = test_block("src/core/Message.h");
        product.name = "Message".into();
        product.kind = "class_specifier".into();
        product.source = "\
#ifndef my_project_Message_h\n\
namespace my_project { class Message {}; }\n#endif\n"
            .into();

        let mut vendor = test_block("vendor/protobuf/Message.h");
        vendor.name = "Message".into();
        vendor.kind = "class_specifier".into();
        vendor.source = "namespace google { namespace protobuf { class Message {}; }}\n".into();

        let g = code_graph::CodeGraph::new();
        let best =
            seed_qualified_symbol(&g, &[&vendor, &product], "my_project::Message").expect("seed");
        assert!(
            best.file.to_string_lossy().contains("src/core"),
            "my_project::Message must not seed vendor protobuf, got {:?}",
            best.file
        );
    }

    /// #3 Standard pattern clasher: many Builders — qualified tokens win.
    #[test]
    fn ladder_sniper_builder_pattern_qualified() {
        let mut req = test_block("crates/http/src/request.rs");
        req.name = "Builder".into();
        req.kind = "struct_item".into();
        req.source = "\
pub mod request {\n\
    pub struct Builder { }\n\
}\n\
// path: used as http::Request::Builder in docs\n\
// namespace-ish: module request under http crate\n"
            .into();
        // Simulate path tokens http + request for hierarchical filter.
        req.file = std::path::PathBuf::from("crates/http/src/request/builder.rs");
        req.source = "pub struct Builder {}\n".into();

        let mut resp = test_block("crates/http/src/response/builder.rs");
        resp.name = "Builder".into();
        resp.kind = "struct_item".into();
        resp.source = "pub struct Builder {}\n".into();

        let mut cfg = test_block("crates/config/src/builder.rs");
        cfg.name = "Builder".into();
        cfg.kind = "struct_item".into();
        cfg.source = "pub struct Builder {}\n".into();

        // Query path segment `request` as parent token via path (portable).
        let g = code_graph::CodeGraph::new();
        // Use parent path token: request::Builder filters to request/ tree.
        let best =
            seed_qualified_symbol(&g, &[&resp, &cfg, &req], "request::Builder").expect("seed");
        assert!(
            best.file.to_string_lossy().contains("request"),
            "request::Builder must not drift to response/config, got {:?}",
            best.file
        );
    }

    /// #4 Internal implementation detail (Gecko special): public API vs detail/ + foreign ns.
    #[test]
    fn ladder_sniper_detail_vs_public_and_foreign_ns() {
        let mut public = test_block("xpcom/threads/Mutex.h");
        public.name = "Mutex".into();
        public.kind = "class_specifier".into();
        public.source = "\
#ifndef mozilla_Mutex_h\n#define mozilla_Mutex_h\n\
namespace mozilla {\nclass Mutex { void Lock(); };\n}\n#endif\n"
            .into();

        let mut detail = test_block("xpcom/threads/detail/Mutex.h");
        detail.name = "Mutex".into();
        detail.kind = "class_specifier".into();
        // Higher "impl" gravity: detail often has real machinery + high fan-in.
        detail.source = "\
namespace mozilla { namespace detail {\n\
class Mutex { void Lock(); void Unlock(); void Assert(); };\n\
}}\n"
            .into();

        let mut js = test_block("js/src/threading/Mutex.h");
        js.name = "Mutex".into();
        js.kind = "class_specifier".into();
        js.source = "namespace js { class Mutex { void lock(); }; }\n".into();

        let g = code_graph::CodeGraph::new();
        let best = seed_qualified_symbol(
            &g,
            &[&js, &detail, &public],
            "mozilla::Mutex",
        )
        .expect("seed");
        // Public header + include guard should beat detail and js.
        // detail also has namespace mozilla — evidence may tie; homonym ladder demotes /detail/.
        let path = best.file.to_string_lossy();
        assert!(
            path.contains("xpcom") && !path.contains("js/"),
            "mozilla::Mutex must stay in mozilla product tree, got {:?}",
            best.file
        );
        assert!(
            !path.contains("/detail/"),
            "prefer public Mutex.h over detail/, got {:?}",
            best.file
        );
    }

    /// Tauri-shaped: crates/tauri App vs plugin noise (basename + crates priority).
    #[test]
    fn ladder_sniper_tauri_shaped_app() {
        let mut app = test_block("crates/tauri/src/app.rs");
        app.name = "App".into();
        app.kind = "struct_item".into();
        app.source = "pub struct App<R> { }\n".into();
        app.start_line = 1;
        app.end_line = 80;

        let mut plugin = test_block("crates/tauri-plugin/src/build/mod.rs");
        plugin.name = "App".into();
        plugin.kind = "struct_item".into();
        plugin.source = "pub struct App;\n".into();

        let best = pick_best_homonym([&plugin, &app]).unwrap();
        // Both under crates/; body span / path depth / name entry should prefer real app.rs.
        assert!(
            best.file.to_string_lossy().contains("tauri/src/app"),
            "tauri App should prefer crates/tauri/src/app.rs, got {:?}",
            best.file
        );
    }

    /// Bevy-shaped: bevy_app::App vs incidental App in tests.
    #[test]
    fn ladder_sniper_bevy_shaped_app() {
        let mut core = test_block("crates/bevy_app/src/app.rs");
        core.name = "App".into();
        core.kind = "struct_item".into();
        core.source = "pub struct App { world: World }\n".into();
        core.start_line = 1;
        core.end_line = 200;

        let mut test_app = test_block("crates/bevy_asset/src/processor/tests.rs");
        test_app.name = "App".into();
        test_app.kind = "struct_item".into();
        test_app.source = "struct App;\n".into();

        let deg = |b: &BlockInfo| -> usize {
            if b.file.to_string_lossy().contains("tests") {
                500
            } else {
                80
            }
        };
        let best = pick_best_homonym_with_in_degree([&test_app, &core], deg).unwrap();
        assert!(
            best.file.to_string_lossy().contains("bevy_app"),
            "bevy App product spine must beat test App, got {:?}",
            best.file
        );
    }

    /// Redis-shaped: createClient definition vs benchmark twin.
    #[test]
    fn ladder_sniper_redis_shaped_create_client() {
        let mut core = test_block("src/networking.c");
        core.name = "createClient".into();
        core.kind = "function_definition".into();
        core.source = "client *createClient(connection *conn) { return c; }\n".into();
        core.start_line = 121;
        core.end_line = 200;

        let mut bench = test_block("src/redis-benchmark.c");
        bench.name = "createClient".into();
        bench.kind = "function_definition".into();
        bench.source = "static client createClient(char *cmd, size_t len, client from, int thread_id) {}\n".into();
        bench.start_line = 625;
        bench.end_line = 700;

        let best = pick_best_homonym([&bench, &core]).unwrap();
        // benchmark path demotion / non-static product def preference
        let path = best.file.to_string_lossy();
        assert!(
            path.contains("networking"),
            "createClient should prefer networking.c over redis-benchmark.c, got {:?}",
            best.file
        );
    }

    #[test]
    fn seed_role_tier_polyglot_kinds() {
        assert_eq!(seed_role_tier("struct_item"), 100);
        assert_eq!(seed_role_tier("class_definition"), 100);
        assert_eq!(seed_role_tier("interface_declaration"), 100);
        assert_eq!(seed_role_tier("type_spec"), 100);
        assert_eq!(seed_role_tier("function_item"), 90);
        assert_eq!(seed_role_tier("method_declaration"), 90);
        assert_eq!(seed_role_tier("mod_item"), 30);
        assert_eq!(seed_role_tier("call_expression"), 10);
        assert_eq!(seed_role_tier("if_expression"), 0);
    }

    #[test]
    fn is_module_shell_detects_rust_mod_semicolon() {
        let mut m = test_block("src/main.rs");
        m.name = "walk".into();
        m.kind = "mod_item".into();
        m.source = "mod walk;".into();
        assert!(is_module_shell(&m));

        let mut fn_b = test_block("src/walk.rs");
        fn_b.name = "scan".into();
        fn_b.kind = "function_item".into();
        fn_b.source = "pub fn scan() {}".into();
        assert!(!is_module_shell(&fn_b));
    }

    #[test]
    fn module_file_path_hints_include_rs_and_mod_rs() {
        let mut m = test_block("/repo/src/main.rs");
        m.name = "walk".into();
        let hints = module_file_path_hints(&m);
        assert!(hints.iter().any(|p| p.ends_with("walk.rs")));
        assert!(hints.iter().any(|p| p.ends_with("walk/mod.rs")));
    }

    #[test]
    fn pick_module_interior_prefers_file_stem_and_definition() {
        let mut shell = test_block("/repo/src/main.rs");
        shell.name = "walk".into();
        shell.kind = "mod_item".into();
        shell.source = "mod walk;".into();

        let mut helper = test_block("/repo/src/walk.rs");
        helper.id = Id::new("/repo/src/walk.rs", "function_item", "helphash123456");
        helper.name = "helper".into();
        helper.kind = "function_item".into();
        helper.score = 1.0;
        helper.source = "fn helper() {}".into();

        let mut hub = test_block("/repo/src/walk.rs");
        hub.id = Id::new("/repo/src/walk.rs", "struct_item", "hubhash1234567");
        hub.name = "WorkerState".into();
        hub.kind = "struct_item".into();
        hub.score = 5.0;
        hub.is_highly_connected = true;
        hub.source = "pub struct WorkerState { x: u32 }".into();

        let ranked = rank_module_interior("walk", vec![&helper, &hub], false);
        assert_eq!(ranked[0].name, "WorkerState");
        assert_eq!(ranked[0].kind, "struct_item");
        assert!(ranked.len() >= 2);
    }

    #[test]
    fn resolve_module_shell_detailed_opens_walk_rs_with_top3() {
        let mut shell = test_block("/repo/src/main.rs");
        shell.id = Id::new("/repo/src/main.rs", "mod_item", "modhashwalk0001");
        shell.name = "walk".into();
        shell.kind = "mod_item".into();
        shell.source = "mod walk;".into();

        let mut interior = test_block("/repo/src/walk.rs");
        interior.id = Id::new("/repo/src/walk.rs", "struct_item", "structwalk0001");
        interior.name = "Batch".into();
        interior.kind = "struct_item".into();
        interior.score = 9.0;
        interior.source = "pub struct Batch {}".into();

        let mut other = test_block("/repo/src/walk.rs");
        other.id = Id::new("/repo/src/walk.rs", "function_item", "fnwalk00000001");
        other.name = "spawn".into();
        other.kind = "function_item".into();
        other.score = 2.0;
        other.source = "pub fn spawn() {}".into();

        let mut graph = CodeGraph::default();
        graph.nodes.insert(shell.id.clone(), shell.clone());
        graph.nodes.insert(interior.id.clone(), interior.clone());
        graph.nodes.insert(other.id.clone(), other.clone());

        let shell_ref = graph.nodes.get(&shell.id).unwrap();
        let int_ref = graph.nodes.get(&interior.id).unwrap();
        let other_ref = graph.nodes.get(&other.id).unwrap();
        let scoped = vec![shell_ref, int_ref, other_ref];

        let res =
            resolve_module_shell_detailed(&graph, shell_ref, &scoped, "walk", false).unwrap();
        assert_eq!(res.from_mod, "walk");
        assert_eq!(res.seed.name, "Batch");
        assert!(res.top_candidates.len() >= 2);
        let dtos = module_interior_candidate_dtos("walk", &res.top_candidates, false, 3);
        assert_eq!(dtos[0].name, "Batch");
    }

    #[test]
    fn noise_filter_matches_testutil_and_testing_segments() {
        let cfg = NoiseFilterConfig::default();
        let root = Path::new("/repo");
        let b = test_block("/repo/web/api/v1/testutil/helpers.go");
        assert!(is_noise(&b, root, &cfg));
    }

    #[test]
    fn noise_filter_does_not_treat_test_repos_folder_as_noise() {
        let cfg = NoiseFilterConfig::default();
        // Project root is the checkout itself
        let root = Path::new("/projects/test_repos/fd");
        let app = test_block("/projects/test_repos/fd/src/main.rs");
        assert!(
            !is_noise(&app, root, &cfg),
            "real code under test_repos must not be noise"
        );
        // Even when relative path still contains the test_repos segment
        let root2 = Path::new("/projects");
        let app2 = test_block("/projects/test_repos/bat/src/lib.rs");
        assert!(
            !is_noise(&app2, root2, &cfg),
            "segment test_repos must not match test_* prefix"
        );
    }

    #[test]
    fn noise_filter_still_flags_real_test_dirs_and_files() {
        let cfg = NoiseFilterConfig::default();
        let root = Path::new("/repo");
        assert!(is_noise(
            &test_block("/repo/src/tests/helpers.rs"),
            root,
            &cfg
        ));
        assert!(is_noise(&test_block("/repo/src/test/foo.rs"), root, &cfg));
        assert!(is_noise(
            &test_block("/repo/src/foo_test.rs"),
            root,
            &cfg
        ));
        assert!(is_noise(
            &test_block("/repo/pkg/bar_test.go"),
            root,
            &cfg
        ));
        assert!(is_noise(
            &test_block("/repo/src/test_parser.rs"),
            root,
            &cfg
        ));
    }

    #[test]
    fn application_path_priority_test_repos_not_peripheral() {
        let under_eval = application_path_priority("test_repos/fd/src/main.rs");
        let under_tests = application_path_priority("tests/unit/main.rs");
        assert!(
            under_eval > under_tests,
            "test_repos app code ({under_eval}) should outrank tests/ ({under_tests})"
        );
    }

    #[test]
    fn is_vendored_segment_exact_covers_in_package_copies() {
        use std::path::PathBuf;
        assert!(is_vendored(&PathBuf::from("typer/_click/core.py")));
        assert!(is_vendored(&PathBuf::from("torch/_vendor/packaging/__init__.py")));
        assert!(is_vendored(&PathBuf::from("third_party/gloo/foo.cpp")));
        assert!(is_vendored(&PathBuf::from("vendor/foo.go")));
        assert!(is_vendored(&PathBuf::from("pkg/vendored/lib.rs")));
        assert!(is_vendored(&PathBuf::from("crates/deps/util.rs")));
        assert!(is_vendored(&PathBuf::from("next/ext/bar.ts")));
        // Substring traps must not false-positive.
        assert!(!is_vendored(&PathBuf::from("my_vendor_tool/main.rs")));
        assert!(!is_vendored(&PathBuf::from("src/dependencies.py")));
        assert!(!is_vendored(&PathBuf::from("torch/_dynamo/eval_frame.py")));
        assert!(!is_vendored(&PathBuf::from("typer/main.py")));
    }

    #[test]
    fn noise_filter_flags_in_package_vendor_segments() {
        let cfg = NoiseFilterConfig::default();
        let root = Path::new("/repo");
        assert!(is_noise(
            &test_block("/repo/typer/_click/core.py"),
            root,
            &cfg
        ));
        assert!(is_noise(
            &test_block("/repo/torch/_vendor/packaging/__init__.py"),
            root,
            &cfg
        ));
        assert!(is_noise(
            &test_block("/repo/pkg/third_party/foo.rs"),
            root,
            &cfg
        ));
        assert!(
            !is_noise(&test_block("/repo/torch/_dynamo/eval_frame.py"), root, &cfg),
            "product private packages are not vendor noise"
        );
    }
}
