//! Go language support (parser + edges).
//! Facade mirroring the other language modules (function/method/type_spec + call edges + go.mod sniffer).

pub mod edges;
pub mod parser;

pub(crate) use edges::{build_call_edges, build_usage_edges, collect_call_edges, collect_usage_edges};
pub use parser::parse;

// Re-export ParseError for consistency
pub use super::super::parser::ParseError;

// Blacklist of standard Go built-ins, builtins from common pkgs (fmt, log, etc.)
// and common stdlib funcs that would cause thousands of false-positive cross-file
// edges if global_names fallback was used. Distinctive project funcs will still
// resolve via the global map.
pub(crate) const GENERIC_NAMES: &[&str] = &[
    "make",
    "new",
    "len",
    "cap",
    "append",
    "copy",
    "delete",
    "panic",
    "recover",
    "print",
    "println",
    "fmt.Print",
    "fmt.Println",
    "fmt.Printf",
    "fmt.Fprintf",
    "log.Print",
    "log.Println",
    "log.Printf",
    "log.Fatal",
    "log.Fatalln",
    "log.Fatalf",
    "errors.New",
    "fmt.Errorf",
    "context.Background",
    "context.TODO",
    "context.WithCancel",
    "context.WithTimeout",
    "http.Handle",
    "http.HandleFunc",
    "http.ListenAndServe",
    "http.ListenAndServeTLS",
    "json.Marshal",
    "json.Unmarshal",
    "xml.Marshal",
    "xml.Unmarshal",
    "ioutil.ReadFile",
    "ioutil.WriteFile", // legacy
    "os.Open",
    "os.Create",
    "os.ReadFile",
    "os.WriteFile",
    "time.Now",
    "time.Sleep",
    "time.After",
    "sync.Mutex",
    "sync.RWMutex",
    "sync.WaitGroup",
    "sync.Once",
    "testing.T",
    "testing.B",
];
