// Lucina's Pokémon Red planner kernel (Lucina repo, rust/lucina-pokemon, branch lucina-game-link).
//
// Production:
//   Grid.kind / Grid.elev   <-> MapInfo tiles: `floor` (passable), `cut_tree`, `key_door`, people and
//                              story_blocked cells, the tileset's elevation pairs (`pair_blocked`) and
//                              LEDGES (world.rs)
//   target                  <-> the step and ledge-hop arms of world::neighbours (one map: warps, edge
//                              connections, spinners and the Mansion holes are not in this kernel; the ROM
//                              differential test covers them)
//   plan                    <-> world::plan (Dijkstra there; a bounded relaxation here: the kernel proves
//                              what every returned route satisfies, not which route is cheapest)
//   STORY_REQUIRES          <-> plan.rs STORY + REQUIRES (same order); next_milestone <-> progress()
// Differential tests (Lucina repo, rust/lucina-pokemon): world.rs `routes_only_enter_walkable_cells`
// (every step of planned ROM routes against this step rule) and plan.rs `story_requirements_come_first`
// (the table below, parsed from the STORY_REQUIRES line, equals plan.rs REQUIRES).
//
// Proven:
//   P1 legal_lands_on_walkable: every legal move ends on an enterable cell (floor, a Cut tree only with
//      Cut, a card-key door only with the Card Key), never a wall or a blocked cell; a two-cell move
//      crosses a ledge in its own direction from a floor cell.
//   P2 plan_routes_are_legal: a route returned by `plan` starts at the goal, ends at the start, and every
//      consecutive pair is a legal move (so, with P1, it never enters a wall, a person, a story block or a
//      locked door).
//   P3 termination: every loop of `plan` and `step_target` has a decreases clause (Verus checks it).
//   P4 story_order_respects_requirements: the milestone `next_milestone` picks has every requirement done.
use vstd::prelude::*;

verus! {

pub const FLOOR: u8 = 0;
pub const WALL: u8 = 1;
pub const TREE: u8 = 2;
pub const KEY_DOOR: u8 = 3;
/// A person, a story block (gate guard, Snorlax) or a cell the player was turned back from.
pub const BLOCKED: u8 = 4;
pub const LEDGE_DOWN: u8 = 5;
pub const LEDGE_LEFT: u8 = 6;
pub const LEDGE_RIGHT: u8 = 7;
/// 128 x 128 cells: a map is at most 64 x 64 blocks of 2 x 2 cells.
pub const MAX_CELLS: usize = 16384;
pub const MAX_DIST: u64 = 1_000_000;

#[derive(Clone, Copy)]
pub struct Abilities {
    pub cut: bool,
    pub card_key: bool,
}

pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub kind: Vec<u8>,
    pub elev: Vec<u8>,
}

impl Grid {
    pub open spec fn wf(&self) -> bool {
        &&& 0 < self.w && 0 < self.h
        &&& self.w * self.h <= MAX_CELLS
        &&& self.kind@.len() == self.w * self.h
        &&& self.elev@.len() == self.w * self.h
    }

    pub open spec fn inside(&self, x: int, y: int) -> bool {
        0 <= x < self.w && 0 <= y < self.h
    }

    pub open spec fn idx(&self, x: int, y: int) -> int {
        y * self.w + x
    }

    pub open spec fn k(&self, x: int, y: int) -> u8 {
        self.kind@[self.idx(x, y)]
    }

    pub open spec fn e(&self, x: int, y: int) -> u8 {
        self.elev@[self.idx(x, y)]
    }
}

// ---------------------------------------------------------------- step rule (spec)

pub open spec fn dx(d: int) -> int {
    if d == 2 { -1 } else if d == 3 { 1 } else { 0 }
}

pub open spec fn dy(d: int) -> int {
    if d == 0 { -1 } else if d == 1 { 1 } else { 0 }
}

/// The direction a ledge tile lets the player hop (0 up, 1 down, 2 left, 3 right), -1 for other tiles.
pub open spec fn ledge_of(k: u8) -> int {
    if k == LEDGE_DOWN { 1 } else if k == LEDGE_LEFT { 2 } else if k == LEDGE_RIGHT { 3 } else { -1 }
}

