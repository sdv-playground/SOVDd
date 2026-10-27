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

## Revert: `x-update-revert`

`PUT {entity}/updates/{update_id}/x-update-revert`, no body, discards an update before `execute`:
the executor drops what it staged, and the software in use is never touched. It is accepted in
READY, PREPARING, PREPARED, and FAILED before `execute`; again in REVERTED, where it changes
nothing. The update goes through REVERTING to REVERTED, which the client awaits.

| answer | when |
|---|---|
| 202 + `Location: {entity}/updates/{update_id}/status` | accepted; the update moves to REVERTING |
| 404 | unknown update |
| 409 `precondition-not-fulfilled` | `execute` was already sent |

Not implemented yet in SOVDd.

## Delete

Once an update has settled (COMMITTED, ROLLED_BACK, REVERTED or FAILED), the client removes it
with the standard `DELETE {entity}/updates/{update_id}`. There is no finish verb. An executor still busy answers the next `prepare` or
`execute` with 409, as the standard does.

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
| READY | prepare | pending | registered; manifest and parts may be uploaded; `prepare` may be sent |
| PREPARING | prepare | inProgress | verifying and staging |
| PREPARED | prepare | completed | staged in the inactive bank; `execute` or `x-update-revert` |
| REVERTING | prepare | inProgress | revert accepted; the executor discards what it staged |
| REVERTED | prepare | failed | nothing staged any more; the software in use was never touched |
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

1. Register one update per component (`POST {entity}/updates`).
2. Upload the manifest and parts.
3. `PUT prepare` on each; wait for PREPARED. If any fails, or the client decides not to go on:
   `x-update-revert` on each update not yet executed, await REVERTED, then `DELETE`.
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
