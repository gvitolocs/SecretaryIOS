//! Who is who: an incremental, DBSCAN-like grouping of face vectors into people.
//!
//! All face vectors are in memory (a [`VectorIndex`], 128 values each). Two faces are neighbours
//! when their cosine is at least [`MATCH`]. For a face that has no person yet:
//!
//! 1. if any neighbour has a person, it joins the person of the *closest* such neighbour (two
//!    people are never merged because a face sits between them);
//! 2. else, if it has at least [`MIN_FACES`]` − 1` neighbours to take along (a core point), a new
//!    person is made for it and those neighbours;
//! 3. else it stays unassigned and is tried again when something near it changes.
//!
//! Whenever a face gets a person, the unassigned faces around it are tried too, so a group grows
//! outwards the way a DBSCAN cluster does. Faces the owner assigned or unassigned by hand
//! (`manual`) are never changed and never taken along into a new person; an owner-assigned face
//! still counts as a person's member when another face looks for a person to join. Clustering only
//! ever adds faces to people, so a person the owner named is never merged into another.
//!
//! The graph knows nothing about the catalog: callers load it from the database, call
//! [`Graph::cluster`] with the faces that are new or changed, and store the returned assignments.

use std::collections::{HashMap, VecDeque};

use crate::index::VectorIndex;

/// Cosine at or above which two faces are neighbours.
pub const MATCH: f32 = 0.40;
/// Faces a new person needs, counting the one that starts it.
pub const MIN_FACES: usize = 3;
/// Neighbours looked at per face.
const NEIGHBOURS_MAX: usize = 256;

#[derive(Clone, Debug, PartialEq)]
struct Node {
    person: Option<String>,
    manual: bool,
}

/// A face that got a person in a clustering pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Assignment {
    pub face_id: String,
    pub person_id: String,
    /// The person was made in this pass (the first assignment to it).
    pub created: bool,
}

#[derive(Default)]
pub struct Graph {
    index: VectorIndex,
    nodes: HashMap<String, Node>,
}

enum Job {
    /// Look for a person for this unassigned face.
    Place(String),
    /// This face has a person now: try the unassigned faces around it.
    Expand(String),
}