pub open spec fn enterable(k: u8, ab: Abilities) -> bool {
    k == FLOOR || (k == TREE && ab.cut) || (k == KEY_DOOR && ab.card_key)
}

/// Where a move in direction `d` from (x, y) lands, if it is allowed.
pub open spec fn target(g: &Grid, ab: Abilities, x: int, y: int, d: int) -> Option<(int, int)> {
    let nx = x + dx(d);
    let ny = y + dy(d);
    if !g.inside(nx, ny) {
        None
    } else if enterable(g.k(nx, ny), ab) && g.e(nx, ny) == g.e(x, y) {
        Some((nx, ny))
    } else if ledge_of(g.k(nx, ny)) == d && g.k(x, y) == FLOOR && g.inside(nx + dx(d), ny + dy(d))
        && g.k(nx + dx(d), ny + dy(d)) == FLOOR {
        Some((nx + dx(d), ny + dy(d)))
    } else {
        None
    }
}

pub open spec fn legal(g: &Grid, ab: Abilities, x1: int, y1: int, x2: int, y2: int) -> bool {
    ||| target(g, ab, x1, y1, 0) == Some((x2, y2))
    ||| target(g, ab, x1, y1, 1) == Some((x2, y2))
    ||| target(g, ab, x1, y1, 2) == Some((x2, y2))
    ||| target(g, ab, x1, y1, 3) == Some((x2, y2))
}

/// P1.
pub proof fn legal_lands_on_walkable(g: &Grid, ab: Abilities, x1: int, y1: int, x2: int, y2: int)
    requires
        g.wf(),
        g.inside(x1, y1),
        legal(g, ab, x1, y1, x2, y2),
    ensures
        g.inside(x2, y2),
        enterable(g.k(x2, y2), ab),
        g.k(x2, y2) != WALL && g.k(x2, y2) != BLOCKED,
        g.k(x2, y2) == KEY_DOOR ==> ab.card_key,
        g.k(x2, y2) == TREE ==> ab.cut,
        // a hop: the cell in between is a ledge pointing this way, taken from a floor cell
        (x2 - x1) * (x2 - x1) + (y2 - y1) * (y2 - y1) > 1 ==> exists|d: int|
            0 <= d < 4 && x2 == x1 + 2 * dx(d) && y2 == y1 + 2 * dy(d) && ledge_of(g.k(x1 + dx(d), y1 + dy(d))) == d
                && g.k(x1, y1) == FLOOR,
{
    let d = if target(g, ab, x1, y1, 0) == Some((x2, y2)) { 0int } else if target(g, ab, x1, y1, 1) == Some(
        (x2, y2),
    ) { 1int } else if target(g, ab, x1, y1, 2) == Some((x2, y2)) { 2int } else { 3int };
    assert(target(g, ab, x1, y1, d) == Some((x2, y2)));
    let nx = x1 + dx(d);
    let ny = y1 + dy(d);
    if !(enterable(g.k(nx, ny), ab) && g.e(nx, ny) == g.e(x1, y1)) {
        assert(x2 == x1 + 2 * dx(d) && y2 == y1 + 2 * dy(d));
    } else {
        assert((x2 - x1) * (x2 - x1) + (y2 - y1) * (y2 - y1) == 1) by (nonlinear_arith)
            requires
                x2 == x1 + dx(d),
                y2 == y1 + dy(d),
                (dx(d) == 0 && (dy(d) == 1 || dy(d) == -1)) || (dy(d) == 0 && (dx(d) == 1 || dx(d) == -1)),
        ;
    }
}

// ---------------------------------------------------------------- exec kernel

