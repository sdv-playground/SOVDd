---
name: guest-triage
description: Diagnose a guest VM (or the whole vehicle) over SOVD WITHOUT SSH — read system health, disk/RAM/log usage, run smoke tests, and pinpoint faults like a runaway log or a wedged service. Use when a guest is misbehaving, a flash failed with an opaque error, or you'd otherwise reach for ssh + df/du/free/journalctl. Keys off DISCOVERED capabilities, so it works the same for one VM or every ECU behind a gateway.
metadata:
  author: sumo-team
  version: "1.0"
---

# Guest Triage (diagnose without SSH)

Triage a guest — or the whole vehicle — entirely over the SOVD REST API. This is
the read-first counterpart to the UDS-focused `sovd-diagnostics` skill: where
that reads DIDs/DTCs on an ECU, this reads **system health** (memory, disk, log
usage, processes, boot log, container state) proxied from each guest's in-guest
`diag-agent`, plus the `scripts` (smoke tests) and `logs` surfaces.

The design principle that makes this skill small: **it discovers, it doesn't
hardcode.** Every probe/test is declared by a layer (`diag.d/*.toml`,
`tests/manifest.toml`) and surfaced as a SOVD capability. So the same steps work
on a VM that exposes 8 probes and one that exposes 40 — and scale to a whole
vehicle by walking the gateway's sub-entities. See
`tasks/diag-agent-design.md`.

## When to reach for this
- A guest is slow / unresponsive / a service died and you'd normally SSH in.
- A flash failed with an opaque error (e.g. `verify_part … out of memory`) —
  the root cause is often resource pressure a probe surfaces directly.
- You want a fast health snapshot before/after a change, or a smoke test run.
- You DON'T have (or don't want to use) an SSH path to the guest.

## Tooling: `sovd-cli`
All calls go through `sovd-cli` (typed client). The device serves HTTPS with a
bearer token once provisioned; mint/pass it as the log/flash scripts do
(`SOVD_TOKEN` env, `--ca-cert` for the tower root). Examples below omit auth
flags for brevity — add `--token "$SOVD_TOKEN" --ca-cert <root.pem>` (or
`--insecure` on a dev rig) exactly as `examples/autoloader/sovd-get-logs.sh`.

```bash
S="--server https://<device>:443"   # or http://localhost:8080 in dev
```

## Step 1 — discover what the target can tell you
Never assume an endpoint exists; read the entity's capabilities first.

```bash
sovd-cli $S list                 # components (vm1, vm2, supernova, hsm, …)
sovd-cli $S info vm1             # capabilities: logs / scripts / diagnostics / …
```

Branch on what's present:
- `diagnostics: true` → Step 2 (system probes).
- `scripts: true` → Step 3 (smoke tests).
- `logs: true` → Step 4 (log inspection / the cursor).
- none of the above → this target has no triage surface; say so, don't guess.

## Step 2 — system health probes (§7.9 diagnostics)
```bash
sovd-cli $S diagnostics vm1                 # list registered probes (+ ?tags=)
sovd-cli $S diagnostics vm1 guest-hal.mem   # gather one now
```
Fetch the health set and reason over it. Probe ids are `<layer>.<id>`; the base
guest-hal set (both VMs):

| Probe | Source | What it answers |
|---|---|---|
| `guest-hal.mem` | /proc/meminfo (QNX: pidin) | RAM total / available / swap |
| `guest-hal.load` | loadavg / uptime | is it overloaded, how long up |
| `guest-hal.disk` | df | filesystem usage |
| `guest-hal.shmem-usage` | du /dev/shmem | **RAM-log usage, largest first** |
| `guest-hal.log-usage` | du /var/log | disk-log usage, largest first |
| `guest-hal.procs` | ps by RSS | top memory consumers |
| `guest-hal.services` | systemctl (Linux) | which services aren't running |
| `guest-hal.boot-log` | dmesg / slog2 | kernel/boot faults |

vm2 (Linux) additionally exposes container probes: `container.ps`,
`container.storage` (podman system df), `container.images`, `container.info`.

A probe result is `{ ok, output, message }`. `ok:false` means the gather itself
failed (source unavailable / bad descriptor) — report the `message`, don't treat
it as data. `ok:true` with data is the real answer.

## Step 3 — smoke tests (§7.15 scripts) via the external tester
```bash
sovd-test-cli $S vm1 --tag smoke              # run the smoke subset, report verdicts
sovd-test-cli $S vm1 guest-hal.hsm            # run one test
sovd-test-cli $S vm1 --tag smoke -r report.json
```
The tester runs each test over SOVD, captures the run's log window via the
cursor bracket, and judges verdict + logs. Exit code IS the result: `0` all
passed, `1` a test failed, `2` couldn't run (setup/transport).

## Step 4 — logs (§7.21) when a probe points at one
```bash
sovd-cli $S logs vm1 --tail 100               # recent lines
sovd-cli $S logs vm1 --grep panic             # search
sovd-cli $S logs vm1 --since END-10m          # last 10 min of this boot
```

## The reasoning: turn probe output into a root cause
This is the skill's job — read the numbers, name the cause, propose the fix.

| Signal | Likely cause | Fix |
|---|---|---|
| `mem.available` tiny **and** a `shmem-usage` entry huge (e.g. `vfoo.log = 900 MiB`) | a runaway guest service spamming its RAM-backed log | restart/stop that service; factory-reset clears `/dev/shmem`. The log writer is bounded by `svclog` on current images — an unbounded one means a stale image. |
| `disk` at ~100% + big `log-usage` entry | `/var/log` filled | rotate/clear that log; check the writer's cap |
| a `services` unit not `running` | crashed/failed daemon | check `boot-log` + `logs --grep <svc>` for why |
| `boot-log` shows panic/OOM-kill | kernel/driver fault or the OOM above | correlate with `mem`/`shmem-usage` |
| `container.storage` huge / many `container.images` | podman image cache filling disk | prune images |
| flash failed `verify_part … out of memory` | memory pressure during verify | `mem` + `shmem-usage` show the pressure source directly — this is the incident this surface was built for |

Always report what you checked AND what you couldn't (a missing capability or an
`ok:false` probe) so the picture is honest.

## Vehicle-wide (the reason it's discovery-driven)
Point it at the gateway and walk sub-entities — the SAME steps, no new code:
```bash
sovd-cli $S list                              # or the gateway's /apps
# for each child that reports diagnostics: true, run Step 2
```
As more ECUs come online and expose a `diagnostics` capability (their own §7.9
data or a diag-agent-equivalent), this skill picks them up automatically. One
skill, whole vehicle.

## Notes / gotchas
- **Capabilities gate everything.** `diagnostics`/`scripts` are only `true` when
  the component config sets `diag_agent_url`/`test_agent_url` (guest VMs today:
  vm1, vm2). A `false` capability = the agent isn't wired, not that the guest is
  healthy.
- **Read-only.** Diagnostics probes never mutate; the tester's scripts CAN
  (they're gated dev/QA — don't run on a production vehicle unless intended).
- **Best-effort brackets.** A probe or log tip may be unavailable on some
  sources; the surface degrades to empty/`ok:false` rather than erroring.
```
