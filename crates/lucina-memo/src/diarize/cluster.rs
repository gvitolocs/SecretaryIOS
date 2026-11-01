//! Average-linkage agglomerative clustering of voice embeddings by cosine distance
//! (nearest-neighbour chain, O(n²) time and memory), cut where clusters are farther apart
//! than a threshold.

/// Above this many embeddings, an even sample is clustered and the rest join the nearest
/// cluster (the distance matrix stays under 40 MB).
pub const MAX_POINTS: usize = 3000;

/// Cluster labels `0..k` for `points`, numbered by first appearance. Points whose average
/// cosine distance is at most `threshold` end up together.
pub fn agglomerative(points: &[Vec<f32>], threshold: f32) -> Vec<usize> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    if n > MAX_POINTS {
        let step = n as f64 / MAX_POINTS as f64;
        let picked: Vec<usize> = (0..MAX_POINTS)
            .map(|i| (i as f64 * step) as usize)
            .collect();
        let sample: Vec<Vec<f32>> = picked.iter().map(|&i| points[i].clone()).collect();
        let labels = agglomerative(&sample, threshold);
        let k = labels.iter().max().map_or(0, |m| m + 1);
        let centroids: Vec<Vec<f32>> = (0..k)
            .map(|c| {
                let members: Vec<&[f32]> = labels
                    .iter()
                    .zip(&sample)
                    .filter(|(l, _)| **l == c)
                    .map(|(_, p)| p.as_slice())
                    .collect();
                super::mean_direction(&members)
            })
            .collect();
        let all: Vec<usize> = points
            .iter()
            .map(|p| {
                (0..k)
                    .max_by(|a, b| {
                        super::cosine(p, &centroids[*a])
                            .total_cmp(&super::cosine(p, &centroids[*b]))
                    })
                    .unwrap_or(0)
            })
            .collect();
        return renumber(&all);
    }
    let unit: Vec<Vec<f32>> = points
        .iter()
        .map(|p| {
            let mut u = p.clone();
            super::normalize(&mut u);
            u
        })
        .collect();
    let mut d = vec![0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let dot: f32 = unit[i].iter().zip(&unit[j]).map(|(a, b)| a * b).sum();
            let v = 1.0 - dot;
            d[i * n + j] = v;
            d[j * n + i] = v;
        }
    }
    let mut size = vec![1usize; n];
    let mut active = vec![true; n];
    let mut left = n;
    let mut merges: Vec<(f32, usize, usize)> = Vec::with_capacity(n - 1);
    let mut chain: Vec<usize> = Vec::new();
    while left > 1 {
        if chain.is_empty() {
            chain.push(active.iter().position(|a| *a).unwrap_or(0));
        }
        loop {
            let a = chain[chain.len() - 1];
            let prev = (chain.len() >= 2).then(|| chain[chain.len() - 2]);
            // Nearest active neighbour; ties go to the previous chain element (needed for
            // termination of the chain).
            let mut best = usize::MAX;
            let mut best_d = f32::INFINITY;
            if let Some(p) = prev {
                best = p;
                best_d = d[a * n + p];
            }
            for x in 0..n {
                if x != a && active[x] && d[a * n + x] < best_d {
                    best = x;
                    best_d = d[a * n + x];
                }
            }
            if Some(best) == prev {
                chain.truncate(chain.len() - 2);
                let (keep, gone) = (a.min(best), a.max(best));
                merges.push((best_d, keep, gone));
                let (sa, sb) = (size[keep] as f32, size[gone] as f32);
                for x in 0..n {
                    if active[x] && x != keep && x != gone {
                        let v = (sa * d[keep * n + x] + sb * d[gone * n + x]) / (sa + sb);
                        d[keep * n + x] = v;
                        d[x * n + keep] = v;
                    }
                }
                size[keep] += size[gone];
                active[gone] = false;
                left -= 1;
                break;
            }
            chain.push(best);
        }
    }
    // Cut the dendrogram: apply every merge at or under the threshold (average linkage is
    // monotone, so these merges are closed under their sub-merges).
    merges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for (dist, a, b) in merges {
        if dist > threshold {
            break;
        }
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[rb.max(ra)] = ra.min(rb);
        }
    }
    let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
    renumber(&roots)
}

/// Relabels so the first point's cluster is 0, the next new one 1, and so on.
pub fn renumber(labels: &[usize]) -> Vec<usize> {
    let mut map = std::collections::HashMap::new();
    labels
        .iter()
        .map(|l| {
            let k = map.len();
            *map.entry(*l).or_insert(k)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn around(base: &[f32], jitter: f32, i: usize) -> Vec<f32> {
        base.iter()
            .enumerate()
            .map(|(k, v)| v + jitter * (((i * 7 + k * 13) % 11) as f32 / 11.0 - 0.5))
            .collect()
    }

    #[test]
    fn two_voices_and_a_third() {
        let a = [1.0, 0.0, 0.0, 0.2];
        let b = [0.0, 1.0, 0.1, 0.0];
        let c = [0.0, 0.0, 1.0, 1.0];
        let mut pts = Vec::new();
        for i in 0..20 {
            pts.push(around(&a, 0.2, i));
            pts.push(around(&b, 0.2, i));
            if i % 4 == 0 {
                pts.push(around(&c, 0.2, i));
            }
        }
        let l = agglomerative(&pts, 0.5);
        assert_eq!(l[0], 0);
        assert_eq!(l[1], 1);
        assert_eq!(l[2], 2);
        assert_eq!(*l.iter().max().unwrap(), 2);
        for (i, p) in pts.iter().enumerate() {
            let want = if super::super::cosine(p, &a) > 0.8 {
                0
            } else if super::super::cosine(p, &b) > 0.8 {
                1
            } else {
                2
            };
            assert_eq!(l[i], want, "point {i}");
        }
        // A threshold of 2 (the largest cosine distance) merges everything.
        assert!(agglomerative(&pts, 2.0).iter().all(|x| *x == 0));
        assert_eq!(agglomerative(&[], 0.5), Vec::<usize>::new());
        assert_eq!(agglomerative(&[vec![1.0]], 0.5), vec![0]);
    }

    #[test]
    fn many_points_are_sampled_then_assigned() {
        let a = [1.0, 0.0];
        let b = [0.0, 1.0];
        let pts: Vec<Vec<f32>> = (0..MAX_POINTS + 50)
            .map(|i| {
                if i % 2 == 0 {
                    around(&a, 0.1, i)
                } else {
                    around(&b, 0.1, i)
                }
            })
            .collect();
        let l = agglomerative(&pts, 0.5);
        assert!(l.iter().enumerate().all(|(i, x)| *x == i % 2));
    }
}
