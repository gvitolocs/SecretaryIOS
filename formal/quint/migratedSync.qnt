// Lucina-bring client -> server item-op sync, Quint port of
// formal/MigratedSync.tla (same architecture as crates/lucina-core/src/sync.rs).
//
// Stop-and-wait per client: commits append to a FIFO outbox (delete only
// after its add); each op is sent once on the in-order channel plus one
// delayed duplicate on an out-of-band retry path that may overtake later
// ops. The server applies every copy through the op-id dedup gate (DEDUP).
//
// Bounds: 2 clients, 1 owned item each, Add then Delete per client.
// Invariant checked by the simulator: deletedStaysDeleted.
// Witness: generate with DEDUP = false -- the invariant must be violated.

module migratedSync {
  type Op = { c: str, kind: str, item: str }

  val DEDUP = true

  // explicit bounds (2 clients x 1 owned item, add+del each)
  val c1add = { c: "c1", kind: "add", item: "i1" }
  val c1del = { c: "c1", kind: "del", item: "i1" }
  val c2add = { c: "c2", kind: "add", item: "i2" }
  val c2del = { c: "c2", kind: "del", item: "i2" }
  val allOps = Set(c1add, c1del, c2add, c2del)
  val clients = Set("c1", "c2")

  var committed: Set[Op]            // ops committed locally (any client)
  var outbox: str -> List[Op]    // per-client FIFO outbox
  var chan: str -> List[Op]      // per-client in-order channel
  var retry: str -> Set[Op]      // delayed duplicate, out-of-band
  var copies: str -> int         // 0 until current head sent
  var applied: Set[Op]              // ops applied at least once (dedup keys)
  var serverItems: Set[str]
  var everDeleted: Set[str]

  val deletedStaysDeleted = everDeleted.forall(i => not(serverItems.contains(i)))

  // identity steps (frame-complete no-ops) to keep if/else frames equal
  action noopClient = (outbox' = outbox) and (chan' = chan) and
    (retry' = retry) and (copies' = copies)
  action noopApply = (serverItems' = serverItems) and
    (everDeleted' = everDeleted) and (applied' = applied)
  action noopDeliverIn = noopApply and (chan' = chan)
  action noopDeliverRetry = noopApply and (retry' = retry)

  action init = (committed' = Set()) and
    (outbox' = Map("c1" -> [], "c2" -> [])) and
    (chan' = Map("c1" -> [], "c2" -> [])) and
    (retry' = Map("c1" -> Set(), "c2" -> Set())) and
    (copies' = Map("c1" -> 0, "c2" -> 0)) and
    (applied' = Set()) and
    (serverItems' = Set()) and
    (everDeleted' = Set())

  // one server-side application of an arriving copy; the dedup gate is
  // what makes the overtaking retry duplicate harmless
  action applyOnce(op) = {
    if (not (DEDUP and applied.contains(op))) {
      if (op.kind == "add") {
        (serverItems' = serverItems.union(Set(op.item))) and
        (everDeleted' = everDeleted) and
        (applied' = applied.union(Set(op)))
      } else {
        (serverItems' = serverItems.exclude(Set(op.item))) and
        (everDeleted' = everDeleted.union(Set(op.item))) and
        (applied' = applied.union(Set(op)))
      }
    } else {
      (serverItems' = serverItems) and
      (everDeleted' = everDeleted) and
      (applied' = applied)
    }
  }

  action commit(c) = {
    val ready = allOps.filter(
      o => (o.c == c)
        and not(committed.contains(o))
        // a delete op only exists after its add was committed
        and ((o.kind == "add")
             or committed.contains({ c: c, kind: "add", item: o.item })))
    if (ready.size() > 0) {
      nondet op = oneOf(ready)
      (committed' = committed.union(Set(op))) and
      (outbox' = outbox.set(c, outbox.get(c).append(op)))
    } else {
      (committed' = committed) and (outbox' = outbox)
    }
  }

  // stop-and-wait, FIFO outbox: exactly the OpLog drain-from-front
  // discipline (ack applied head, or send the head once + retry copy)
  action pump(c) = {
    if (outbox.get(c).length() > 0) {
      if (applied.contains(outbox.get(c)[0])) {
        (outbox' = outbox.set(c, outbox.get(c).tail())) and
        (copies' = copies.set(c, 0)) and (chan' = chan) and (retry' = retry)
      } else {
        if (copies.get(c) == 0) {
          (chan' = chan.set(c, chan.get(c).append(outbox.get(c)[0]))) and
          (retry' = retry.set(c, retry.get(c).union(Set(outbox.get(c)[0])))) and
          (copies' = copies.set(c, 1)) and (outbox' = outbox)
        } else {
          noopClient
        }
      }
    } else {
      noopClient
    }
  }

  action deliverInOrder() = {
    val ready = clients.filter(cc => chan.get(cc).length() > 0)
    if (ready.size() > 0) {
      nondet c = oneOf(ready)
      applyOnce(chan.get(c)[0]) and
      (chan' = chan.set(c, chan.get(c).tail()))
    } else {
      noopDeliverIn
    }
  }

  action deliverRetry() = {
    val ready = clients.filter(cc => retry.get(cc).size() > 0)
    if (ready.size() > 0) {
      nondet c = oneOf(ready)
      nondet op = oneOf(retry.get(c))
      applyOnce(op) and
      (retry' = retry.set(c, retry.get(c).exclude(Set(op))))
    } else {
      noopDeliverRetry
    }
  }

  // every top-level step must have the same frame, so each wrapper
  // composes its action with identity assignments for the untouched vars
  action doCommit(c) = commit(c) and (chan' = chan) and (retry' = retry) and
    (copies' = copies) and (applied' = applied) and
    (serverItems' = serverItems) and (everDeleted' = everDeleted)

  action doPump(c) = pump(c) and (committed' = committed) and
    (applied' = applied) and (serverItems' = serverItems) and
    (everDeleted' = everDeleted)

  action doDeliverIn = deliverInOrder and (outbox' = outbox) and
    (retry' = retry) and (copies' = copies) and (committed' = committed)

  action doDeliverRetry = deliverRetry and (outbox' = outbox) and
    (chan' = chan) and (copies' = copies) and (committed' = committed)

  action step = any {
    doCommit("c1"),
    doCommit("c2"),
    doPump("c1"),
    doPump("c2"),
    doDeliverIn,
    doDeliverRetry,
  }
}