proof fn lemma_idx(g: &Grid, x: int, y: int)
    requires
        g.wf(),
        g.inside(x, y),
    ensures
        0 <= g.idx(x, y) < g.w * g.h,
        0 <= y * g.w <= g.idx(x, y),
{
    assert(0 <= y * g.w + x < g.w * g.h) by (nonlinear_arith)
        requires
            0 <= x < g.w,
            0 <= y < g.h,
            0 < g.w,
    {
        assert(y * g.w + x < (y + 1) * g.w);
        assert((y + 1) * g.w <= g.h * g.w);
    }
    assert(0 <= y * g.w) by (nonlinear_arith)
        requires
            0 <= y,
            0 < g.w,
    ;
}

fn idx(g: &Grid, x: usize, y: usize) -> (r: usize)
    requires
        g.wf(),
        g.inside(x as int, y as int),
    ensures
        r == g.idx(x as int, y as int),
        r < g.w * g.h,
{
    proof {
        lemma_idx(g, x as int, y as int);
    }
    y * g.w + x
}

fn kind_at(g: &Grid, x: usize, y: usize) -> (r: u8)
    requires
        g.wf(),
        g.inside(x as int, y as int),
    ensures
        r == g.k(x as int, y as int),
{
    let i = idx(g, x, y);
    g.kind[i]
}

fn elev_at(g: &Grid, x: usize, y: usize) -> (r: u8)
    requires
        g.wf(),
        g.inside(x as int, y as int),
    ensures
        r == g.e(x as int, y as int),
{
    let i = idx(g, x, y);
    g.elev[i]
}

/// The neighbour of (x, y) in direction `d`, if on the map.
fn neighbour(g: &Grid, x: usize, y: usize, d: usize) -> (r: Option<(usize, usize)>)
    requires
        g.wf(),
        d < 4,
    ensures
        match r {
            Some((a, b)) => a as int == x + dx(d as int) && b as int == y + dy(d as int) && g.inside(a as int, b as int),
            None => !g.inside(x + dx(d as int), y + dy(d as int)),
        },
{
    if d == 0 {
        if y == 0 || x >= g.w || y > g.h { None } else { Some((x, y - 1)) }
    } else if d == 1 {
        if x >= g.w || y >= g.h || y + 1 >= g.h { None } else { Some((x, y + 1)) }
    } else if d == 2 {
        if x == 0 || x > g.w || y >= g.h { None } else { Some((x - 1, y)) }
    } else {
        if y >= g.h || x >= g.w || x + 1 >= g.w { None } else { Some((x + 1, y)) }
    }
}

fn enterable_exec(k: u8, ab: Abilities) -> (r: bool)
    ensures
        r == enterable(k, ab),
{
    k == FLOOR || (k == TREE && ab.cut) || (k == KEY_DOOR && ab.card_key)
}

fn ledge_exec(k: u8) -> (r: usize)
    ensures
        r == if ledge_of(k) < 0 { 4 } else { ledge_of(k) },
{
    if k == LEDGE_DOWN { 1 } else if k == LEDGE_LEFT { 2 } else if k == LEDGE_RIGHT { 3 } else { 4 }
}

/// world::neighbours, one map: the step rule.
pub fn step_target(g: &Grid, ab: Abilities, x: usize, y: usize, d: usize) -> (r: Option<(usize, usize)>)
    requires
        g.wf(),
        g.inside(x as int, y as int),
        d < 4,
    ensures
        match r {
            Some((a, b)) => target(g, ab, x as int, y as int, d as int) == Some((a as int, b as int)),
            None => target(g, ab, x as int, y as int, d as int).is_none(),
        },
{
    match neighbour(g, x, y, d) {
        None => None,
        Some((nx, ny)) => {
            let k = kind_at(g, nx, ny);
            if enterable_exec(k, ab) && elev_at(g, nx, ny) == elev_at(g, x, y) {
                Some((nx, ny))
            } else if ledge_exec(k) == d && kind_at(g, x, y) == FLOOR {
                match neighbour(g, nx, ny, d) {
                    Some((bx, by)) => if kind_at(g, bx, by) == FLOOR { Some((bx, by)) } else { None },
                    None => None,
                }
            } else {
                None
            }
        },
    }
}

