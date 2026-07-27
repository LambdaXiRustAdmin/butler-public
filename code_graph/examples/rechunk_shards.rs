//! Offline: split/merge shard parts for parallel hydrate.
//!
//! ```bash
//! cargo run -p code_graph --example rechunk_shards -- /path/to/repo
//! cargo run -p code_graph --example rechunk_shards -- --merge /path/to/repo
//! ```

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let merge = args.first().is_some_and(|a| a == "--merge");
    if merge {
        args.remove(0);
    }
    let root = args
        .first()
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let root = std::path::Path::new(&root);
    if merge {
        println!("Merge parts → monolith under {}/.butler/cache …", root.display());
        match code_graph::snooper::scanner::shards::merge_parts_to_monolith(root) {
            Ok((n_sym, n_edge)) => println!("Done. symbols={n_sym} edges={n_edge}"),
            Err(e) => {
                eprintln!("merge failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    println!("Rechunk shards under {}/.butler/cache …", root.display());
    match code_graph::snooper::scanner::shards::rechunk_monolith_shards(root) {
        Ok((n_sym, n_edge)) => {
            println!("Done. symbols_touched={n_sym} edges_touched={n_edge}");
        }
        Err(e) => {
            eprintln!("rechunk failed: {e}");
            std::process::exit(1);
        }
    }
}
