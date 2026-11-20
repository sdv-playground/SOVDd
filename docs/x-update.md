# x-update

The contract for the `x-update` vendor extensions to ISO 17978-3 §7.18 (software update) and
§7.19 (entity status and restart). Everything here sits beside the standard members and verbs;
nothing replaces them.

The same contract as OpenAPI: [`x-update/x-update.openapi.json`](x-update/x-update.openapi.json).

## Principles

The node reports facts and never vetoes; the client decides the verdict. Whether a verdict is
awaited at all is a property of the executor, not a choice the client makes per request. The
restart that activates new software is the standard §7.19 `PUT {entity}/status/restart`, gated by
the server's authorization scope; this contract does not say who issues it. An orchestrator
holding a suitable token, a workshop tester, the vehicle's state management and an ignition
cycle are all valid. The client owns the update ids. Every verb is explicit and
addressed to the update's executor. `x-update` is served beside `x-ota` as a set of aliases; the
`x-ota` names will be deprecated later.

## Entities

The orchestrator addresses updates on the node level. `{entity}` in this document is the node — the
server's root scope — so the collection is `/updates`, and every registration names the executor
the update is for as `target`, one of the executors the node lists as `x-update.update-targets`
in its capability document (`GET /`, beside the `updates` link). A node without update executors
advertises neither. Each executor is an entity of its own — a top-level component, or a
subcomponent of the node where the server serves an entity tree — whose document reports the
standard `software_update` capability and `capabilities.x-update.verdict`; the `apps` executor,
where a node has one, is an update target like any other. A server may also offer an executor's
own collection, `{executor}/updates`, for entity-level updates; the lifecycle below is the same
there, with `target` naming that entity. An executor is IDLE while it holds no update.

## Update ids and listing

The client gives the id in a registration (`POST {entity}/updates`, `id`); the server keeps it
verbatim and never replaces or derives an id from it. Under `/v1`, a registration without `id`
gets a server-created UUID (ISO 17978-3 §7.18.9), returned in `Location`, `id` and `update_id`;
`/vehicle/v1` refuses it with 400. An empty `id` is refused with 400 on both trees.

`GET {entity}/updates` lists the updates the server holds, by the client's id. A server rebuilds
that list from its executor, so a client that lost its state finds its updates by id after a
server or executor restart; SOVDd does, merging the updates its executor holds into the ones it
has registered. An update only the executor holds is described (`GET {entity}/updates/{id}`, with
the id as its name and the size of its held parts), reports its status (the held state, per the
table under "Status member") and lists its held parts; a verdict, `prepare` or `execute` on it
answers 404 until it is adopted (below).

A registration with an id the executor still holds adopts that update instead of starting a new
one: 201, with `x-update.adopted: true` in the details and status bodies. A registration with
the same id but a different manifest is refused with 409.

How a client recovers is its own choice; recovering from the node's facts instead of by id is
equally valid. The listing and adopting are required regardless: without them, a client that lost
its state cannot find and settle its updates, and those recovery cases cannot be tested.

Not implemented yet: SOVDd does not adopt.

## Bulk data

An update's parts are uploaded to its bulk-data collection, `{entity}/updates/{update_id}/bulk-data`
(also served under the node-level collection). The collection is a documented extension: ISO
17978-3 §7.20 places bulk-data on entities, not on an update package; its upload and descriptor
forms are used here unchanged.

- `POST {entity}/updates/{update_id}/bulk-data` is the upload (§7.20, Tables 301-303). The body is
  the part's bytes, streamed as is (not parsed as multipart). `Content-Disposition` is required
  and names the part: its `filename` parameter, else its `name`
  (`form-data; name="manifest"; filename="manifest"`). The name is the part's URL segment: not
  empty, no `/`, not `.` or `..`. A missing header or an unusable name answers 400
  `incomplete-request`. `Content-Type` is recorded as the part's mimetype (default
  `application/octet-stream`). The answer is 201 with `Location:
  {entity}/updates/{update_id}/bulk-data/{part}`, an `ETag` of the part's SHA-256 and the body
  `{"id": "<part>"}`. Uploading a part of the same name again replaces it (201 again, same
  `Location`). Parts are accepted while the update is registered or uploading (409 otherwise).
