// Original reference-client sync semantics (full-snapshot push,
// last-write-wins server), Quint port of formal/OriginalSync.tla.
//
// Invariants (run separately):
//   serverIsSomePushed -- server only holds a snapshot some client pushed
//                         (atomic replace; must HOLD)
//   committedSurvive   -- a committed+pushed item survives on the server
//                         (the WITNESS: must FAIL -- interleaved
//                         full-snapshot pushes lose concurrent commits)

module originalSync {
  val clients = Set("c1", "c2")
  val owned = Map("c1" -> "i1", "c2" -> "i2")

  var localItems: str -> Set[str]
  var localCommitted: str -> Set[str]
  var pushedOnce: str -> bool
  var serverItems: Set[str]
  var pushed: Set[Set[str]]

  val serverIsSomePushed = pushed.contains(serverItems)

  val committedSurvive = clients.forall(
    c => not(pushedOnce.get(c)) or serverItems.contains(owned.get(c)))

  action init = (localItems' = Map("c1" -> Set(), "c2" -> Set())) and
    (localCommitted' = Map("c1" -> Set(), "c2" -> Set())) and
    (pushedOnce' = Map("c1" -> false, "c2" -> false)) and
    (serverItems' = Set()) and (pushed' = Set(Set()))

  // every top-level step touches all five vars (uniform frame)
  action work(c) = {
    if (not(pushedOnce.get(c)) and not(localCommitted.get(c).contains(owned.get(c)))) {
      (localItems' = localItems.set(c, localItems.get(c).union(Set(owned.get(c))))) and
      (localCommitted' = localCommitted.set(c, localCommitted.get(c).union(Set(owned.get(c))))) and
      (pushedOnce' = pushedOnce) and
      (serverItems' = serverItems) and
      (pushed' = pushed)
    } else {
      (localItems' = localItems) and
      (localCommitted' = localCommitted) and
      (pushedOnce' = pushedOnce.set(c, true)) and
      (serverItems' = localItems.get(c)) and
      (pushed' = pushed.union(Set(localItems.get(c))))
    }
  }

  action step = any {
    work("c1"),
    work("c2"),
  }
}
