# lucina-agentd deploy

`install.sh` builds and installs `lucina-agentd` and `lagent` as a user unit (see its header).

## The user slice `build.slice`

`lagent work start` launches each lane run as `systemd-run --user --scope --slice=build.slice
-p CPUQuota=400% -p MemoryMax=12G ...` (finding HOST-11: uncapped work scopes plus builds
caused a global out-of-memory on nezopt). agentd therefore needs the **user unit `build.slice`**
to be loaded in the user manager. It is not shipped in this repository: the host provisions it
at `~/.config/systemd/user/build.slice` (lead LB). As provisioned on nezopt it sets
`MemoryHigh=34G` and `CPUWeight=30`, and it is shared with bq jobs, lane attempts and
check-fast steps.

agentd asks `systemctl --user show build.slice -p LoadState --value` (5 s cap) when a start
would use a scope. Anything but `loaded`, or no answer in time, refuses the start with
`build.slice is not loaded in the user manager ...` before anything is spawned (finding
WORK-25). A "not loaded" answer is looked up again after a minute, so a slice added later is
picked up without a restart; a `loaded` answer is kept.

`LUCINA_AGENTD_WORK_SCOPE` (`auto`, `on`, `off`; default `auto`) keeps its meaning: with `off`
no scope is used and the slice is not checked.
