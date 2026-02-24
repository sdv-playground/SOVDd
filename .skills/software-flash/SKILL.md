---
name: software-flash
description: Software flash and OTA update procedures for ECUs via SOVD. Use when performing firmware updates, managing packages, or working with the flash transfer lifecycle.
metadata:
  author: sovd-team
  version: "2.0"
---

# Software Flash

This skill covers the SOVD firmware update lifecycle: package upload, verification, flash transfer, activation, commit, and rollback.

## Overview

SOVD flash follows a package-based workflow with a strict state machine:

```
Upload package → Verify → Start flash transfer → Transfer blocks →
Finalize (transfer exit) → ECU reset → Activated → Commit or Rollback
```

State machine: `Queued → Preparing → Transferring → AwaitingExit → AwaitingReset → Activated → Committed|RolledBack`

## API Endpoints

All under `/vehicle/v1/components/{id}/`.

### Package Management

| Method | Path | Description |
|--------|------|-------------|
| POST | `/files` | Upload firmware package (binary body) |
| GET | `/files` | List uploaded packages |
| GET | `/files/{file_id}` | Get package info |
| DELETE | `/files/{file_id}` | Delete package |
| POST | `/files/{file_id}/verify` | Verify package integrity |

### Flash Transfer

| Method | Path | Description |
|--------|------|-------------|
| POST | `/flash/transfer` | Start flash (body: `{"package_id": "..."}`) |
| GET | `/flash/transfer` | List active transfers |
| GET | `/flash/transfer/{transfer_id}` | Get transfer status/progress |
| DELETE | `/flash/transfer/{transfer_id}` | Abort transfer |
| PUT | `/flash/transferexit` | Finalize transfer (RequestTransferExit) |

### Activation & Commit

| Method | Path | Description |
|--------|------|-------------|
| GET | `/flash/activation` | Get activation state |
| POST | `/flash/commit` | Commit activated firmware |
| POST | `/flash/rollback` | Rollback to previous firmware |

### ECU Reset

| Method | Path | Description |
|--------|------|-------------|
| POST | `/reset` | ECU reset (body: `{"type": "hard"}`) |

## Flash Workflow

### Prerequisites

The caller is responsible for session and security setup before starting flash.

```bash
BASE=http://localhost:9080/vehicle/v1/components/vtx_ecm

# 1. Switch to programming session
curl -X PUT "$BASE/modes/session" \
  -H "Content-Type: application/json" \
  -d '{"value": "programming"}'

# 2. Security access (request seed)
SEED=$(curl -s -X PUT "$BASE/modes/security" \
  -H "Content-Type: application/json" \
  -d '{"value": "level1_requestseed"}' | jq -r '.seed')

# 3. Send key (XOR seed with secret)
curl -X PUT "$BASE/modes/security" \
  -H "Content-Type: application/json" \
  -d "{\"value\": \"level1\", \"key\": \"$KEY\"}"
```

### Upload & Verify

```bash
# Upload firmware binary
FILE_ID=$(curl -s -X POST "$BASE/files" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @firmware.bin | jq -r '.id')

# Verify integrity
curl -X POST "$BASE/files/$FILE_ID/verify"
```

### Flash Transfer

```bash
# Start flash (async — UDS 0x34/0x36 happen in background)
TRANSFER=$(curl -s -X POST "$BASE/flash/transfer" \
  -H "Content-Type: application/json" \
  -d "{\"package_id\": \"$FILE_ID\"}" | jq -r '.transfer_id')

# Poll progress
curl "$BASE/flash/transfer/$TRANSFER"
# → { "state": "transferring", "progress": { "percent": 75.0 } }

# Finalize (UDS RequestTransferExit 0x37)
curl -X PUT "$BASE/flash/transferexit"
# → state becomes "awaiting_reset"
```

### Activate & Commit

```bash
# ECU reset (UDS 0x11) — ECU reboots with new firmware
curl -X POST "$BASE/reset" \
  -H "Content-Type: application/json" \
  -d '{"type": "hard"}'

# Check activation (auto-detects reboot via DID 0xF189)
curl "$BASE/flash/activation"
# → { "state": "activated", "active_version": "3.0.0" }

# Commit (requires extended session + security after reset)
curl -X POST "$BASE/flash/commit"

# Or rollback
curl -X POST "$BASE/flash/rollback"
```

### Abort

```bash
# Abort is only valid during Queued through AwaitingExit
curl -X DELETE "$BASE/flash/transfer/$TRANSFER"
```

## State Machine

| State | Valid transitions | Description |
|-------|-------------------|-------------|
| Queued | Preparing, Failed (abort) | Transfer created, not started |
| Preparing | Transferring, Failed | Session/erase in progress |
| Transferring | AwaitingExit, Failed (abort) | Block transfer in progress |
| AwaitingExit | AwaitingReset, Complete, Failed (abort) | Transfer done, awaiting exit |
| AwaitingReset | Activated | ECU must reboot before commit/rollback |
| Activated | Committed, RolledBack | New firmware running, awaiting decision |
| Committed | (terminal) | Firmware accepted |
| RolledBack | (terminal) | Reverted to previous firmware |

## UDS Services Used

| SOVD Operation | UDS Service | Description |
|----------------|-------------|-------------|
| Start flash | 0x34 RequestDownload | Initiate download to ECU |
| Transfer blocks | 0x36 TransferData | Send firmware blocks |
| Finalize | 0x37 RequestTransferExit | End transfer session |
| Erase memory | 0x31 RoutineControl (0xFF00) | Erase flash before write |
| ECU reset | 0x11 ECUReset | Reboot ECU |
| Commit | 0x31 RoutineControl (0xFF01) | Commit A/B bank |
| Rollback | 0x31 RoutineControl (0xFF02) | Rollback A/B bank |

## Important Notes

- **Session/security after reset**: ECU reset reverts to default session (0x01) and re-locks security. The caller must set up extended session + security again before commit/rollback.
- **Block counter**: Configurable start value (0 or 1). Wraps at 255 back to start value.
- **Abort cleanup**: After abort, the server sends TransferExit to the ECU and returns to default session.
- **Lock ordering**: `activation_state` lock acquired before `flash_state` to prevent deadlocks.

See [references/flash-sequences.md](references/flash-sequences.md) for raw UDS flash sequences.