/// Search state: per cell, visited, its own coordinates, its parent's and its distance.
pub struct Search {
    pub visited: Vec<bool>,
    pub ox: Vec<usize>,
    pub oy: Vec<usize>,
    pub px: Vec<usize>,
    pub py: Vec<usize>,
    pub dist: Vec<u64>,
}

impl Search {
    /// Visited cell `i`: its own coordinates, and (other than the start) a visited parent one step closer,
    /// reached by a legal move.
    pub open spec fn cell_ok(&self, g: &Grid, ab: Abilities, sx: int, sy: int, i: int) -> bool {
        let n = g.w * g.h;
        let p = g.idx(self.px@[i] as int, self.py@[i] as int);
        &&& g.inside(self.ox@[i] as int, self.oy@[i] as int)
        &&& g.idx(self.ox@[i] as int, self.oy@[i] as int) == i
        &&& self.dist@[i] <= MAX_DIST
        &&& (i == g.idx(sx, sy) || (g.inside(self.px@[i] as int, self.py@[i] as int) && 0 <= p < n && self.visited@[p]
            && self.dist@[p] + 1 == self.dist@[i] && legal(
            g,
            ab,
            self.px@[i] as int,
            self.py@[i] as int,
            self.ox@[i] as int,
            self.oy@[i] as int,
        )))
    }

    pub open spec fn tree(&self, g: &Grid, ab: Abilities, sx: int, sy: int) -> bool {
        let n = g.w * g.h;
        let s = g.idx(sx, sy);
        &&& self.visited@.len() == n && self.ox@.len() == n && self.oy@.len() == n
        &&& self.px@.len() == n && self.py@.len() == n && self.dist@.len() == n
        &&& 0 <= s < n && self.visited@[s] && self.ox@[s] == sx && self.oy@[s] == sy && self.dist@[s] == 0
        &&& forall|i: int| #![trigger self.visited@[i]] 0 <= i < n && self.visited@[i] ==> self.cell_ok(g, ab, sx, sy, i)
    }
}

/// Follow every legal move out of (cx, cy) to unvisited cells.
fn relax(s: &mut Search, g: &Grid, ab: Abilities, sx: usize, sy: usize, cx: usize, cy: usize)
    requires
        g.wf(),
        g.inside(sx as int, sy as int),
        g.inside(cx as int, cy as int),
        old(s).tree(g, ab, sx as int, sy as int),
    ensures
        final(s).tree(g, ab, sx as int, sy as int),
{
    let ci = idx(g, cx, cy);
    if !s.visited[ci] || s.dist[ci] >= MAX_DIST {
        return;
    }
    let mut d: usize = 0;
    while d < 4
        invariant
            g.wf(),
            g.inside(sx as int, sy as int),
            g.inside(cx as int, cy as int),
            ci == g.idx(cx as int, cy as int),
            0 <= ci < g.w * g.h,
            s.tree(g, ab, sx as int, sy as int),
            s.visited@[ci as int],
            s.dist@[ci as int] < MAX_DIST,
        decreases 4 - d,
    {
        if let Some((tx, ty)) = step_target(g, ab, cx, cy, d) {
            proof {
                assert(legal(g, ab, cx as int, cy as int, tx as int, ty as int));
            }
            let ti = idx(g, tx, ty);
            if !s.visited[ti] {
                let nd = s.dist[ci] + 1;
                let ghost before = *s;
                s.visited.set(ti, true);
                s.ox.set(ti, tx);
                s.oy.set(ti, ty);
                s.px.set(ti, cx);
                s.py.set(ti, cy);
                s.dist.set(ti, nd);
                proof {
                    let n = g.w * g.h;
                    let st = g.idx(sx as int, sy as int);
                    assert(ti as int != st);
                    assert(ti != ci);
                    assert forall|i: int|
                        #![trigger s.visited@[i]]
                        0 <= i < n && s.visited@[i] implies s.cell_ok(g, ab, sx as int, sy as int, i)
 by {
                        if i != ti as int {
                            assert(before.visited@[i]);
                            assert(before.cell_ok(g, ab, sx as int, sy as int, i));
                            if i != st {
                                let p = g.idx(before.px@[i] as int, before.py@[i] as int);
                                assert(before.visited@[p]);
                                assert(p != ti as int);
                            }
                        } else {
                            lemma_idx(g, cx as int, cy as int);
                        }
                    }
                }
            }
        }
        d += 1;
    }
}

