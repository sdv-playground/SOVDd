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
answers 404 until it is adopted (see "Not in this version").

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
- `GET {entity}/updates/{update_id}/bulk-data/{part_id}` (the `Location` of the upload) reads a
  part back, under `/v1` only (the legacy `/vehicle/v1` tree is frozen and answers 405): 200 with
  the bytes as uploaded, streamed from the backend, `Content-Type` the recorded mimetype
  (`application/octet-stream` when none was sent), `Content-Length` the part's size and its `ETag`.
  It needs what the upload needs (`UpdateTransfer` and the component's scope). 404 for an update or
  part this server holds no record of (an update only the executor holds, or one deleted) and for a
  part the backend no longer keeps; 501 when the backend cannot read parts back
  (`DiagnosticBackend::read_part` not implemented).

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

On `/v1` of a server configured with a node (ARCHITECTURE.md §7.1), `{base}` for the node-level
resources is the node, `/v1/components/{node}`, and the components are its subcomponents,
`/v1/components/{node}/subcomponents/{id}`: the root and flat forms are not served there, and the
root document's `updates` links the node's collection. `/vehicle/v1` is unchanged.

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

## Execute

Under `/v1`, `PUT {entity}/updates/{update_id}/execute` (component and node-level collections)
takes no x-update parameter: every execute is orchestrated, and this server settles a banked
update only on the tester's verdict. The x-ota alias `x-ota-control=orchestrated` is accepted and
changes nothing; any other `x-ota-control` value answers 400 before anything runs, and the update
stays as it was:

```json
{"error_code": "incomplete-request", "message": "x-ota-control accepts only orchestrated: this server settles an update only on the tester's verdict", "parameters": {"x-ota-control": ["orchestrated"]}}
```

An unknown update answers 404 first; then the state checks answer as before (409). The same
request serves every update shape: a single-shot update takes it and commits on its own as before.
The capability description (`GET /v1/docs`) lists execute without parameters.

A banked update then awaits its verdict however long it takes: the restart its executor needs
can be a power cycle that comes whenever the vehicle's is, and only the tester judges the update.
An embedding server may set a limit (`UpdatesConfig::orchestrated_watchdog`, none by default):
when it expires the server rolls the update back as `x-update-rollback` would, and it ends
ROLLED_BACK. A `DELETE` before the verdict rolls it back the same way. A restart of the server ends the wait
without a verdict: the update is no longer registered, and what becomes of it is the executor's
(issue #30).

Under `/v1`, `PUT {entity}/updates/{update_id}/automated` answers 409
`update-automated-not-supported`: an automated chain gives no verdict.

Under `/vehicle/v1` both are unchanged and deprecated: execute without the parameter runs a
banked update to activation and leaves it in a trial no verdict can settle, and `automated` chains
prepare into that execute.

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

Once an update has settled (COMMITTED, ROLLED_BACK or FAILED), the client removes it with the
standard `DELETE {entity}/updates/{update_id}`; before `execute`, the same `DELETE` discards a
prepared update. There is no finish verb. Under `/v1`, a `DELETE` while the update is still
running in phase execute (EXECUTING, EXECUTED, COMMITTING, ROLLING_BACK) answers 405
(ISO 17978-3 §7.18.8); `/vehicle/v1` deletes it, aborting the running step.

## Status member

`GET {entity}/updates/{update_id}/status` carries, beside the standard Table 270 members:

```json
"x-update": { "state": "<State>", "reset_kind": "none" | "component" | "node", "target": "<component id>", "verdict_scope": "update" | "node" }
```

`target` and `verdict_scope` are served under `/v1` only (see "Node-level collection").

The states are named after the verbs that lead to them. The state is total, and the standard
`phase`/`status` in the same body agree with it:

| state | phase | status | meaning |
|---|---|---|---|
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

The client does not choose whether a verdict is needed: under `/v1` every execute is orchestrated,
and a verdict (commit or rollback) is how every banked update is settled.

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
5. The restart named by `reset_kind` happens: whoever is authorized issues
   `PUT {entity}/status/restart`, or a power cycle does it. The client awaits EXECUTED.
6. When the updates report EXECUTED: send the verdict and await COMMITTED or ROLLED_BACK. A
   single-component or `component` update takes `x-update-commit` / `x-update-rollback` on the
   update; a node transaction takes the node operation once, for all of its updates. A rollback
   may also be sent while an update is still EXECUTING.
7. `DELETE` each update.

## Aliases

| x-ota name | x-update equivalent | served by |
|---|---|---|
| `x-ota-commit`, `x-ota-rollback` (per update) | `PUT …/updates/{id}/x-update-commit`, `…/x-update-rollback` | this crate |
| `x-ota-commit-trials`, `x-ota-rollback-trials` | node `x-update-commit`, `x-update-rollback` operations | machine manager |
| `x-ota-update-state` | `x-update-state` | machine manager |
| `x-ota-reset-kind`, `x-ota-substate` | `x-update.reset_kind`, `x-update.state` | this crate |
| `?x-ota-control=orchestrated` | none: every `/v1` execute is orchestrated (see "Execute") | this crate |
| `x-ota-force-rollback` | unchanged (admin operation, not lifecycle) | this crate |
| `x-ota-pull-update` | unchanged (admin operation, not lifecycle) | machine manager |

## Not in this version

- Executors that need a verdict refusing `automated` with 409 under `/vehicle/v1`.
- Retiring the x-ota names.
- Adopting a re-registration the executor still holds.
- Executor-side recording of the update id (which the listing needs).
