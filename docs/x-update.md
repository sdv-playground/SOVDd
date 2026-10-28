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

The client gives the id in every registration (`POST {entity}/updates`, `id`); a registration
without one is refused with 400. The server keeps it verbatim and never assigns, replaces or
derives an id of its own.

`GET {entity}/updates` lists the updates the server holds, by the client's id. A server rebuilds
that list from its executor, so a client that lost its state finds its updates by id after a
server or executor restart; SOVDd does, merging the updates its executor holds into the ones it
has registered. An update only the executor holds is described (`GET {entity}/updates/{id}`, with
the id as its name and the size of its held parts), reports its status (the held state, per the
table under "Status member") and lists its held parts; a verdict, `prepare` or `execute` on it
answers 404 until it is adopted (below).

`GET {entity}/updates/{update_id}/bulk-data` items carry, beside `part_id`, `size`, `sha256` and
`href`, a `present` flag: whether the executor already holds the part. It is matched by name (the
part id is the held part's name or `#` and that name), not by hash: `sha256` is of the uploaded
part, the executor's digest is of the stored one. A part listed for an update only the executor
holds is `present`, carries the executor's digest and has no `href`.

A registration with an id the executor still holds adopts that update instead of starting a new
one: 201, with `x-update.adopted: true` in the details and status bodies. A registration with
the same id but a different manifest is refused with 409.

How a client recovers is its own choice; recovering from the node's facts instead of by id is
equally valid. The listing and adopting are required regardless: without them, a client that lost
its state cannot find and settle its updates, and those recovery cases cannot be tested.

Not implemented yet: SOVDd does not adopt.

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

A verdict is valid for every update, whatever restart activates it.

### Transactions

Updates whose banks one executor switches together (one boot vector, all or nothing) form a
transaction. A transaction need not span the node: an ECU whose SoCs boot from separate boot
vectors has one transaction per vector. Updates in different transactions settle independently;
keeping them consistent with each other is the client's job.

Each update reports its own state, as it actually is; the server does not align the members of
a transaction. When the executor commits or rolls back, by a verdict or on its own (e.g. the boot
stage after failed trial boots), each update it commits reports COMMITTING, then COMMITTED, and
each update it rolls back reports ROLLING_BACK, then ROLLED_BACK.

The server checks nothing across members: a verdict on a member is accepted or refused by that
member's own state, as above. What to send, and when, is the client's decision; it may send the
verdict to every member, at once or one after another, and await each one's settled state.

Served by this crate. Keeping a transaction atomic is the backend's part (in sumo, the machine
manager); not implemented yet.

### Per node (operations)

`POST /vehicle/v1/operations/x-update-commit/executions` and `…/x-update-rollback/executions`,
body `{"nonce"?: string}`, commit or roll back every component in the node's open transaction,
all or nothing, beside the per-update verbs: the same result as a per-update verdict on any one
of its updates. The answer is an ISO §7.14
OperationExecution:

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

The standard `DELETE {entity}/updates/{update_id}` removes an update. It is accepted in REGISTERED and
PREPARED, before `execute`, where it discards what the executor staged and the software in use is
never touched, and once the update has settled (COMMITTED, ROLLED_BACK or FAILED). It is
synchronous: 204 once the update is gone. There is no finish verb and no revert verb: DELETE is
both.

| answer | when |
|---|---|
| 204 | removed |
| 404 | unknown update |
| 409 `precondition-not-fulfilled` | PREPARING, EXECUTING, EXECUTED, COMMITTING or ROLLING_BACK: wait, or send the verdict first |

An executor still busy answers the next `prepare` or `execute` with 409, as the standard does.

## Lifecycle

