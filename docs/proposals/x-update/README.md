# x-update: counter-proposal

Draft 1, 2026-10-22. Files: `sovd-x-update.schema.json` (types), `x-update.openapi.json`
(paths). Written against ISO 17978-3 §7.18 (software update) and §7.19 (entity status and
restart). This directory is the reference contract; `sovd-schema-x-update-api` in
traton-one-os would mirror it. Nothing in it is specific to any executor, so openSOVD could
adopt it unchanged.

## What it keeps from the current x-update (traton-one-os 4e82789f, SOVDd PR #3)

- The name and the placement: `x-update` members nested in the standard registration, details
  and status bodies; verbs as `PUT {entity}/updates/{id}/x-update-<verb>`.
- `two_phase` at registration.
- The manifest as `bulk-data/manifest`: octet-stream, opaque to the server, identical re-upload
  accepted, different manifest refused, GET readback for reconnect.
- Discovery by `capabilities.software_update` on each entity, base URI from `/version-info`
  (traton !977, !986 change 1).
- Every verb answers 202 plus a Location to the status resource.

## What it changes

| | Current x-update | This proposal | Why |
|---|---|---|---|
| State enum | 16 values, "Native UpdateExecutor state", AUTOSAR UCM internals, none documented; TRATON's server emits `kActivated` spellings that violate its own schema | 11 lifecycle states, each documented, each tied to the standard `phase`/`status` pair; one spelling | A wire contract is not an executor's state machine. A client must be able to implement it without knowing UCM. |
| Reset | Absent. The dialect "observes restarts through status"; `!978` deletes the one restart call | `reset_kind` ∈ none, component, node on the status; the client issues the standard §7.19 restart; `ACTIVATING` holds until the new software is observed running | Without it no client can activate a banked image that needs a node restart. |
| Node-level facts | Absent (sumo serves them as `x-ota-update-state`) | `GET {node-entity}/data/x-update-state`: `node_boot_id`, in-flight updates, `reboot_owed`, `rebooted_since_arming` | One read to sequence a restart and the verdicts after it. Facts, not a veto. |
| Verbs | Four names, no paths, no preconditions | Four path items with preconditions stated as states only | A server never refuses a verdict on its own view of the outcome. |
| Reconnect | Server must keep the resource (TRATON executor persists; SOVDd loses it and invents `pending-trial-<component>`) | Registration with the same id adopts the executor's durable state; `adopted: true` reported; manifest readback served from the executor | The client's id is deterministic already. No server-side store, no special ids. |
| Base status for activated-uncommitted | `completed` (TRATON), `inProgress` (SOVDd) | `inProgress` | A plain SOVD client must not read "done" before the verdict. |
| Rollback outcome | `failed` with `sovd-server-failure` (TRATON) or `x-ota-verdict-rollback` (SOVDd) | `failed` with `update-rolled-back` | Neutral code, same on every server. |
| Manifest wording | "L2 SUIT envelope", "executor domain" | "the update's manifest, opaque to the server" | Vendor and format neutral. |
| Root schema | Required `registration_options`, `extension_verb`, `status` in one object | `$defs` only | The old root was a bag of definitions, not a message. |

## State table

`x-update.state` is total. The standard members in the same body must agree with it:

| state | phase | status | typical step | may send |
|---|---|---|---|---|
| READY | prepare | pending | | upload manifest, prepare, DELETE |
| PREPARING | prepare | inProgress | transferring, verifying, staging | |
| VALIDATED | prepare | completed | validated | execute, x-update-invalidate, DELETE |
| ACTIVATING | execute | inProgress | installing, awaiting-restart | restart (per reset_kind), x-update-rollback |
| ACTIVATED_UNCOMMITTED | execute | inProgress | awaiting-verdict | x-update-commit, x-update-rollback |
| COMMITTING | execute | inProgress | committing | |
| COMMITTED | execute | completed | committed | x-update-finish, DELETE |
| ROLLING_BACK | execute | inProgress | rolling-back, awaiting-restart | |
| ROLLED_BACK | execute | failed (`update-rolled-back`) | rolled-back | x-update-finish, DELETE |
| FAILED | prepare or execute | failed (standard error) | | x-update-rollback (execute only), x-update-finish, DELETE |
| FINISHED | unchanged | unchanged | finished | DELETE |

One-phase updates (no `two_phase`) go ACTIVATING, COMMITTING, COMMITTED without stopping.

## Reset and activation

