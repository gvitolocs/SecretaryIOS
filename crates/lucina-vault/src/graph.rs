//! The note graph and a repeatable layout. The same vault gives the same picture, on
//! every platform. Force layout with repulsion only between neighbouring grid cells, so
//! a few thousand notes stay fast on one CPU.
use crate::vault::{note_name, Vault};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub path: String,
    pub name: String,
    /// Links in and out, for the node's size.
    pub degree: u32,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Default)]
pub struct GraphOptions {
    /// Show only notes within `depth` links of this one (Obsidian's local graph).
    pub around: Option<String>,
    pub depth: u32,
    pub with_attachments: bool,
    pub with_orphans: bool,
    /// Only notes with this tag (or under it).
    pub tag: Option<String>,
}

impl Vault {
    pub fn graph(&self, o: &GraphOptions) -> Graph {
        let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
        for n in self.notes() {
            let targets = n
                .dest
                .iter()
                .chain(&n.frontmatter_dest)
                .flatten()
                .map(|d| self.resolver.path(*d as usize));
            for t in targets {
                if t == n.path {
                    continue;
                }
                let is_note = self.note(t).is_some();
                if is_note || o.with_attachments {
                    edges.insert((n.path.clone(), t.to_owned()));
                }
            }
        }
        let mut keep: BTreeSet<String> = match &o.tag {
            Some(t) => self
                .notes_with_tag(t)
                .into_iter()
                .map(|n| n.path.clone())
                .collect(),
            None => self.notes().map(|n| n.path.clone()).collect(),
        };
        if o.tag.is_none() && o.with_attachments {
            keep.extend(edges.iter().map(|(_, t)| t.clone()));
        }
        if let Some(c) = &o.around {
            let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
            for (a, b) in &edges {
                adj.entry(a).or_default().push(b);
                adj.entry(b).or_default().push(a);
            }
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut q = VecDeque::from([(c.as_str(), 0u32)]);
            seen.insert(c.clone());
            while let Some((p, d)) = q.pop_front() {
                if d >= o.depth {
                    continue;
                }
                for n in adj.get(p).into_iter().flatten() {
                    if seen.insert((*n).to_owned()) {
                        q.push_back((n, d + 1));
                    }
                }
            }
            keep = keep.intersection(&seen).cloned().collect();
            keep.insert(c.clone());
        }
        let edges: Vec<(String, String)> = edges
            .into_iter()
            .filter(|(a, b)| keep.contains(a) && keep.contains(b))
            .collect();
        if !o.with_orphans && o.around.is_none() {
            let linked: BTreeSet<&String> = edges.iter().flat_map(|(a, b)| [a, b]).collect();
            keep = keep
                .iter()
                .filter(|p| linked.contains(p))
                .cloned()
                .collect();
        }
        let index: BTreeMap<&str, u32> = keep
            .iter()
            .enumerate()
            .map(|(i, p)| (p.as_str(), i as u32))
            .collect();
        let mut degree = vec![0u32; keep.len()];
        let mut e = Vec::with_capacity(edges.len());
        for (a, b) in &edges {
            let (Some(&a), Some(&b)) = (index.get(a.as_str()), index.get(b.as_str())) else {
                continue;
            };
            degree[a as usize] += 1;
            degree[b as usize] += 1;
            e.push((a, b));
        }
        let points = layout(keep.len(), &e);
        Graph {
            nodes: keep
                .iter()
                .zip(points)
                .zip(degree)
                .map(|((p, (x, y)), degree)| Node {
                    path: p.clone(),
                    name: note_name(p).to_owned(),
                    degree,
                    x,
                    y,
                })
                .collect(),
            edges: e,
        }
    }
}

const DISTANCE: f32 = 60.0;

/// Starts on a sunflower spiral and settles with cooling. Pure arithmetic in a fixed
/// order, so it repeats exactly on one machine.
pub fn layout(n: usize, edges: &[(u32, u32)]) -> Vec<(f32, f32)> {
    let mut p: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let a = i as f32 * 2.399_963;
            let r = DISTANCE * 0.5 * ((i + 1) as f32).sqrt();
            (a.cos() * r, a.sin() * r)
        })
        .collect();
    if n < 2 {
        return p;
    }
    let rounds = if n <= 200 {
        120
    } else if n <= 2000 {
        70
    } else {
        40
    };
    let mut v = vec![(0.0f32, 0.0f32); n];
    let cell = DISTANCE * 1.5;
    for round in 0..rounds {
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::with_capacity(n);
        for (i, q) in p.iter().enumerate() {
            grid.entry(((q.0 / cell).floor() as i32, (q.1 / cell).floor() as i32))
                .or_default()
                .push(i as u32);
        }
        let mut f = vec![(0.0f32, 0.0f32); n];
        for i in 0..n {
            let q = p[i];
            let c = ((q.0 / cell).floor() as i32, (q.1 / cell).floor() as i32);
            let mut samples = 0;
            'near: for dy in -1..=1 {
                for dx in -1..=1 {
                    let Some(others) = grid.get(&(c.0 + dx, c.1 + dy)) else {
                        continue;
                    };
                    for &o in others {
                        if o as usize == i {
                            continue;
                        }
                        let d = (q.0 - p[o as usize].0, q.1 - p[o as usize].1);
                        let s = d.0 * d.0 + d.1 * d.1 + 1.0;
                        let strength = (DISTANCE * DISTANCE * 0.5 / s).min(8.0);
                        let len = s.sqrt();
                        f[i].0 += d.0 / len * strength;
                        f[i].1 += d.1 / len * strength;
                        samples += 1;
                        if samples == 48 {
                            break 'near;
                        }
                    }
                }
            }
            f[i].0 -= q.0 * 0.004;
            f[i].1 -= q.1 * 0.004;
        }
        for &(a, b) in edges {
            let (a, b) = (a as usize, b as usize);
            let d = (p[b].0 - p[a].0, p[b].1 - p[a].1);
            let len = (d.0 * d.0 + d.1 * d.1).sqrt().max(1.0);
            let s = ((len - DISTANCE) * 0.02).clamp(-10.0, 10.0);
            let pull = (d.0 / len * s, d.1 / len * s);
            f[a].0 += pull.0;
            f[a].1 += pull.1;
            f[b].0 -= pull.0;
            f[b].1 -= pull.1;
        }
        let cooling = 1.0 - 0.8 * round as f32 / rounds as f32;
        for i in 0..n {
            v[i].0 = ((v[i].0 * 0.7) + f[i].0).clamp(-16.0, 16.0);
            v[i].1 = ((v[i].1 * 0.7) + f[i].1).clamp(-16.0, 16.0);
            p[i].0 += v[i].0 * cooling;
            p[i].1 += v[i].1 * cooling;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_repeats_and_stays_finite() {
        let edges: Vec<(u32, u32)> = (0..299).map(|i| (i, i + 1)).collect();
        let a = layout(300, &edges);
        assert_eq!(a, layout(300, &edges));
        assert!(a.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
        assert_eq!(layout(0, &[]).len(), 0);
        assert_eq!(layout(1, &[]).len(), 1);
    }
    #[test]
    fn linked_nodes_end_closer_than_unlinked() {
        let p = layout(40, &[(0, 39)]);
        let d = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
        let sum: f32 = (1..39).map(|i| d(p[0], p[i])).sum();
        assert!(d(p[0], p[39]) < sum / 38.0 * 2.0);
    }
}
