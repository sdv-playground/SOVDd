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
cycle are all valid. `x-update` is served beside `x-ota` as a set of aliases; the `x-ota` names
will be deprecated later.

## Verdict: `x-update-verdict`

Body: `{"verdict": "commit" | "rollback"}`. It comes in two standard shapes.

### Per update (lifecycle PUT)

`PUT {entity}/updates/{update_id}/x-update-verdict` decides this one update.

| answer | when |
|---|---|
| 202 + `Location: {entity}/updates/{update_id}/status` | accepted; the update moves to COMMITTING or ROLLING_BACK |
| 422 | `verdict` missing or not one of `commit`, `rollback` (400 for malformed JSON, 415 without `Content-Type: application/json`, as for every JSON body on this server) |
| 404 | unknown update |
| 409 `precondition-not-fulfilled` | the update is not awaiting a verdict; `commit` additionally requires the executor to report the new software running (state TRIAL) |

Served by this crate.

### Per node (operation)

`POST /vehicle/v1/operations/x-update-verdict/executions`, body
`{"verdict": "commit" | "rollback", "nonce"?: string}`, decides every component in the node's open
transaction, all or nothing. The answer is an ISO §7.14 OperationExecution:

```json
{
  "execution_id": "...",
  "operation_id": "x-update-verdict",
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

### Finish and invalidate

There is no x-update verb for either. Finish is the standard `DELETE {entity}/updates/{update_id}`
after a terminal state; invalidate is the same `DELETE` before `execute`.

## Status member

`GET {entity}/updates/{update_id}/status` carries, beside the standard Table 270 members:

```json
"x-update": { "state": "<State>", "reset_kind": "none" | "component" | "node" }
```

The state is total, and the standard `phase`/`status` in the same body agree with it:

| state | phase | status | meaning |
|---|---|---|---|
| READY | prepare | pending | registered; manifest and parts may be uploaded; `prepare` may be sent |
| PREPARING | prepare | inProgress | verifying and staging |
| PREPARED | prepare | completed | staged in the inactive bank; `execute` or `DELETE` |
| EXECUTING | execute | inProgress | armed; the restart named by `reset_kind` has not been observed yet |
| TRIAL | execute | inProgress | the new software runs; a verdict is awaited |
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
| true | `execute` stops at TRIAL; a verdict is required |
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
5. Read `x-update-state`.
6. If `reset_kind` is `node`: restart the node's entity (`PUT {node-entity}/status/restart`) if
   authorized, otherwise wait for someone else to.
7. When `rebooted_since_arming` is true and the updates report TRIAL: send one node verdict
   (all or nothing).
8. `DELETE` each update.

Per-update verdicts are for single-component executors and for `component` (local) resets.

## Aliases

| x-ota name | x-update equivalent | served by |
|---|---|---|
| `x-ota-commit`, `x-ota-rollback` (per update) | `PUT …/updates/{id}/x-update-verdict` | this crate |
| `x-ota-commit-trials`, `x-ota-rollback-trials` | node `x-update-verdict` operation | machine manager |
| `x-ota-update-state` | `x-update-state` | machine manager |
| `x-ota-reset-kind`, `x-ota-substate` | `x-update.reset_kind`, `x-update.state` | this crate |
| `?x-ota-control=orchestrated` | unchanged for now; the executor's `verdict` property replaces it later | this crate |
| `x-ota-force-rollback` | unchanged (admin operation, not lifecycle) | this crate |
| `x-ota-pull-update` | unchanged (admin operation, not lifecycle) | machine manager |

## Not in this version

- Executors that need a verdict refusing `automated` with 409.
- Listing held updates after a server restart; only the per-update path is served.
- Retiring the x-ota names.