/// The reconstruction so far: from the goal back to (lx, ly), every step legal.
pub open spec fn back_path(g: &Grid, ab: Abilities, p: Seq<(usize, usize)>, gx: int, gy: int, lx: int, ly: int) -> bool {
    &&& p.len() >= 1
    &&& p[0].0 == gx && p[0].1 == gy
    &&& p.last().0 == lx && p.last().1 == ly
    &&& forall|k: int| #![trigger p[k]] 0 <= k < p.len() ==> g.inside(p[k].0 as int, p[k].1 as int)
    &&& forall|k: int|
        #![trigger p[k + 1]]
        0 <= k < p.len() - 1 ==> legal(g, ab, p[k + 1].0 as int, p[k + 1].1 as int, p[k].0 as int, p[k].1 as int)
}


/// Follow the parents from the goal back to the start.
fn rebuild(s: &Search, g: &Grid, ab: Abilities, sx: usize, sy: usize, gx: usize, gy: usize) -> (r: Option<
    Vec<(usize, usize)>,
>)
    requires
        g.wf(),
        g.inside(sx as int, sy as int),
        g.inside(gx as int, gy as int),
        s.tree(g, ab, sx as int, sy as int),
    ensures
        match r {
            Some(p) => route_ok(g, ab, p@, sx as int, sy as int, gx as int, gy as int),
            None => true,
        },
{
    let n = g.w * g.h;
    let si = idx(g, sx, sy);
    let gi = idx(g, gx, gy);
    if !s.visited[gi] {
        return None;
    }
    let mut path: Vec<(usize, usize)> = Vec::new();
    path.push((gx, gy));
    let mut cur = gi;
    assert(s.ox@[gi as int] == gx && s.oy@[gi as int] == gy) by {
        lemma_idx_injective(g, s.ox@[gi as int] as int, s.oy@[gi as int] as int, gx as int, gy as int);
    }
    while cur != si
        invariant
            g.wf(),
            n == g.w * g.h,
            g.inside(sx as int, sy as int),
            si == g.idx(sx as int, sy as int),
            s.tree(g, ab, sx as int, sy as int),
            0 <= cur < n,
            s.visited@[cur as int],
            back_path(g, ab, path@, gx as int, gy as int, s.ox@[cur as int] as int, s.oy@[cur as int] as int),
        decreases s.dist@[cur as int],
    {
        proof {
            assert(s.cell_ok(g, ab, sx as int, sy as int, cur as int));
        }
        let ppx = s.px[cur];
        let ppy = s.py[cur];
        let ghost old_path = path@;
        let ghost cx = s.ox@[cur as int] as int;
        let ghost cy = s.oy@[cur as int] as int;
        path.push((ppx, ppy));
        let next = idx(g, ppx, ppy);
        proof {
            assert(legal(g, ab, ppx as int, ppy as int, cx, cy));
            assert(s.visited@[next as int]);
            assert(s.cell_ok(g, ab, sx as int, sy as int, next as int));
            lemma_idx_injective(g, s.ox@[next as int] as int, s.oy@[next as int] as int, ppx as int, ppy as int);
            assert(path@.len() == old_path.len() + 1);
            assert(path@[path@.len() - 2] == old_path.last());
            assert forall|k: int| #![trigger path@[k]] 0 <= k < path@.len() implies g.inside(
                path@[k].0 as int,
                path@[k].1 as int,
            ) by {
                if k < old_path.len() {
                    assert(path@[k] == old_path[k]);
                }
            }
            assert forall|k: int|
                #![trigger path@[k + 1]]
                0 <= k < path@.len() - 1 implies legal(
                    g,
                    ab,
                    path@[k + 1].0 as int,
                    path@[k + 1].1 as int,
                    path@[k].0 as int,
                    path@[k].1 as int,
                ) by {
                if k < old_path.len() - 1 {
                    assert(path@[k] == old_path[k] && path@[k + 1] == old_path[k + 1]);
                } else {
                    assert(path@[k] == old_path.last());
                }
            }
        }
        cur = next;
    }
    proof {
        assert(s.ox@[si as int] == sx && s.oy@[si as int] == sy);
    }
    Some(path)
}

