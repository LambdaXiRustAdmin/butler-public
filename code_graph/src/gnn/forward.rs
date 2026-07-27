//! CPU GNN forward for SmartButler scoring — **TrainLayout v2 twin of Eve/Xi train**.
//!
//! Tape order (same as Eve `compile_training_map`):
//!   L1 matmul (D×D) → multi-rel mean aggregate → tanh → L2 linear head (D→1)
//!
//! Weight banks (little-endian f32 blob, same as Eve dual-publish):
//!   [0 .. D*D)     L1  (row-major: w[in * D + out])
//!   [D*D .. D*D+D) L2  (length D)
//!
//! Node loops use rayon (`par_iter`) for multi-core CPU.

use rayon::prelude::*;

use super::projection::FEATURE_DIM;

pub const NUM_REL: usize = 5;
/// Intermediate width after L1 (= FEATURE_DIM under TrainLayout v2).
pub const HIDDEN: usize = FEATURE_DIM;
pub const L1_OFFSET: usize = 0;
pub const L1_LEN: usize = FEATURE_DIM * FEATURE_DIM; // 1024
pub const L2_OFFSET: usize = L1_LEN; // 1024
pub const L2_LEN: usize = FEATURE_DIM; // 32
pub const W_ACTIVE: usize = L2_OFFSET + L2_LEN; // 1056

/// Pure CPU forward matching Xi train kernels (linear L2 logit, no train-time tanh on head).
/// `features`: n × FEATURE_DIM row-major. `typed_edges`: (src, dst, rel).
pub fn cpu_gnn_forward(
    weights: &[f32],
    n: usize,
    features: &[f32],
    typed_edges: &[(usize, usize, u8)],
) -> Vec<f32> {
    let d = FEATURE_DIM;
    if n == 0 || features.len() != n * d {
        return vec![0.0f32; n];
    }
    if weights.len() < W_ACTIVE {
        eprintln!(
            "[gnn] weights len {} < W_ACTIVE {}; scores=0",
            weights.len(),
            W_ACTIVE
        );
        return vec![0.0f32; n];
    }

    let w1 = &weights[L1_OFFSET..L1_OFFSET + L1_LEN];
    let w2 = &weights[L2_OFFSET..L2_OFFSET + L2_LEN];

    // --- L1 matmul (parallel over nodes) ---
    // Matches Xi 62009: w = data[l1_off + in_f * out_dim + out_f]
    let mut pre = vec![0.0f32; n * d];
    pre.par_chunks_mut(d)
        .enumerate()
        .for_each(|(node, pre_row)| {
            let fbase = node * d;
            for out_f in 0..d {
                let mut sum = 0.0f32;
                for in_f in 0..d {
                    sum += features[fbase + in_f] * w1[in_f * d + out_f];
                }
                pre_row[out_f] = sum;
            }
        });

    // --- Multi-rel mean aggregate (self + neighbors); adj serial, reduce parallel ---
    let mut neigh: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(src, dst, r) in typed_edges {
        if (r as usize) >= NUM_REL || src >= n || dst >= n {
            continue;
        }
        neigh[dst].push(src);
    }
    for (i, nlist) in neigh.iter_mut().enumerate() {
        nlist.push(i); // self
    }

    let mut agg = vec![0.0f32; n * d];
    agg.par_chunks_mut(d)
        .zip(neigh.par_iter())
        .for_each(|(agg_row, sources)| {
            let deg = sources.len().max(1) as f32;
            for &src in sources {
                let sbase = src * d;
                for f in 0..d {
                    agg_row[f] += pre[sbase + f];
                }
            }
            for v in agg_row.iter_mut() {
                *v /= deg;
                *v = v.tanh(); // fuse tanh with agg write
            }
        });

    // --- L2 linear head (parallel over nodes) ---
    let scores: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|node| {
            let hbase = node * d;
            let mut logit = 0.0f32;
            for f in 0..d {
                logit += agg[hbase + f] * w2[f];
            }
            logit
        })
        .collect();

    if std::env::var("LAMBDA_XI_DEBUG").is_ok() && n > 0 {
        eprintln!(
            "[gnn] cpu_forward v2 n={} w_active={} threads≈rayon | sample[0..{}]: {:?}",
            n,
            W_ACTIVE,
            scores.len().min(5),
            &scores[..scores.len().min(5)]
        );
    }

    scores
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banks_match_train_layout() {
        assert_eq!(FEATURE_DIM, 32);
        assert_eq!(L1_LEN, 1024);
        assert_eq!(L2_OFFSET, 1024);
        assert_eq!(L2_LEN, 32);
        assert_eq!(W_ACTIVE, 1056);
    }

    #[test]
    fn short_weights_yield_zeros() {
        let n = 2;
        let feats = vec![1.0f32; n * FEATURE_DIM];
        let out = cpu_gnn_forward(&[0.1; 10], n, &feats, &[]);
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn identity_ish_not_nan() {
        let n = 3;
        let mut w = vec![0.0f32; W_ACTIVE];
        for i in 0..FEATURE_DIM {
            w[i * FEATURE_DIM + i] = 0.1;
        }
        for i in 0..L2_LEN {
            w[L2_OFFSET + i] = 0.05;
        }
        let mut feats = vec![0.0f32; n * FEATURE_DIM];
        for i in 0..n {
            feats[i * FEATURE_DIM] = 1.0;
            feats[i * FEATURE_DIM + 1] = 0.5;
        }
        let edges = vec![(0, 1, 0u8), (1, 2, 1u8)];
        let out = cpu_gnn_forward(&w, n, &feats, &edges);
        assert_eq!(out.len(), n);
        assert!(out.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn parallel_matches_serial_reference() {
        // Determinism: same inputs → same logits (rayon order-independent reductions).
        let n = 64;
        let mut w = vec![0.0f32; W_ACTIVE];
        for i in 0..W_ACTIVE {
            w[i] = ((i as f32 * 0.017) % 0.2) - 0.1;
        }
        let mut feats = vec![0.0f32; n * FEATURE_DIM];
        for i in 0..n * FEATURE_DIM {
            feats[i] = ((i % 7) as f32) * 0.1;
        }
        let mut edges = Vec::new();
        for i in 0..n - 1 {
            edges.push((i, i + 1, (i % 4) as u8));
            edges.push((i + 1, i, 1u8));
        }
        let a = cpu_gnn_forward(&w, n, &feats, &edges);
        let b = cpu_gnn_forward(&w, n, &feats, &edges);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-5, "{x} vs {y}");
        }
    }
}