1. After `prepare`, the status carries `reset_kind`.
2. The client sends `execute` to every update on the node. Each goes ACTIVATING.
3. The client restarts: for `component`, `PUT {entity}/status/restart` on that entity; for
   `node`, one `PUT {node-entity}/status/restart`, or any out-of-band restart (ignition cycle,
   workshop power cycle). Authorization of the restart is the server's §5.4.4 policy, not this
   contract's: a workshop tester may be allowed to restart a node that an onboard agent may not.
4. The client reads `x-update-state` until `rebooted_since_arming` is true (for `node`) and the
   updates report ACTIVATED_UNCOMMITTED.
5. The client decides, per update, from what it can observe: the entity's reported version,
   its health, the manifest it uploaded. Then `x-update-commit` or `x-update-rollback` on each.
6. `x-update-finish` on each, then DELETE.

What the server guarantees: `reboot_owed` and the armed boot id survive the restart and are
cleared only by a verdict; `ACTIVATING` is never reported as `ACTIVATED_UNCOMMITTED` before the
new software is observed running. What the server does not do: refuse a commit because it has
not seen a boot witness of its own. If a client commits a bank that is not running, the commit
either fails honestly (FAILED with an error) or the executor's own rules apply; the server
reports the result.

## Reconnect

The client registers with a deterministic id. On a re-registration with an id the executor
still holds (VALIDATED to ROLLED_BACK), the server rebuilds the resource from the executor's
durable state and answers 201 with `adopted: true`. The manifest readback returns the manifest
the executor holds, so the client can verify it byte for byte. The SOVD server itself keeps no
state on disk; the executor is the durable party. This matches TRATON's architecture (stateless
server, persisting executor) and gives SOVDd the same property via the backend trait.

## Migration

- SOVDd: emit the 11 states; add `reset_kind`, `two_phase`, `adopted` to the status member;
  serve `data/x-update-state` from the backend's node facts (sumo already reports them);
  adopt-on-register via a backend `held_update()` returning id digest, two_phase and manifest;
  drop `pending-trial-*`; finish and invalidate answer 202 plus Location; rollback uses
  `update-rolled-back`; auth maps finish and invalidate to the verdict capability.
- sumo-machine-manager: store a 16-byte digest of the update id in the NV update session (as
  `armed_boot_id` is stored); expose `reset_kind` (already `ResetKind`), `node_boot_id`,
  `reboot_owed`, `rebooted_since_arming` (already in `x-ota-update-state`) under the x-update
  name beside the x-ota one.
- TRATON SovdServerVehicleOs: map UCM ordinals to the 11 states (kActivating → ACTIVATING,
  kActivated → ACTIVATED_UNCOMMITTED, kProcessing/kTransferringMeta/kValidating → PREPARING,
  kProcessed → READY, kCleaningUp → the terminal state until done, then FINISHED); report
  inProgress while activated-uncommitted; drop `entity` and `registry_degraded` from the
  `x-update` member (they may live under a vendor member); stop emitting `k` names.
- lib-orchestrate: read `reset_kind`; after all executes on a node issue the component or node
  restart (the call !978 deletes, kept); poll `x-update-state` for `rebooted_since_arming`;
  then commit each. Drop the dual-spelling decoder. x-ota stays until this is in; !978 is the
  last step.

## Deprecation of x-ota

Once SOVDd and sumo serve this contract, the x-ota members that x-update now covers can be
retired in order: `x-ota-commit` / `x-ota-rollback` (per-update verbs, same semantics),
`x-ota-reset-kind` (now `reset_kind`), `x-ota-update-state` (now `x-update-state`),
`x-ota-commit-trials` / `x-ota-rollback-trials` (per-update verbs after one restart make the
node-level verdict unnecessary). `x-ota-pull-update` and `x-ota-force-rollback` are not
update-lifecycle verbs and stay where they are.

## Open questions for the review

1. Should `FAILED` in the execute phase always be followed by an explicit `x-update-rollback`,
   or may a server auto-revert and report ROLLED_BACK directly? The draft allows both; the
   state tells the client which happened.
2. `x-update-state` lives on the node entity. On a server with one entity per node that is the
   root component; on a gateway that proxies several nodes it is one resource per node entity.
   Is a vehicle-level aggregate wanted?
3. Manifest size limit: the current schema fixes 1 MiB. The draft leaves it to the server
   (413) and suggests advertising it in the entity's capabilities. Fix a number?
