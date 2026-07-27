//! Declarative template for harvester runs (configurable options).
//! Controls output shape, incremental behavior, accuracy rules.
//! Loaded from JSON. Explicit, minimal.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Output {
    pub schema: String,
    pub format_version: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Incremental {
    pub batch_size: usize,
    /// Hard ceiling on batches (depth / cost cap). Always enforced.
    pub max_steps: usize,
    pub save_after_each: bool,
    pub load_previous_context: bool,
    /// Soft goal: stop early when criticals >= this (0 = disabled, run until max_steps).
    #[serde(default)]
    pub target_criticals: usize,
    /// Soft goal: stop early when rejections >= this.
    ///
    /// **`0` does not mean “no rejections wanted.”** When `target_rejections == 0` and
    /// `target_criticals > 0`, effective rejections **mirror** criticals
    /// (`effective_target_rejections()` → same number) so pos/neg stay ~balanced for Eve.
    /// Example: `target_criticals: 40, target_rejections: 0` ⇒ want ~40 of **each** pole.
    /// Set an explicit positive `target_rejections` only when you want an asymmetric floor.
    #[serde(default)]
    pub target_rejections: usize,
}

impl Incremental {
    /// Soft rejection floor used by goals/caps/batch mins.
    ///
    /// | `target_rejections` | `target_criticals` | effective rejections |
    /// |---------------------|--------------------|----------------------|
    /// | `N > 0`             | any                | `N`                  |
    /// | `0`                 | `C > 0`            | **`C` (mirror)**     |
    /// | `0`                 | `0`                | `0` (no soft goal)   |
    pub fn effective_target_rejections(&self) -> usize {
        if self.target_rejections > 0 {
            self.target_rejections
        } else {
            self.target_criticals
        }
    }

    /// True when soft gold floors are met (both poles if targets active).
    pub fn goals_met(&self, criticals: usize, rejections: usize) -> bool {
        if self.target_criticals == 0 && self.target_rejections == 0 {
            return false; // no soft goal — only max_steps
        }
        let need_c = self.target_criticals;
        let need_r = self.effective_target_rejections();
        let c_ok = need_c == 0 || criticals >= need_c;
        let r_ok = need_r == 0 || rejections >= need_r;
        c_ok && r_ok
    }

    /// Per-batch minimums given counts already in the fat.
    /// Once a pole has hit its target, stop *requiring* (and we cap adding) that pole
    /// so 15 crit / 41 rej can still grow to ~40/41 instead of 40/90.
    pub fn batch_polarity_mins(
        &self,
        criticals_so_far: usize,
        rejections_so_far: usize,
        default_min_crit: usize,
        default_min_rej: usize,
    ) -> (usize, usize) {
        let need_c = self.target_criticals;
        let need_r = self.effective_target_rejections();
        let min_c = if need_c > 0 && criticals_so_far >= need_c {
            0
        } else {
            default_min_crit
        };
        let min_r = if need_r > 0 && rejections_so_far >= need_r {
            0
        } else {
            default_min_rej
        };
        (min_c, min_r)
    }

    /// Whether we should drop new labels of this pole (already at/over target).
    pub fn cap_new_criticals(&self, criticals_so_far: usize) -> bool {
        self.target_criticals > 0 && criticals_so_far >= self.target_criticals
    }

