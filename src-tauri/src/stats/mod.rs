//! Agreement and recall statistics (§9). Code does the arithmetic (principle 5).

use serde::Serialize;

/// Cohen's kappa for two raters over the same items (any number of categories).
/// Returns `None` when there are no items. When expected agreement is 1
/// (both raters used one identical category throughout) kappa is defined as 1.
pub fn cohen_kappa(a: &[&str], b: &[&str]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let n = a.len() as f64;
    let mut cats: Vec<&str> = a.iter().chain(b.iter()).copied().collect();
    cats.sort_unstable();
    cats.dedup();
    let observed = a.iter().zip(b).filter(|(x, y)| x == y).count() as f64 / n;
    let expected: f64 = cats
        .iter()
        .map(|c| {
            let pa = a.iter().filter(|x| *x == c).count() as f64 / n;
            let pb = b.iter().filter(|x| *x == c).count() as f64 / n;
            pa * pb
        })
        .sum();
    if (1.0 - expected).abs() < 1e-12 {
        return Some(1.0);
    }
    Some((observed - expected) / (1.0 - expected))
}

/// Wilson score interval for a proportion `k/n` at 95%.
pub fn wilson(k: u64, n: u64) -> Option<(f64, f64)> {
    if n == 0 {
        return None;
    }
    let z = 1.959_963_984_540_054_f64;
    let n_f = n as f64;
    let p = k as f64 / n_f;
    let denom = 1.0 + z * z / n_f;
    let centre = (p + z * z / (2.0 * n_f)) / denom;
    let half = z * ((p * (1.0 - p) / n_f) + z * z / (4.0 * n_f * n_f)).sqrt() / denom;
    Some(((centre - half).max(0.0), (centre + half).min(1.0)))
}

/// One stratum of the Gate 2 sample.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stratum {
    pub name: String,
    /// Records in the whole population for this stratum.
    pub population: u64,
    /// Sampled records the person judged relevant (include or unsure).
    pub relevant: u64,
    /// Records the person judged in this stratum's sample.
    pub sampled: u64,
    /// True if the agents' decision for this stratum lets records through
    /// (included or sent onwards); false for the agent-excluded stratum.
    pub passed_by_agents: bool,
}

/// Estimated recall of the agents against the person, weighting each stratum
/// by its share of the population: estimated relevant records the agents let
/// through, over estimated relevant records overall.
pub fn stratified_recall(strata: &[Stratum]) -> Option<f64> {
    let est = |s: &Stratum| {
        if s.sampled == 0 {
            0.0
        } else {
            s.population as f64 * s.relevant as f64 / s.sampled as f64
        }
    };
    let total: f64 = strata.iter().map(est).sum();
    if total <= 0.0 {
        return None;
    }
    let kept: f64 = strata.iter().filter(|s| s.passed_by_agents).map(est).sum();
    Some(kept / total)
}

/// Small deterministic PRNG (xorshift64*) so bootstrap results can be reproduced from the logged seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform integer in 0..n.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }

    /// Fisher-Yates shuffle.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }
}

/// Percentile bootstrap 95% interval for the stratified recall: resamples each
/// stratum's judged sample with replacement, `reps` times.
pub fn recall_bootstrap(strata: &[Stratum], reps: usize, seed: u64) -> Option<(f64, f64)> {
    stratified_recall(strata)?;
    let mut rng = Rng::new(seed);
    let mut estimates = Vec::with_capacity(reps);
    for _ in 0..reps {
        let resampled: Vec<Stratum> = strata
            .iter()
            .map(|s| {
                let mut relevant = 0;
                for _ in 0..s.sampled {
                    if rng.below(s.sampled) < s.relevant {
                        relevant += 1;
                    }
                }
                Stratum {
                    relevant,
                    ..s.clone()
                }
            })
            .collect();
        if let Some(r) = stratified_recall(&resampled) {
            estimates.push(r);
        }
    }
    if estimates.is_empty() {
        return None;
    }
    estimates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f64| estimates[((estimates.len() as f64 - 1.0) * q).round() as usize];
    Some((at(0.025), at(0.975)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kappa_textbook_example() {
        // 50 items: yes/yes 20, yes/no 5, no/yes 10, no/no 15 -> po 0.7, pe 0.5, kappa 0.4.
        let mut a = Vec::new();
        let mut b = Vec::new();
        for (x, y, n) in [
            ("yes", "yes", 20),
            ("yes", "no", 5),
            ("no", "yes", 10),
            ("no", "no", 15),
        ] {
            for _ in 0..n {
                a.push(x);
                b.push(y);
            }
        }
        let k = cohen_kappa(&a, &b).unwrap();
        assert!((k - 0.4).abs() < 1e-12, "{k}");
        assert_eq!(cohen_kappa(&["a", "a"], &["a", "a"]), Some(1.0));
        assert_eq!(cohen_kappa(&[], &[]), None);
    }

    #[test]
    fn wilson_known_values() {
        // 81/263 -> (0.2553, 0.3662) per Wilson (1927) as tabulated in Newcombe (1998).
        let (lo, hi) = wilson(81, 263).unwrap();
        assert!(
            (lo - 0.2553).abs() < 1e-3 && (hi - 0.3662).abs() < 1e-3,
            "{lo} {hi}"
        );
        let (lo, hi) = wilson(0, 10).unwrap();
        assert_eq!(lo, 0.0);
        assert!((hi - 0.2775).abs() < 1e-3);
        assert_eq!(wilson(0, 0), None);
    }

    #[test]
    fn recall_weights_strata_by_population() {
        let strata = vec![
            Stratum {
                name: "include".into(),
                population: 200,
                relevant: 36,
                sampled: 40,
                passed_by_agents: true,
            },
            Stratum {
                name: "uncertain".into(),
                population: 50,
                relevant: 10,
                sampled: 20,
                passed_by_agents: true,
            },
            Stratum {
                name: "exclude".into(),
                population: 2000,
                relevant: 1,
                sampled: 40,
                passed_by_agents: false,
            },
        ];
        // kept = 200*0.9 + 50*0.5 = 205; missed = 2000*0.025 = 50; recall = 205/255.
        let r = stratified_recall(&strata).unwrap();
        assert!((r - 205.0 / 255.0).abs() < 1e-12);
        let (lo, hi) = recall_bootstrap(&strata, 2000, 42).unwrap();
        assert!(lo < r && r < hi, "{lo} {r} {hi}");
        // Same seed, same interval.
        assert_eq!(recall_bootstrap(&strata, 2000, 42), Some((lo, hi)));
    }

    #[test]
    fn perfect_recall_when_no_misses() {
        let strata = vec![
            Stratum {
                name: "include".into(),
                population: 10,
                relevant: 8,
                sampled: 10,
                passed_by_agents: true,
            },
            Stratum {
                name: "exclude".into(),
                population: 90,
                relevant: 0,
                sampled: 40,
                passed_by_agents: false,
            },
        ];
        assert_eq!(stratified_recall(&strata), Some(1.0));
        assert_eq!(recall_bootstrap(&strata, 200, 1), Some((1.0, 1.0)));
    }
}
