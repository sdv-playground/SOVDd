# x-update transfer: how an update's parts reach the server (proposal)

Status: proposal, not served. It extends the [x-update](x-update.md) contract; nothing in it
changes what a server serves today.

## Problem

Today every part of an update is pushed: the client uploads the manifest and each payload to the
update's bulk-data collection, then sends `prepare`. Some executors can fetch a payload
themselves, from the URI the manifest names (a content store, a time-limited download URL), and
some deployments want them to: the bytes then travel once, straight to the device, instead of
through the client. An embedding machine manager may already do this outside the `/updates`
lifecycle (`x-ota-pull-update`, a whole-campaign operation). A client of `/updates` cannot tell
which parts the server wants pushed and which it will fetch, so it pushes everything.

## What ISO 17978-3 says

- §7.18.9 (register, `POST /updates`): the request body and its `Content-Type` are "ExVe
  manufacturer and SOVD server implementation specific"; only the `201`, `Location` and `id` are
  fixed. How the update's bytes reach the server is left to the implementation.
- §7.18.1.3, step 2 (prepare): the update "is prepared for installation, by downloading it from the
  OTA backend, validating its applicability …". The standard's own picture is the server pulling at
  prepare.
- §7.18.3 Table 261 (update details) has no transfer attribute; `origin` (`remote`, `proximity`)
  says who triggers the update, not how its bytes move.
- §7.20 (bulk-data upload) is the standard push form, which x-update already uses on an update's
  bulk-data collection.
- §5.4.5: anything custom is an `x-<ext>-<name>` extension.

So stating the transfer is an x-update extension, beside the standard members, and pulling at
`prepare` is the behaviour §7.18.1.3 describes.

## Proposal

The server states, per part, whether it wants the part pushed or will pull it. The client may
always push: a part uploaded before `prepare` is used as uploaded and is not fetched.

### Capability: what an entity can do

`GET {entity}` (component body), beside the existing members:

```json
"capabilities": { "x-update": { "verdict": true, "verdict_scope": "node", "transfer": ["pull", "push"] } }
```

`transfer` lists the modes the entity's executor supports, preferred first. Absent means
`["push"]`, which is every server today. The node-level capability document may carry the same
list per update target:

```json
"x-update": { "update-targets": ["host", "vm1", "vm2", "apps"], "transfer": { "vm2": ["pull", "push"] } }
```

A target not named there is `["push"]`.

### Per part: what this update wants

The manifest is always pushed: until the server has it, it knows no other part. Once the manifest
is uploaded, `GET {entity}/updates/{update_id}/bulk-data` lists every part the manifest names,
uploaded or not, and each item carries `transfer`:

| `transfer` | meaning | `href` |
|---|---|---|
| `push` | the client uploads the part (`POST …/bulk-data`, as today) before `prepare` | the upload's `Location` once uploaded |
| `pull` | the executor fetches the part from the URI the manifest names, during `prepare` | absent until uploaded |

`present` keeps its meaning (the executor already holds the part, matched by name). A part that
is `present` needs neither a push nor a pull.

`GET {entity}/updates/{update_id}/status` summarises it in the `x-update` member:

```json
"x-update": { "state": "REGISTERED", "transfer": { "push": ["manifest", "<part>"], "pull": ["<part>"] } }
```

listing the part ids still to be pushed and those the server will pull. The lists are emptied as
parts are uploaded or fetched, so a client sends `prepare` when `push` is empty.

### Pulling

- The source is the part's URI in the uploaded manifest; the part's id is that URI, as for a
  pushed payload today. The executor checks a fetched part against the digest and size the
  manifest commits to, as it checks an uploaded one; a mismatch fails the update (FAILED, standard
  error object).
- The fetch happens in `prepare`: PREPARING covers it, and the standard `progress` counts it. A
  part that cannot be fetched fails `prepare`; the update goes FAILED and the client may `DELETE`
  it and register again, pushing the part this time.
- The URI must be reachable from the server, and valid until `prepare` has fetched it (a
  time-limited URL is issued for the server's network, not the client's). A client that knows it
  is not reachable pushes the part: an upload is always accepted while the update is REGISTERED.
- Credentials: none beyond the URI. A URI that needs a bearer the server does not have is the
  client's to push.

### Backward compatibility

A server that serves none of this is a server whose every part is `push`. A client that ignores
`transfer` pushes everything, which a server serving this accepts unchanged. A client that reads
`transfer` and finds it absent pushes everything.

### In this crate

- A `DiagnosticBackend` method reporting a part's transfer, given the update's component and the
  part id (default `push`), and the entity's supported modes for the capability (default
  `["push"]`). The bulk-data listing, the status member and the capability documents are built from
  them.
- The fetch itself is the backend's, at `prepare`, as an embedding machine manager's per-part
  copy-vs-fetch reconciliation already does; this crate only reports it.

## Open questions

1. Per part or per update? Per part keeps a mixed bank possible (a part the executor fetches, a
   small one the client pushes), and the per-update summary in the status member covers a client
   that wants one answer. Is the per-part form wanted, or only `transfer` on the update?
2. Naming. §5.4.5 reads `x-<ext>-<name>`, with `<ext>` leading back to the extension's source;
   `x-update` and its verbs have no `<ext>` part. This proposal keeps the new member inside the
   existing `x-update` object, consistent with what is served. Should the x-update names move to
   an `x-<ext>-` form as a whole, separately?
3. Should the server be able to require `pull` (refuse an upload of a `pull` part with 409), e.g.
   to keep large payloads off the client link? This draft never refuses an upload.
4. `x-ota-pull-update` (the whole-campaign pull operation of an embedding machine manager) stays as
   it is; with per-part pull on `/updates`, is it still needed by `/updates` clients?