    pub fn cap_new_rejections(&self, rejections_so_far: usize) -> bool {
        let need_r = self.effective_target_rejections();
        need_r > 0 && rejections_so_far >= need_r
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Accuracy {
    pub require_exploration_note: bool,
    pub require_reason_on_every_edge: bool,
    pub require_explicit_rejections: bool,
    pub min_hard_negatives_per_batch: usize,
    /// Fail-closed: each accepted batch needs at least this many criticals (default 1).
    #[serde(default = "default_min_criticals")]
    pub min_criticals_per_batch: usize,
    /// Reject stub notes like "selected from CodeGraph" (default true).
    #[serde(default = "default_true")]
    pub ban_stub_notes: bool,
    /// Every emitted node must be either critical or hard-negative (default true).
    #[serde(default = "default_true")]
    pub require_label_polarity: bool,
}

fn default_min_criticals() -> usize {
    1
}
fn default_true() -> bool {
    true
}

impl Default for Accuracy {
    fn default() -> Self {
        Self {
            require_exploration_note: true,
            require_reason_on_every_edge: false,
            require_explicit_rejections: true,
            min_hard_negatives_per_batch: 1,
            min_criticals_per_batch: 1,
            ban_stub_notes: true,
            require_label_polarity: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Focus {
    pub scope_paths: Vec<String>,
    #[serde(default)]
    pub ignore_paths: Vec<String>,
    /// Legacy; only used when frontier.strategy = priority/legacy.
    #[serde(default)]
    pub prefer_high_degree: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Frontier {
    /// neighborhood | random_walk | priority (legacy degree-ish)
    pub strategy: String,
    pub use_ast_distance: bool,
    /// Only for legacy priority strategy (capped mild bias).
    pub use_degree: bool,
    pub use_bm25: bool,
    /// Card size profile: `fast` (large snippets, more neighbors) | `slow` (compact for local CPU).
    #[serde(default = "default_card_profile")]
    pub card_profile: String,
    /// Optional overrides (0 = use profile default).
    #[serde(default)]
    pub max_neighbors: usize,
    #[serde(default)]
    pub max_snippet_chars: usize,
}

fn default_card_profile() -> String {
    "fast".into()
}

impl Frontier {
    pub fn card_budget(&self) -> crate::harvester::cards::CardBudget {
        let mut b = crate::harvester::cards::CardBudget::from_profile(&self.card_profile);
        if self.max_neighbors > 0 {
            b.max_neighbors = self.max_neighbors;
        }
        if self.max_snippet_chars > 0 {
            b.max_snippet_chars = self.max_snippet_chars;
        }
        b
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Llm {
    pub via: String,
    pub model: String,
    pub temperature: f32,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Polyglot {
    pub include_interconnect: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Template {
    pub name: String,
    pub query: String,
    pub repo: String,
    #[serde(default)]
    pub butler_export: Option<String>,
    pub output: Output,
    pub incremental: Incremental,
    pub accuracy: Accuracy,
    #[serde(default)]
    pub focus: Focus,
    pub frontier: Frontier,
    pub llm: Llm,
    #[serde(default)]
    pub polyglot: Polyglot,
}

impl Template {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read_to_string(path)?;
        let t: Self = serde_json::from_str(&data)?;
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::Incremental;

    #[test]
    fn goals_met_requires_both_poles_when_only_criticals_set() {
        let inc = Incremental {
            batch_size: 4,
            max_steps: 100,
            save_after_each: true,
            load_previous_context: true,
            target_criticals: 20,
            target_rejections: 0, // mirrors → 20
        };
        assert!(!inc.goals_met(20, 5));
        assert!(inc.goals_met(20, 20));
        assert!(inc.goals_met(25, 30));
    }

    #[test]
    fn no_soft_goals_never_met() {
        let inc = Incremental {
            batch_size: 4,
            max_steps: 10,
            save_after_each: true,
            load_previous_context: true,
            target_criticals: 0,
            target_rejections: 0,
        };
        assert!(!inc.goals_met(100, 100));
    }

    #[test]
    fn catch_up_criticals_when_rejections_already_over_target() {
        // Real fd-like: 15/41, want 40/40 — must not require more rejects.
        let inc = Incremental {
            batch_size: 4,
            max_steps: 80,
            save_after_each: true,
            load_previous_context: true,
            target_criticals: 40,
            target_rejections: 40,
        };
        assert!(!inc.goals_met(15, 41));
        let (min_c, min_r) = inc.batch_polarity_mins(15, 41, 1, 1);
        assert_eq!((min_c, min_r), (1, 0));
        assert!(inc.cap_new_rejections(41));
        assert!(!inc.cap_new_criticals(15));
        // After catch-up
        assert!(inc.goals_met(40, 41));
    }
}
