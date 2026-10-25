// Verus kernel of `deploy-guard check` (tools/deploy-guard), the fix the
// deploy model found (verification/models/src/deploy.rs, formal/deploy/).
//
// Commit content is a set of features, as a bit mask: "A is an ancestor of B"
// is "A's bits are a subset of B's". The guard lets a build go live only when
// the tree is clean, the commit is on main, and the live build is an ancestor
// of the commit. Proved here for all inputs (the models check 2-3 sessions):
//
//   D-K1  `guard_allows` is exactly that rule.
//   D-K2  an allowed deploy keeps "nothing deployed before is missing":
//         if everything ever deployed is in the live build, it is also in
//         the new one (no rollback, by induction over deploys).
//   D-K3  an allowed deploy keeps "production is a commit on main", also
//         after main grows (merges only add).

use vstd::prelude::*;

verus! {

pub open spec fn subset(a: u64, b: u64) -> bool {
    a & !b == 0
}

/// D-K1: the decision of `deploy-guard check`.
pub fn guard_allows(live: u64, commit: u64, main: u64, dirty: bool) -> (ok: bool)
    ensures ok == (!dirty && subset(commit, main) && subset(live, commit)),
{
    !dirty && commit & !main == 0 && live & !commit == 0
}

/// D-K2: no rollback across an allowed deploy.
pub proof fn allowed_deploy_keeps_history(ever: u64, live: u64, commit: u64, main: u64)
    requires
        subset(ever, live),
        subset(live, commit),
    ensures
        subset(ever | commit, commit),
{
    assert((ever & !live == 0 && live & !commit == 0) ==> ((ever | commit) & !commit == 0)) by (bit_vector);
}

/// D-K3: production stays on main when main only grows.
pub proof fn prod_stays_on_main(commit: u64, main: u64, merged: u64)
    requires
        subset(commit, main),
    ensures
        subset(commit, main | merged),
{
    assert((commit & !main == 0) ==> (commit & !(main | merged) == 0)) by (bit_vector);
}

pub proof fn subset_refl(x: u64)
    ensures
        subset(x, x),
{
    assert(x & !x == 0) by (bit_vector);
}

pub proof fn subset_trans(a: u64, b: u64, c: u64)
    requires
        subset(a, b),
        subset(b, c),
    ensures
        subset(a, c),
{
    assert((a & !b == 0 && b & !c == 0) ==> (a & !c == 0)) by (bit_vector);
}

/// A sequence of guarded deploys never rolls back: every build deployed
/// before is contained in the last one (each step is D-K2's `subset(live, commit)`).
pub proof fn guarded_sequence_never_rolls_back(builds: Seq<u64>)
    requires
        builds.len() > 0,
        forall|k: int| 0 < k < builds.len() ==> #[trigger] subset(builds[k - 1], builds[k]),
    ensures
        forall|k: int| 0 <= k < builds.len() ==> #[trigger] subset(builds[k], builds[builds.len() - 1]),
    decreases builds.len(),
{
    let n = builds.len() - 1;
    if builds.len() == 1 {
        subset_refl(builds[0]);
    } else {
        let prefix = builds.drop_last();
        assert forall|k: int| 0 < k < prefix.len() implies #[trigger] subset(prefix[k - 1], prefix[k]) by {
            assert(prefix[k - 1] == builds[k - 1] && prefix[k] == builds[k]);
            assert(subset(builds[k - 1], builds[k]));
        }
        guarded_sequence_never_rolls_back(prefix);
        assert forall|k: int| 0 <= k < builds.len() implies #[trigger] subset(builds[k], builds[n]) by {
            if k < n {
                assert(prefix[k] == builds[k] && prefix[n - 1] == builds[n - 1]);
                assert(subset(prefix[k], prefix[prefix.len() - 1]));
                assert(subset(builds[n - 1], builds[n]));
                subset_trans(builds[k], builds[n - 1], builds[n]);
            } else {
                subset_refl(builds[n]);
            }
        }
    }
}

fn main() {}

} // verus!