pub open spec fn route_ok(g: &Grid, ab: Abilities, p: Seq<(usize, usize)>, sx: int, sy: int, gx: int, gy: int) -> bool {
    &&& p.len() >= 1
    &&& p[0].0 == gx && p[0].1 == gy
    &&& p.last().0 == sx && p.last().1 == sy
    &&& forall|i: int|
        #![trigger p[i]]
        0 <= i < p.len() ==> g.inside(p[i].0 as int, p[i].1 as int)
    &&& forall|i: int|
        #![trigger p[i + 1]]
        0 <= i < p.len() - 1 ==> legal(g, ab, p[i + 1].0 as int, p[i + 1].1 as int, p[i].0 as int, p[i].1 as int)
}

/// world::plan on one map. The route comes back goal first.
pub fn plan(g: &Grid, ab: Abilities, sx: usize, sy: usize, gx: usize, gy: usize) -> (r: Option<Vec<(usize, usize)>>)
    requires
        g.wf(),
        g.inside(sx as int, sy as int),
        g.inside(gx as int, gy as int),
    ensures
        match r {
            Some(p) => route_ok(g, ab, p@, sx as int, sy as int, gx as int, gy as int),
            None => true,
        },
{
    let n = g.w * g.h;
    let mut s = Search { visited: Vec::new(), ox: Vec::new(), oy: Vec::new(), px: Vec::new(), py: Vec::new(), dist: Vec::new() };
    let mut i: usize = 0;
    while i < n
        invariant
            g.wf(),
            g.inside(sx as int, sy as int),
            g.inside(gx as int, gy as int),
            n == g.w * g.h,
            i <= n,
            s.visited@.len() == i && s.ox@.len() == i && s.oy@.len() == i,
            s.px@.len() == i && s.py@.len() == i && s.dist@.len() == i,
            forall|j: int| 0 <= j < i ==> !#[trigger] s.visited@[j],
        decreases n - i,
    {
        s.visited.push(false);
        s.ox.push(0);
        s.oy.push(0);
        s.px.push(0);
        s.py.push(0);
        s.dist.push(0);
        i += 1;
    }
    let si = idx(g, sx, sy);
    let ghost v0 = s.visited@;
    s.visited.set(si, true);
    s.ox.set(si, sx);
    s.oy.set(si, sy);
    s.dist.set(si, 0);
    proof {
        assert forall|j: int| #![trigger s.visited@[j]] 0 <= j < n && s.visited@[j] implies s.cell_ok(
            g,
            ab,
            sx as int,
            sy as int,
            j,
        ) by {
            if j != si as int {
                assert(v0[j] == s.visited@[j]);
                assert(!v0[j]);
            }
        }
    }
    assert(s.tree(g, ab, sx as int, sy as int));
    // n rounds over every cell: a shortest route has fewer than n moves.
    let mut round: usize = 0;
    while round < n
        invariant
            g.wf(),
            n == g.w * g.h,
            g.inside(sx as int, sy as int),
            s.tree(g, ab, sx as int, sy as int),
        decreases n - round,
    {
        let mut y: usize = 0;
        while y < g.h
            invariant
                g.wf(),
                g.inside(sx as int, sy as int),
                s.tree(g, ab, sx as int, sy as int),
            decreases g.h - y,
        {
            let mut x: usize = 0;
            while x < g.w
                invariant
                    g.wf(),
                    y < g.h,
                    g.inside(sx as int, sy as int),
                    s.tree(g, ab, sx as int, sy as int),
                decreases g.w - x,
            {
                relax(&mut s, g, ab, sx, sy, x, y);
                x += 1;
            }
            y += 1;
        }
        round += 1;
    }
    rebuild(&s, g, ab, sx, sy, gx, gy)
}