impl Graph {
    pub fn new(dim: usize) -> Self {
        Self {
            index: VectorIndex::new(dim),
            nodes: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains(&self, face_id: &str) -> bool {
        self.nodes.contains_key(face_id)
    }

    /// Adds a face, or replaces its vector and state. `false` for a vector the index refuses.
    pub fn insert(&mut self, face_id: &str, v: &[f32], person: Option<&str>, manual: bool) -> bool {
        if !self.index.upsert(face_id, v) {
            return false;
        }
        self.nodes.insert(
            face_id.to_string(),
            Node {
                person: person.map(str::to_string),
                manual,
            },
        );
        true
    }

    /// Replaces a face's vector, keeping its person and state.
    pub fn update_vector(&mut self, face_id: &str, v: &[f32]) -> bool {
        self.nodes.contains_key(face_id) && self.index.upsert(face_id, v)
    }

    pub fn remove(&mut self, face_id: &str) -> bool {
        self.index.remove(face_id);
        self.nodes.remove(face_id).is_some()
    }

    pub fn person_of(&self, face_id: &str) -> Option<&str> {
        self.nodes.get(face_id)?.person.as_deref()
    }

    pub fn is_manual(&self, face_id: &str) -> bool {
        self.nodes.get(face_id).is_some_and(|n| n.manual)
    }

    /// The owner's choice: the face belongs to `person` (or to nobody) and clustering leaves it be.
    pub fn set_manual(&mut self, face_id: &str, person: Option<&str>) {
        if let Some(n) = self.nodes.get_mut(face_id) {
            n.person = person.map(str::to_string);
            n.manual = true;
        }
    }

    /// Moves every face of the `from` people to `to` (a merge), whatever their source.
    pub fn move_people(&mut self, from: &[String], to: &str) {
        for n in self.nodes.values_mut() {
            if n.person
                .as_deref()
                .is_some_and(|p| from.iter().any(|f| f == p))
            {
                n.person = Some(to.to_string());
            }
        }
    }

    /// Every face that clustering may place: no person and not set by hand.
    pub fn unassigned(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.person.is_none() && !n.manual)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }

    fn free(&self, id: &str) -> bool {
        self.nodes
            .get(id)
            .is_some_and(|n| n.person.is_none() && !n.manual)
    }

    fn neighbours(&self, id: &str) -> Vec<(String, Option<String>, bool)> {
        let Some(v) = self.index.get(id) else {
            return Vec::new();
        };
        self.index
            .search(v, NEIGHBOURS_MAX, MATCH, Some(id))
            .into_iter()
            .filter_map(|h| {
                let n = self.nodes.get(&h.id)?;
                Some((h.id, n.person.clone(), n.manual))
            })
            .collect()
    }

    /// One clustering pass from `seeds` (new or changed faces, or [`Graph::unassigned`] for a
    /// full pass). `new_person` makes the id of a person. Returns every assignment made, in order;
    /// the graph already reflects them.
    pub fn cluster(
        &mut self,
        seeds: &[String],
        new_person: &mut dyn FnMut() -> String,
    ) -> Vec<Assignment> {
        let mut out = Vec::new();
        let mut queue: VecDeque<Job> = seeds.iter().cloned().map(Job::Place).collect();
        while let Some(job) = queue.pop_front() {
            match job {
                Job::Place(f) => {
                    if !self.free(&f) {
                        continue;
                    }
                    let nb = self.neighbours(&f);
                    // The closest neighbour with a person (the list is best first).
                    if let Some(p) = nb.iter().find_map(|(_, p, _)| p.clone()) {
                        self.nodes.get_mut(&f).expect("free above").person = Some(p.clone());
                        out.push(Assignment {
                            face_id: f.clone(),
                            person_id: p,
                            created: false,
                        });
                        queue.push_back(Job::Expand(f));
                        continue;
                    }
                    // No neighbour has a person: the ones to take along are the free ones.
                    let take: Vec<String> = nb
                        .into_iter()
                        .filter(|(id, _, manual)| !manual && self.free(id))
                        .map(|(id, _, _)| id)
                        .collect();
                    if take.len() + 1 < MIN_FACES {
                        continue;
                    }
                    let p = new_person();
                    for (i, id) in std::iter::once(f).chain(take).enumerate() {
                        self.nodes.get_mut(&id).expect("free above").person = Some(p.clone());
                        out.push(Assignment {
                            face_id: id.clone(),
                            person_id: p.clone(),
                            created: i == 0,
                        });
                        queue.push_back(Job::Expand(id));
                    }
                }
                Job::Expand(m) => {
                    for (id, _, _) in self.neighbours(&m) {
                        if self.free(&id) {
                            queue.push_back(Job::Place(id));
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(deg: f32) -> Vec<f32> {
        let a = deg.to_radians();
        vec![a.cos(), a.sin()]
    }

    struct Ids(u32);
    impl Ids {
        fn make(&mut self) -> String {
            self.0 += 1;
            format!("p{}", self.0)
        }
    }

    fn graph(faces: &[(&str, f32, Option<&str>, bool)]) -> Graph {
        let mut g = Graph::new(2);
        for (id, deg, p, manual) in faces {
            assert!(g.insert(id, &unit(*deg), *p, *manual));
        }
        g
    }

    fn run(g: &mut Graph, seeds: &[&str], ids: &mut Ids) -> Vec<Assignment> {
        let seeds: Vec<String> = seeds.iter().map(|s| s.to_string()).collect();
        g.cluster(&seeds, &mut || ids.make())
    }

    #[test]
    fn a_face_joins_the_person_of_its_closest_neighbour_and_people_never_merge() {
        let mut ids = Ids(0);
        let mut g = graph(&[
            ("a", 0.0, Some("alice"), false),
            ("b", 60.0, Some("bob"), false), // cos(60°) = 0.5 to alice: close, but separate people
            ("x", 40.0, None, false),        // 20° from bob, 40° from alice
        ]);
        let out = run(&mut g, &["x"], &mut ids);
        assert_eq!(
            out,
            vec![Assignment {
                face_id: "x".into(),
                person_id: "bob".into(),
                created: false
            }]
        );
        assert_eq!(g.person_of("a"), Some("alice"));
        assert_eq!(g.person_of("b"), Some("bob"));
        assert_eq!(ids.0, 0, "no person was made");
    }

    #[test]
    fn three_close_faces_make_a_person_two_wait() {
        let mut ids = Ids(0);
        let mut g = graph(&[("a", 0.0, None, false), ("b", 10.0, None, false)]);
        assert!(run(&mut g, &["a", "b"], &mut ids).is_empty());
        assert_eq!(g.person_of("a"), None);
        // A third arrives: the deferred two are claimed with it.
        g.insert("c", &unit(20.0), None, false);
        let out = run(&mut g, &["c"], &mut ids);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|a| a.person_id == "p1"));
        assert_eq!(out.iter().filter(|a| a.created).count(), 1);
        for f in ["a", "b", "c"] {
            assert_eq!(g.person_of(f), Some("p1"));
        }
    }

    #[test]
    fn faces_far_apart_do_not_link() {
        let mut ids = Ids(0);
        // 90° apart: cosine 0, below the match.
        let mut g = graph(&[
            ("a1", 0.0, None, false),
            ("a2", 5.0, None, false),
            ("a3", 10.0, None, false),
            ("b1", 90.0, None, false),
            ("b2", 95.0, None, false),
            ("b3", 100.0, None, false),
            ("lone", 180.0, None, false),
        ]);
        let all = g.unassigned();
        let seeds: Vec<&str> = all.iter().map(String::as_str).collect();
        let out = run(&mut g, &seeds, &mut ids);
        assert_eq!(out.len(), 6);
        assert_eq!(g.person_of("a1"), g.person_of("a3"));
        assert_eq!(g.person_of("b1"), g.person_of("b3"));
        assert_ne!(g.person_of("a1"), g.person_of("b1"));
        assert_eq!(g.person_of("lone"), None);
    }

    #[test]
    fn a_deferred_face_is_claimed_when_a_person_forms_near_it() {
        let mut ids = Ids(0);
        // "lone" is 60° from the group's first face and has too few neighbours of its own.
        let mut g = graph(&[("lone", 60.0, None, false)]);
        assert!(run(&mut g, &["lone"], &mut ids).is_empty());
        for (id, deg) in [("g1", 0.0), ("g2", 8.0), ("g3", 16.0)] {
            g.insert(id, &unit(deg), None, false);
        }
        let out = run(&mut g, &["g1", "g2", "g3"], &mut ids);
        // The group forms; "lone" is within the match of g3 (44°: cos 0.72) and joins it.
        assert_eq!(g.person_of("g1"), Some("p1"));
        assert_eq!(g.person_of("lone"), Some("p1"), "{out:?}");
    }

    #[test]
    fn a_group_grows_outwards_through_its_members() {
        let mut ids = Ids(0);
        // A chain every 30° (cos 0.87): g1..g3 form a person; f1 (90°) is only near g3 (60°
        // away: cos 0.5), f2 (120°) only near f1.
        let mut g = graph(&[
            ("f1", 90.0, None, false),
            ("f2", 120.0, None, false),
            ("g1", 0.0, None, false),
            ("g2", 15.0, None, false),
            ("g3", 30.0, None, false),
        ]);
        let all = g.unassigned();
        let seeds: Vec<&str> = all.iter().map(String::as_str).collect();
        run(&mut g, &seeds, &mut ids);
        for f in ["g1", "g2", "g3", "f1", "f2"] {
            assert_eq!(g.person_of(f), Some("p1"), "{f}");
        }
        assert_eq!(ids.0, 1);
    }

    #[test]
    fn manual_faces_are_never_changed_or_taken_along() {
        let mut ids = Ids(0);
        let mut g = graph(&[
            ("owner_yes", 0.0, Some("alice"), true), // assigned by hand
            ("owner_no", 3.0, None, true),           // unassigned by hand
            ("n1", 20.0, None, false),
            ("n2", 25.0, None, false),
        ]);
        // n1 and n2 join alice through the owner's face; the unassigned manual one stays out.
        let out = run(&mut g, &["owner_yes", "owner_no", "n1", "n2"], &mut ids);
        let touched: Vec<&str> = out.iter().map(|a| a.face_id.as_str()).collect();
        assert!(touched.contains(&"n1") && touched.contains(&"n2"));
        assert!(!touched.contains(&"owner_yes") && !touched.contains(&"owner_no"));
        assert_eq!(g.person_of("owner_no"), None);
        assert!(g.is_manual("owner_no") && g.is_manual("owner_yes"));
        assert_eq!(g.person_of("owner_yes"), Some("alice"));
        // A full pass finds nothing for a manual face either.
        assert!(!g.unassigned().contains(&"owner_no".to_string()));

        // Without a person near, manual faces don't count towards a new one.
        let mut g = graph(&[
            ("m1", 0.0, None, true),
            ("a", 5.0, None, false),
            ("b", 10.0, None, false),
        ]);
        assert!(run(&mut g, &["a", "b"], &mut ids).is_empty());
    }

    #[test]
    fn set_manual_remove_and_move_people() {
        let mut g = graph(&[("a", 0.0, Some("p1"), false), ("b", 5.0, Some("p2"), false)]);
        g.set_manual("a", None);
        assert_eq!(g.person_of("a"), None);
        assert!(g.is_manual("a"));
        g.move_people(&["p2".to_string()], "p9");
        assert_eq!(g.person_of("b"), Some("p9"));
        assert!(g.remove("b"));
        assert!(!g.remove("b"));
        assert_eq!(g.len(), 1);
        assert!(g.update_vector("a", &unit(30.0)));
        assert!(!g.update_vector("zz", &unit(30.0)));
        assert!(!g.insert("bad", &[0.0, 0.0], None, false));
    }
}
