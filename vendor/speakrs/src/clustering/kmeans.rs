use ndarray::{Array2, ArrayView1, ArrayView2};

/// Lloyd iterations per restart.
const MAX_ITERS: usize = 100;
/// Seedings tried per call; the lowest-inertia run wins.
const RESTARTS: usize = 3;
/// Fixed RNG seed: the same input yields the same labels on every run.
const SEED: u64 = 42;

/// Cluster `points` into `k` groups by squared euclidean distance, returning one
/// label per row. `k` is capped at the row count. Seeding is k-means++.
pub fn cluster(points: &ArrayView2<f32>, k: usize) -> Vec<usize> {
    let n = points.nrows();
    if n == 0 {
        return Vec::new();
    }
    let k = k.clamp(1, n);
    if k == 1 {
        return vec![0; n];
    }

    let mut rng = Rng::new(SEED);
    let mut best: Option<(f64, Vec<usize>)> = None;
    for _ in 0..RESTARTS {
        let mut centroids = seed_centroids(points, k, &mut rng);
        let mut labels = vec![usize::MAX; n];
        for _ in 0..MAX_ITERS {
            let moved = assign_labels(points, &centroids.view(), &mut labels);
            recompute_centroids(points, &labels, &mut centroids);
            if !moved {
                break;
            }
        }
        let score = inertia(points, &centroids.view(), &labels);
        if best.as_ref().is_none_or(|(best_score, _)| score < *best_score) {
            best = Some((score, labels));
        }
    }
    best.map(|(_, labels)| labels).unwrap_or_else(|| vec![0; n])
}

/// k-means++ seeding: the first centre is uniform, each later one is drawn with
/// probability proportional to its squared distance from the nearest centre.
fn seed_centroids(points: &ArrayView2<f32>, k: usize, rng: &mut Rng) -> Array2<f32> {
    let n = points.nrows();
    let mut centroids = Array2::<f32>::zeros((k, points.ncols()));
    let first = rng.below(n);
    centroids.row_mut(0).assign(&points.row(first));

    let mut nearest: Vec<f64> = (0..n)
        .map(|row| squared_distance(&points.row(row), &centroids.row(0)))
        .collect();
    for centre in 1..k {
        let total: f64 = nearest.iter().sum();
        let pick = if total > 0.0 {
            let mut target = rng.unit() * total;
            let mut chosen = n - 1;
            for (row, distance) in nearest.iter().enumerate() {
                target -= *distance;
                if target <= 0.0 {
                    chosen = row;
                    break;
                }
            }
            chosen
        } else {
            rng.below(n)
        };
        centroids.row_mut(centre).assign(&points.row(pick));
        for (row, distance) in nearest.iter_mut().enumerate() {
            *distance = distance.min(squared_distance(&points.row(row), &centroids.row(centre)));
        }
    }
    centroids
}

/// Label every point with its nearest centroid. Returns whether any label moved.
fn assign_labels(
    points: &ArrayView2<f32>,
    centroids: &ArrayView2<f32>,
    labels: &mut [usize],
) -> bool {
    let mut moved = false;
    for (row, label) in labels.iter_mut().enumerate() {
        let mut best = (0usize, f64::INFINITY);
        for centre in 0..centroids.nrows() {
            let distance = squared_distance(&points.row(row), &centroids.row(centre));
            if distance < best.1 {
                best = (centre, distance);
            }
        }
        if *label != best.0 {
            *label = best.0;
            moved = true;
        }
    }
    moved
}

/// Move each centroid to the mean of its members; empty clusters keep their
/// previous position.
fn recompute_centroids(points: &ArrayView2<f32>, labels: &[usize], centroids: &mut Array2<f32>) {
    let mut sums = Array2::<f32>::zeros(centroids.raw_dim());
    let mut counts = vec![0usize; centroids.nrows()];
    for (row, &label) in labels.iter().enumerate() {
        sums.row_mut(label).scaled_add(1.0, &points.row(row));
        counts[label] += 1;
    }
    for (centre, count) in counts.iter().enumerate() {
        if *count > 0 {
            centroids
                .row_mut(centre)
                .assign(&sums.row(centre).mapv(|value| value / *count as f32));
        }
    }
}

fn inertia(points: &ArrayView2<f32>, centroids: &ArrayView2<f32>, labels: &[usize]) -> f64 {
    labels
        .iter()
        .enumerate()
        .map(|(row, &label)| squared_distance(&points.row(row), &centroids.row(label)))
        .sum()
}

fn squared_distance(lhs: &ArrayView1<f32>, rhs: &ArrayView1<f32>) -> f64 {
    lhs.iter()
        .zip(rhs.iter())
        .map(|(left, right)| {
            let delta = (*left - *right) as f64;
            delta * delta
        })
        .sum()
}

/// xorshift64*, so seeding is reproducible without a dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [0, `bound`).
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::array;

    #[test]
    fn separates_two_blobs() {
        let points = array![
            [10.0, 0.0],
            [10.1, 0.1],
            [9.9, -0.1],
            [-10.0, 0.0],
            [-10.1, 0.1],
            [-9.9, -0.1],
        ];

        let labels = cluster(&points.view(), 2);

        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[0], labels[2]);
        assert_eq!(labels[3], labels[4]);
        assert_eq!(labels[3], labels[5]);
        assert_ne!(labels[0], labels[3]);
    }

    #[test]
    fn splits_a_blob_when_more_clusters_are_asked_for() {
        // Three tight groups that AHC-style thresholds could read as two.
        let points = array![
            [0.0, 0.0],
            [0.1, 0.0],
            [5.0, 0.0],
            [5.1, 0.0],
            [5.05, 0.2],
            [10.0, 0.0],
        ];

        let labels = cluster(&points.view(), 3);

        let unique: std::collections::BTreeSet<_> = labels.iter().copied().collect();
        assert_eq!(unique.len(), 3);
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[2], labels[3]);
        assert_eq!(labels[2], labels[4]);
        assert_ne!(labels[0], labels[2]);
        assert_ne!(labels[2], labels[5]);
    }

    #[test]
    fn caps_k_at_the_row_count() {
        let points = array![[1.0, 0.0], [0.0, 1.0]];

        let labels = cluster(&points.view(), 9);

        assert_eq!(labels.len(), 2);
        assert_ne!(labels[0], labels[1]);
    }

    #[test]
    fn is_deterministic_across_calls() {
        let points = array![
            [1.0, 2.0],
            [1.2, 2.1],
            [8.0, 9.0],
            [8.3, 9.2],
            [4.0, 4.0],
            [4.2, 3.8],
        ];

        assert_eq!(cluster(&points.view(), 3), cluster(&points.view(), 3));
    }

    #[test]
    fn empty_input_yields_no_labels() {
        let points = Array2::<f32>::zeros((0, 4));

        assert!(cluster(&points.view(), 3).is_empty());
    }
}