proof fn lemma_idx_injective(g: &Grid, x1: int, y1: int, x2: int, y2: int)
    requires
        g.wf(),
        g.inside(x1, y1),
        g.inside(x2, y2),
        g.idx(x1, y1) == g.idx(x2, y2),
    ensures
        x1 == x2 && y1 == y2,
{
    assert(y1 == y2) by (nonlinear_arith)
        requires
            y1 * g.w + x1 == y2 * g.w + x2,
            0 <= x1 < g.w,
            0 <= x2 < g.w,
            0 <= y1,
            0 <= y2,
    {
        if y1 < y2 {
            assert(y1 * g.w + x1 < (y1 + 1) * g.w);
            assert((y1 + 1) * g.w <= y2 * g.w);
        } else if y2 < y1 {
            assert(y2 * g.w + x2 < (y2 + 1) * g.w);
            assert((y2 + 1) * g.w <= y1 * g.w);
        }
    }
}

// ---------------------------------------------------------------- story order (P4)

// The prerequisites of each STORY milestone (plan.rs), by index; a differential test in the Lucina repo
// parses the next line and compares it with plan.rs REQUIRES.
// STORY_REQUIRES = [[],[0],[1],[2],[3],[4],[5],[5],[7],[8],[6,8],[10],[11],[12],[13],[10],[15],[14],[4,6,9,10,13,16,17],[10,13],[14,18,19]]
pub const STORY_LEN: usize = 21;

/// Milestone `m` needs milestone `q` done first (the STORY_REQUIRES line above).
pub open spec fn req(m: int, q: int) -> bool {
    ||| (m == 1 && q == 0)
    ||| (m == 2 && q == 1)
    ||| (m == 3 && q == 2)
    ||| (m == 4 && q == 3)
    ||| (m == 5 && q == 4)
    ||| (m == 6 && q == 5)
    ||| (m == 7 && q == 5)
    ||| (m == 8 && q == 7)
    ||| (m == 9 && q == 8)
    ||| (m == 10 && (q == 6 || q == 8))
    ||| (m == 11 && q == 10)
    ||| (m == 12 && q == 11)
    ||| (m == 13 && q == 12)
    ||| (m == 14 && q == 13)
    ||| (m == 15 && q == 10)
    ||| (m == 16 && q == 15)
    ||| (m == 17 && q == 14)
    ||| (m == 18 && (q == 4 || q == 6 || q == 9 || q == 10 || q == 13 || q == 16 || q == 17))
    ||| (m == 19 && (q == 10 || q == 13))
    ||| (m == 20 && (q == 14 || q == 18 || q == 19))
}

/// The table only points backwards.
pub proof fn story_table_points_backwards()
    ensures
        forall|m: int, q: int| #[trigger] req(m, q) ==> 0 <= q < m < STORY_LEN,
{
}

/// progress(): the first milestone not done.
pub fn next_milestone(done: &Vec<bool>) -> (r: usize)
    requires
        done@.len() == STORY_LEN,
    ensures
        r <= STORY_LEN,
        forall|j: int| 0 <= j < r ==> done@[j],
        r < STORY_LEN ==> !done@[r as int],
{
    let mut i: usize = 0;
    while i < STORY_LEN
        invariant
            done@.len() == STORY_LEN,
            i <= STORY_LEN,
            forall|j: int| 0 <= j < i ==> done@[j],
        decreases STORY_LEN - i,
    {
        if !done[i] {
            return i;
        }
        i += 1;
    }
    i
}

/// P4: the milestone the plan shows next has every prerequisite done.
pub proof fn story_order_respects_requirements(done: Seq<bool>, r: int)
    requires
        done.len() == STORY_LEN,
        0 <= r < STORY_LEN,
        forall|j: int| 0 <= j < r ==> done[j],
    ensures
        forall|q: int| #[trigger] req(r, q) ==> done[q],
{
    story_table_points_backwards();
}

} // verus!

fn main() {
}