- `PUT {entity}/updates/{update_id}/bulk-data/{part_id}` is the legacy alias of the `POST`, the
  part named by the path; it records `Content-Type` alike and answers 201, the `ETag` and
  `{part_id, size, sha256, href}`. It is scheduled for deprecation.
- `GET {entity}/updates/{update_id}/bulk-data/{part_id}` reads a part back as it was uploaded:
  200 with its bytes, its recorded `Content-Type` and the `ETag`; 404 while the update has no such
  part (or there is no such update). A client retrying an upload checks with it whether its last
  one landed: lib-orchestrate reads `{update}/bulk-data/manifest` before it uploads the L2, and
  accepts only 200 (the same bytes) or 404. SOVD keeps the bytes of an update's parts up to 1 MiB
  together, enough for a manifest; a part beyond that, whose bytes only the executor holds,
  answers 501.

`GET {entity}/updates/{update_id}/bulk-data` items carry, beside `part_id`, `size`, `sha256` and
`href`, a `present` flag: whether the executor already holds the part. It is matched by name (the
part id is the held part's name or `#` and that name), not by hash: `sha256` is of the uploaded
part, the executor's digest is of the stored one. A part listed for an update only the executor
holds is `present`, carries the executor's digest and has no `href`. Under `/v1` each item also
carries the §7.20 Table 298 `BulkDataDescriptor` members `id` (the part name), `mimetype` (the
recorded `Content-Type`; `application/octet-stream` when none was sent, or for a held part),
`size`, `hash` (the `sha256` value) and `hash_algorithm` (`sha256`). The part metadata is kept in the
server's update store, so after a server restart a held part is listed with the executor's
digest and the default mimetype.

## Node-level collection

`{base}/updates` on the server root (ISO 17978-3 §7.18.1.1 names it the primary location) is an
alias of the per-component collections, which remain and are unchanged. The root document
(`GET {base}/`) references it as `updates` when at least one component accepts software
updates.

- `POST {base}/updates`, body `{"id"?, "target", "manifest"?}`, registers on the component
  `target`, which is required and must be one of the root document's `x-update.update-targets`
  (400 otherwise). The id rules are those of the component collection. An id registered on, or
  held by the executor of, another component answers 409 `update-process-in-progress`; on
  `target` itself the answer is that of `POST {base}/components/{target}/updates` (a settled or
  untouched registration is replaced, one in progress answers 409).
- Every per-update verb (`GET`, `DELETE`, `bulk-data`, `prepare`, `execute`, `automated`,
  `status`, the verdicts) is served at `{base}/updates/{update_id}/…` and acts on the component
  the update is registered on, or, when none has it registered, the one whose executor holds it.
  An unknown id answers 404. An id registered on several components (through their own
  collections) answers 409 `precondition-not-fulfilled`; address it on a component collection.
- The `Location` and hrefs a node-level request is answered with point into `{base}/updates`.
- `GET {base}/updates` lists the ids of every update target the client may access.
- The client's component scope applies to the update's component as on a component route: 403
  under `/v1`, 401 under `/vehicle/v1`.
- Under `/v1`, `GET …/updates/{update_id}` and `…/status` carry `x-update.target`, the component
  the update is registered on, and `x-update.verdict_scope` (see "Status member"), on both
  collections; `/vehicle/v1` bodies are unchanged.
- `x-ota-force-rollback` is component-rooted and has no node-level form.

## Verdicts: `x-update-commit`, `x-update-rollback`

`PUT {entity}/updates/{update_id}/x-update-commit` and `…/x-update-rollback` decide this one
update. No body.

| verb | accepted in |
|---|---|
| `x-update-commit` | EXECUTED; again in COMMITTED, where it changes nothing |
| `x-update-rollback` | EXECUTING, EXECUTED; again in ROLLED_BACK, where it changes nothing |

| answer | when |
|---|---|
| 202 + `Location: {entity}/updates/{update_id}/status` | accepted; the update moves to COMMITTING or ROLLING_BACK |
| 404 | unknown update |
| 409 `precondition-not-fulfilled` | not in a state the verb is accepted in |

Per-update verdicts are for single-component executors and for `component` (local) resets.
Updates that one node restart activates (`reset_kind` `node`) form one node transaction, settled
all or nothing by the node operations below. This contract gives a per-update verdict on a member
of a node transaction no meaning: a client does not send one, and a server may refuse it with 409
`precondition-not-fulfilled`.

This crate refuses them under `/v1`: on a component whose backend reports its updates as members
of a node transaction, `x-update-commit`, `x-update-rollback`, `x-ota-commit`, `x-ota-rollback`
and `x-ota-force-rollback` answer 409 `precondition-not-fulfilled` before any state check, with a
message naming the node operations as the settle path. Under `/vehicle/v1` they are accepted as
before, for existing clients. Under `/v1`, `x-update.verdict_scope` `node` on the update and on
the component's capabilities tells a client so before it sends a verdict.

Served by this crate, which forwards the verdict to the update's executor and reports what it
answers.

### Transactions

Updates whose banks one executor switches together (one boot vector, all or nothing) form a
transaction. A transaction need not span the node: an ECU whose SoCs boot from separate boot
vectors has one transaction per vector. Updates in different transactions settle independently;
keeping them consistent with each other is the client's job. The node transaction above is the
one whose members report `verdict_scope` `node`.

Each update reports its own state, as it actually is; the server does not align the members of
a transaction. When the executor commits or rolls back, by a verdict or on its own (e.g. the boot
stage after failed trial boots), each update it commits reports COMMITTING, then COMMITTED, and
each update it rolls back reports ROLLING_BACK, then ROLLED_BACK.

Keeping a transaction atomic is the backend's part (in sumo, the machine manager).

### Per node (operations)

`POST /vehicle/v1/operations/x-update-commit/executions` and `…/x-update-rollback/executions`,
body `{"nonce"?: string}`, commit or roll back every component in the node's open transaction,
all or nothing. The answer is an ISO §7.14 OperationExecution:

```json
{
  "execution_id": "...",
  "operation_id": "x-update-commit",
  "status": "completed",
  "result": { "committed": ["<component id>", "..."], "skipped": ["..."] },
  "nonce": "<echoed if sent>"
}
```

`result` carries `committed` for a commit or `rolled_back` for a rollback, plus `skipped`.

| answer | when |
|---|---|
| 200 | the verdict was applied |
| 409 | every refusal was "busy" (retry later) |
| 500 | any other refusal |

Served by the embedding machine manager, not by this crate.

## Delete

The standard `DELETE {entity}/updates/{update_id}` removes an update. Before `execute`
(REGISTERED, PREPARING, PREPARED) it discards what the executor staged, aborting a running
`prepare`; the software in use is never touched. Once the update has settled (COMMITTED,
ROLLED_BACK or FAILED) it removes the record. It is synchronous: 204 once the update is gone.
There is no finish verb and no revert verb: DELETE is both. Under `/v1`, a `DELETE` while the
update is still running in phase execute (EXECUTING, EXECUTED, COMMITTING, ROLLING_BACK) answers
405 (ISO 17978-3 §7.18.8); `/vehicle/v1` deletes it, aborting the running step.

| answer | when |
|---|---|
| 204 | removed |
| 404 | unknown update |
| 405 `method-not-allowed` | under `/v1`, in phase execute and not settled: wait, or send the verdict first |

An executor still busy answers the next `prepare` or `execute` with 409, as the standard does.

## Lifecycle

| Method | Path | Verb / Notes | From states | To state |
|---|---|---|---|---|
| POST | `{entity}/updates` | Register; the client gives the id (under `/v1` the server creates one when it is missing). Re-registering an id the executor holds adopts it (`adopted: true`) | IDLE | REGISTERED |
| GET | `{entity}/updates` | List held updates by the client's id; rebuilt from the executor after a restart | — | — |
| GET | `{entity}/updates/{id}` | Descriptor | — | — |
| DELETE | `{entity}/updates/{id}` | Remove an update. Before execute it discards what was staged; the software in use is not touched. Under `/v1` refused (405) while running in phase execute | REGISTERED, PREPARING, PREPARED, COMMITTED, ROLLED_BACK, FAILED | — |
| POST | `{entity}/updates/{id}/bulk-data` | Upload a part (`PUT …/bulk-data/{part_id}` is the legacy alias) | REGISTERED | REGISTERED |
| GET | `{entity}/updates/{id}/bulk-data` | List parts; `present` tells if the executor holds each one | — | — |
| GET | `{entity}/updates/{id}/bulk-data/{part_id}` | Read a part back as uploaded (up to 1 MiB of an update's parts together) | — | — |
| PUT | `{entity}/updates/{id}/prepare` | §7.18.5 | REGISTERED | PREPARING → PREPARED, PREPARING → FAILED |
| PUT | `{entity}/updates/{id}/execute` | §7.18.6 | PREPARED | EXECUTING → EXECUTED, EXECUTING → ROLLING_BACK → ROLLED_BACK |
| PUT | `{entity}/updates/{id}/x-update-commit` | Commit the trial (`verdict_scope` `update`) | EXECUTED | COMMITTING → COMMITTED |
| PUT | `{entity}/updates/{id}/x-update-rollback` | Roll back the trial (`verdict_scope` `update`) | EXECUTING, EXECUTED | ROLLING_BACK → ROLLED_BACK |
| POST | `/operations/x-update-commit/executions`, `…/x-update-rollback/executions` | Settle a node transaction (`verdict_scope` `node`) | EXECUTED (every member) | COMMITTED, ROLLED_BACK |
| GET | `{entity}` | Component body with `capabilities.x-update.verdict` | — | — |

| State | Transient / Settled | What the client may do |
|---|---|---|
| IDLE | Settled | register an update |
| REGISTERED | Settled | Upload parts, prepare, DELETE |
| PREPARING | Transient | Wait, or DELETE |
| PREPARED | Settled (pre-execute) | execute, DELETE |
| EXECUTING | Transient | Wait; x-update-rollback accepted |
| EXECUTED | Settled (awaiting verdict) | x-update-commit, x-update-rollback, or the node operation |
| COMMITTING | Transient | Wait |
| COMMITTED | Settled | DELETE |
| ROLLING_BACK | Transient | Wait |
| ROLLED_BACK | Settled | DELETE |
| FAILED | Settled | DELETE |

## Status member

`GET {entity}/updates/{update_id}/status` carries, beside the standard Table 270 members:

```json
"x-update": { "state": "<State>", "reset_kind": "none" | "component" | "node", "target": "<component id>", "verdict_scope": "update" | "node", "adopted": true }
```

`target` and `verdict_scope` are served under `/v1` only (see "Node-level collection"). `adopted`
is present, and true, on an update a registration adopted.

The states are named after the verbs that lead to them. The state is total, and the standard
`phase`/`status` in the same body agree with it:

| state | phase | status | meaning |
|---|---|---|---|
| IDLE | — | — | the entity holds no update: not an update's state, but the one its entity reports; `POST` registers one |
| REGISTERED (`/v1`), READY (`/vehicle/v1`) | prepare | pending | registered; manifest and parts may be uploaded; `prepare` may be sent |
| PREPARING | prepare | inProgress | verifying and staging |
| PREPARED | prepare | completed | staged in the inactive bank; `execute` or `DELETE` |
| EXECUTING | execute | inProgress | armed; the restart named by `reset_kind` has not been observed yet |
| EXECUTED | execute | inProgress | the new software runs; a verdict is awaited |
| COMMITTING | execute | inProgress | commit accepted and running |
| COMMITTED | execute | completed | permanent |
| ROLLING_BACK | execute | inProgress | rollback accepted and running (the client's, or the executor's own, e.g. after failed trial boots) |
| ROLLED_BACK | execute | failed (`error_code` `x-ota-verdict-rollback` today) | the previous software is in effect again |
| FAILED | prepare or execute | failed | see the standard error object |

`reset_kind`:

| value | meaning |
|---|---|
| none | the new software runs without a restart |
| component | a standard restart of this entity activates it |
| node | one restart of the node's entity activates every update on the node that reports `node` |

`verdict_scope`:

| value | meaning |
|---|---|
| update | the update is settled by its own `x-update-commit` / `x-update-rollback` verbs |
| node | the update is a member of a node transaction, settled only by the node operations; a per-update verdict is refused with 409 |

`reset_kind` says what must restart to activate the update; `verdict_scope` says what settles it.
They are independent: a VM image in the node's shared boot vector reports `reset_kind`
`component` and `verdict_scope` `node`.

The x-ota member `x-ota-reset-kind` keeps its old vocabulary (`none`, `local`,
`requires_ecu_reset`) and is served beside it.

## Capability flag

`GET {entity}` (component body) carries:

```json
"capabilities": { "...": "...", "x-update": { "verdict": true, "verdict_scope": "update" | "node" } }
```

`verdict_scope` is served under `/v1` only, with the values of the status member.

| `verdict` | meaning |
|---|---|
| true | `execute` stops at EXECUTED; a verdict is required |
| false | `execute` commits on its own |
| absent | the server cannot tell |

The client does not choose whether a verdict is needed; there is no request option for it. The
standard `automated` verb is the way to ask the executor to decide for itself.

## Node state: `x-update-state`

`GET /vehicle/v1/data/x-update-state`, served by the machine manager:

```json
{
  "phase": "Idle" | "Staging" | "RebootPending" | "Trial",
  "components": ["..."],
  "reboot_owed": ["..."],
  "rebooted_since_arming": true | false | null,
  "node_boot_id": "<string>" | null
}
```

The same body as `x-ota-update-state`, plus `node_boot_id`. One read is enough to sequence the
restart and the verdict.

## Orchestrator sequence

1. Register one update per executor on the node's collection (`POST /updates`, `target` naming the executor).
2. Upload the manifest and parts.
3. `PUT prepare` on each; wait for PREPARED. If any fails, or the client decides not to go on:
   `DELETE` each update not yet executed.
4. `PUT execute` on each; each goes EXECUTING.
5. The restart named by `reset_kind` happens: whoever is authorized issues
   `PUT {entity}/status/restart`, or a power cycle does it. The client awaits EXECUTED.
6. When the updates report EXECUTED: send the verdict and await COMMITTED or ROLLED_BACK. A
   single-component or `component` update takes `x-update-commit` / `x-update-rollback` on the
   update; a node transaction takes the node operation once, for all of its updates. A rollback
   may also be sent while an update is still EXECUTING.
7. `DELETE` each update.

A client that restarted lists `GET {entity}/updates`, recognises its ids, and continues from each
update's state.

## Aliases

| x-ota name | x-update equivalent | served by |
|---|---|---|
| `x-ota-commit`, `x-ota-rollback` (per update) | `PUT …/updates/{id}/x-update-commit`, `…/x-update-rollback` | this crate |
| `x-ota-commit-trials`, `x-ota-rollback-trials` | node `x-update-commit`, `x-update-rollback` operations | machine manager |
| `x-ota-update-state` | `x-update-state` | machine manager |
| `x-ota-reset-kind`, `x-ota-substate` | `x-update.reset_kind`, `x-update.state` | this crate |
| `?x-ota-control=orchestrated` | unchanged for now; the executor's `verdict` property replaces it later | this crate |
| `x-ota-force-rollback` | unchanged (admin operation, not lifecycle) | this crate |
| `x-ota-pull-update` | unchanged (admin operation, not lifecycle) | machine manager |

## Not in this version

- Executors that need a verdict refusing `automated` with 409.
- Retiring the x-ota names.
- Executor-side recording of the update id (which the listing needs).
