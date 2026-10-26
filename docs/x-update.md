# x-update

The contract for the `x-update` vendor extensions to ISO 17978-3 §7.18 (software update) and
§7.19 (entity status and restart). Everything here sits beside the standard members and verbs;
nothing replaces them.

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

`GET {entity}/updates` lists every update the executor holds, by the client's id, including
updates registered before a server or executor restart. A client that restarted without local
state finds its updates there and recognises them by their ids.

SOVDd lists the updates it holds while it runs; keeping them over a server restart is not
implemented yet.

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

A verdict is valid for every `reset_kind`, `node` included. Updates that one node restart
activates (`reset_kind` `node`) form one node transaction, and the server keeps it atomic: a
commit on any of them commits all of them, a rollback on any of them rolls back all of them, and
the others then report COMMITTED or ROLLED_BACK on their own. A commit is accepted only once
every update of the transaction reports EXECUTED. So a client may send the commit to every
update, at once or one after another, and await each one's settled state; the node transaction
never ends half committed.

Served by this crate; keeping a node transaction atomic is its backend's part (in sumo, the
embedding machine manager).

### Per node (operations)

`POST /vehicle/v1/operations/x-update-commit/executions` and `…/x-update-rollback/executions`,
body `{"nonce"?: string}`, commit or roll back every component in the node's open transaction,
all or nothing: the same result as the per-update verbs on any one of its updates. The answer is an ISO §7.14
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

## Delete and executor state

Once an update has settled (COMMITTED, ROLLED_BACK or FAILED), the client removes it with the
standard `DELETE {entity}/updates/{update_id}`; before `execute`, the same `DELETE` discards a
prepared update. There is no finish verb.

The executor then cleans up. Its own state is on the component body, `GET {entity}`:

```json
"x-update": { "state": "IDLE" | "BUSY" }
```

| state | meaning |
|---|---|
| IDLE | the executor holds no update and has finished cleaning up; it takes a new registration |
| BUSY | it holds an update, or is still cleaning up after a `DELETE` |

The client awaits IDLE after its `DELETE`s, before it considers the rollout done.

## Status member

`GET {entity}/updates/{update_id}/status` carries, beside the standard Table 270 members:

```json
"x-update": { "state": "<State>", "reset_kind": "none" | "component" | "node" }
```

The states are named after the verbs that lead to them. The state is total, and the standard
`phase`/`status` in the same body agree with it:

| state | phase | status | meaning |
|---|---|---|---|
| READY | prepare | pending | registered; manifest and parts may be uploaded; `prepare` may be sent |
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

The x-ota member `x-ota-reset-kind` keeps its old vocabulary (`none`, `local`,
`requires_ecu_reset`) and is served beside it.

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
3. `PUT prepare` on each; wait for PREPARED.
4. `PUT execute` on each; each goes EXECUTING.
5. The restart named by `reset_kind` happens (a power cycle, or whoever is authorized issues
   `PUT {entity}/status/restart`). The client does not watch for it; it awaits EXECUTED.
6. When the updates report EXECUTED: send each its verdict (`x-update-commit` or
   `x-update-rollback`, or the node operation), and await COMMITTED or ROLLED_BACK. A rollback
   may also be sent while an update is still EXECUTING.
7. `DELETE` each update; await each executor's IDLE.

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
