//! Same-name peer reverse for Trace dossiers (repo-agnostic).
//!
//! Hard CALL reverse into ★ stays in `callers`. Callers of other defs that share
//! the seed name are labeled `relation=name_peer` so agents do not treat them as
//! execution parents of the preferred pin (gin/prometheus Default* class trap).

use crate::server::dto::CallerCallee;
use code_graph::{BlockInfo, CodeGraph, Id, ProjectPaths};

/// Max peer-caller rows in the Trace sample (compact + structured share this cap).
pub(crate) const PEER_CALLERS_SAMPLE_CAP: usize = 8;

/// Drop peer rows that share (name, file) with hop-1 hard CALL rows.
///
/// Prevents the product lie "name_peer = not CALL into ★" when the same identity
/// is already listed under `callers` (loc-fallback / twin-id / false bare CALL).
/// Returns how many peer rows were removed.
pub(crate) fn dedupe_peers_against_hard_callers(
    peer_callers: &mut Vec<CallerCallee>,
    hard_callers: &[CallerCallee],
) -> usize {
    fn file_key(f: &str) -> String {
        f.replace('\\', "/").to_ascii_lowercase()
    }
    fn base_key(f: &str) -> String {
        let n = file_key(f);
        n.rsplit('/').next().unwrap_or(&n).to_string()
    }
    let hard_full: std::collections::HashSet<(String, String)> = hard_callers
        .iter()
        .filter(|c| c.hop <= 1)
        .map(|c| (c.name.clone(), file_key(&c.file)))
        .collect();
    let hard_base: std::collections::HashSet<(String, String)> = hard_callers
        .iter()
        .filter(|c| c.hop <= 1)
        .map(|c| (c.name.clone(), base_key(&c.file)))
        .collect();
    let before = peer_callers.len();
    peer_callers.retain(|p| {
        let full = (p.name.clone(), file_key(&p.file));
        let base = (p.name.clone(), base_key(&p.file));
        !hard_full.contains(&full) && !hard_base.contains(&base)
    });
    before.saturating_sub(peer_callers.len())
}

/// Build labeled peer-caller rows from `(caller_id, peer_def_id)` pairs.
pub(crate) fn peer_callers_to_rows(
    graph: &CodeGraph,
    pairs: &[(Id, Id)],
    pp: &ProjectPaths,
    cap: usize,
) -> Vec<CallerCallee> {
    let mut rows = Vec::new();
    for (caller_id, peer_id) in pairs.iter().take(cap.saturating_mul(2)) {
        if rows.len() >= cap {
            break;
        }
        let Some(caller) = graph.get_block(caller_id.clone()) else {
            continue;
        };
        if crate::server::filters::is_trace_noise_name(&caller.name) {
            continue;
        }
        if crate::server::filters::is_testish_seed_block(caller) {
            continue;
        }
        let peer = graph.get_block(peer_id.clone());
        let mut cc = crate::server::filters::caller_callee_from_block(caller, pp);
        cc.hop = 1;
        cc.relation = Some("name_peer".into());
        cc.why = Some(peer_why(peer, &caller.name));
        rows.push(cc);
    }
    rows
}

fn peer_why(peer: Option<&BlockInfo>, _caller_name: &str) -> String {
    match peer {
        Some(p) => {
            let path = p.file.to_string_lossy().replace('\\', "/");
            let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            let short = if parts.len() <= 2 {
                path.clone()
            } else {
                parts[parts.len() - 2..].join("/")
            };
            format!(
                "calls same-name peer `{}` @ {} — not a CALL into the ★ pin",
                p.name, short
            )
        }
        None => "calls a same-name peer def — not a CALL into the ★ pin".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_why_without_peer_block_is_honest() {
        let w = peer_why(None, "New");
        assert!(w.contains("same-name peer"));
        assert!(w.contains("not a CALL"));
    }

    #[test]
    fn dedupe_drops_same_name_file_in_hard_and_peer() {
        let hard = vec![CallerCallee {
            name: "test".into(),
            file: "/proj/cranelift/native/src/lib.rs".into(),
            line: 188,
            hop: 1,
            lang: Some("rust".into()),
            cluster: None,
            relation: None,
            cite: None,
            why: None,
        }];
        let mut peers = vec![
            CallerCallee {
                name: "test".into(),
                file: "/proj/cranelift/native/src/lib.rs".into(),
                line: 188,
                hop: 1,
                lang: Some("rust".into()),
                cluster: None,
                relation: Some("name_peer".into()),
                cite: None,
                why: None,
            },
            CallerCallee {
                name: "other".into(),
                file: "/proj/other.rs".into(),
                line: 1,
                hop: 1,
                lang: Some("rust".into()),
                cluster: None,
                relation: Some("name_peer".into()),
                cite: None,
                why: None,
            },
        ];
        let n = dedupe_peers_against_hard_callers(&mut peers, &hard);
        assert_eq!(n, 1);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "other");
    }
}