| Method | Path | Verb / Notes | From states | To state |
|---|---|---|---|---|
| POST | `{entity}/updates` | Register; client gives the id. Re-registering an id the executor holds adopts it (`adopted: true`) | IDLE | REGISTERED |
| GET | `{entity}/updates` | List held updates by the client's id; rebuilt from the executor after a restart | — | — |
| GET | `{entity}/updates/{id}` | Descriptor | — | — |
| DELETE | `{entity}/updates/{id}` | Remove an update. Before execute it discards what was staged; the software in use is not touched. Refused (409) while transient or awaiting a verdict | REGISTERED, PREPARED, COMMITTED, ROLLED_BACK, FAILED | — |
| PUT | `{entity}/updates/{id}/bulk-data/{part_id}` | Upload manifest or payload | REGISTERED | REGISTERED |
| GET | `{entity}/updates/{id}/bulk-data` | List parts; `present` tells if the executor holds each one | — | — |
| PUT | `{entity}/updates/{id}/prepare` | §7.18.5 | REGISTERED | PREPARING → PREPARED, PREPARING → FAILED |
| PUT | `{entity}/updates/{id}/execute` | §7.18.6 | PREPARED | EXECUTING → EXECUTED, EXECUTING → ROLLING_BACK → ROLLED_BACK |
| PUT | `{entity}/updates/{id}/x-update-commit` | Commit the trial | EXECUTED | COMMITTING → COMMITTED |
| PUT | `{entity}/updates/{id}/x-update-rollback` | Roll back the trial | EXECUTING, EXECUTED | ROLLING_BACK → ROLLED_BACK |
| GET | `{entity}` | Component body with `capabilities.x-update.verdict` | — | — |

| State | Transient / Settled | What the client may do |
|---|---|---|
| IDLE | Settled | updates |
| REGISTERED | Settled | Upload parts, prepare, DELETE |
| PREPARING | Transient | Wait |
| PREPARED | Settled (pre-execute) | execute, DELETE |
| EXECUTING | Transient | Wait; x-update-rollback accepted |
| EXECUTED | Settled (awaiting verdict) | x-update-commit, x-update-rollback |
| COMMITTING | Transient | Wait |
| COMMITTED | Settled | DELETE |
| ROLLING_BACK | Transient | Wait |
| ROLLED_BACK | Settled | DELETE |
| FAILED | Settled | DELETE |

## Status member

`GET {entity}/updates/{update_id}/status` carries, beside the standard Table 270 members:

```json
"x-update": {
  "state": "<State>",
  "adopted": true
}
```

`adopted` is present, and true, on an update a registration adopted. The states are named after the verbs that lead to
them. The state is total, and the standard
`phase`/`status` in the same body agree with it:

| state | phase | status | meaning |
|---|---|---|---|
| IDLE | — | — | the entity holds no update: not an update's state, but the one its entity reports; `POST` registers one |
| REGISTERED | prepare | pending | registered; manifest and parts may be uploaded; `prepare` may be sent |
| PREPARING | prepare | inProgress | verifying and staging |
| PREPARED | prepare | completed | staged in the inactive bank; `execute` or `DELETE` |
| EXECUTING | execute | inProgress | armed; the new software does not run yet (e.g. a restart is still to come) |
| EXECUTED | execute | inProgress | the new software runs; a verdict is awaited |
| COMMITTING | execute | inProgress | commit accepted and running |
| COMMITTED | execute | completed | permanent |
| ROLLING_BACK | execute | inProgress | rollback accepted and running (the client's, or the executor's own, e.g. after failed trial boots) |
| ROLLED_BACK | execute | failed | the previous software is in effect again |
| FAILED | prepare or execute | failed | see the standard error object |

## Capability flag

`GET {entity}` (component body) carries:

```json
"capabilities": { "...": "...", "x-update": { "verdict": true } }
```

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
5. The restart happens (a power cycle, or whoever is authorized issues
   `PUT {entity}/status/restart`); the client awaits EXECUTED.
6. When the updates report EXECUTED: send each its verdict (`x-update-commit` or
   `x-update-rollback`, or the node operation), and await COMMITTED or ROLLED_BACK. A rollback
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
| `x-ota-reset-kind` | unchanged; not part of x-update | this crate |
| `x-ota-substate` | `x-update.state` | this crate |
| `?x-ota-control=orchestrated` | unchanged for now; the executor's `verdict` property replaces it later | this crate |
| `x-ota-force-rollback` | unchanged (admin operation, not lifecycle) | this crate |
| `x-ota-pull-update` | unchanged (admin operation, not lifecycle) | machine manager |

## Not in this version

- Executors that need a verdict refusing `automated` with 409.
- Retiring the x-ota names.
- Executor-side recording of the update id (which the listing needs).
