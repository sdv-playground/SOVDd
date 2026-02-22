---
name: software-flash
description: Software download and upload procedures for ECU flashing via SOVD. Use when performing firmware updates, reading ECU memory, or transferring software to/from ECUs.
metadata:
  author: sovd-team
  version: "1.0"
---

# Software Flash

This skill covers software transfer operations (download/upload) through the SOVD REST API, implementing UDS services 0x34, 0x35, 0x36, and 0x37.

## Overview

Software transfer follows a session-based workflow:
1. Switch to programming session
2. Authenticate (security access)
3. Start transfer session (download or upload)
4. Transfer data blocks
5. Finalize transfer
6. Reset ECU (optional)

## API Endpoints

### Download (Write to ECU)
- `POST /vehicle/v1/components/{id}/software/download` - Start download session
- `PUT /vehicle/v1/components/{id}/software/download/{session}` - Transfer block
- `DELETE /vehicle/v1/components/{id}/software/download/{session}` - Finalize

### Upload (Read from ECU)
- `POST /vehicle/v1/components/{id}/software/upload` - Start upload session
- `GET /vehicle/v1/components/{id}/software/upload/{session}` - Get block
- `DELETE /vehicle/v1/components/{id}/software/upload/{session}` - Finalize

## Download Workflow (Flashing)

### 1. Prepare Session
```bash
# Switch to programming session
curl -X PUT http://localhost:18080/vehicle/v1/components/vtx_ecm/modes/session \
  -H "Content-Type: application/json" \
  -d '{"value": "programming"}'

# Perform security access
curl -X PUT http://localhost:18080/vehicle/v1/components/vtx_ecm/modes/security \
  -H "Content-Type: application/json" \
  -d '{"value": "level1_requestseed"}'

# Response contains seed, calculate key and send:
curl -X PUT http://localhost:18080/vehicle/v1/components/vtx_ecm/modes/security \
  -H "Content-Type: application/json" \
  -d '{"value": "level1", "key": "DEADBEEF"}'
```

### 2. Start Download
```bash
curl -X POST http://localhost:18080/vehicle/v1/components/vtx_ecm/software/download \
  -H "Content-Type: application/json" \
  -d '{
    "memory_address": "0x00100000",
    "memory_size": 65536,
    "data_format": 0,
    "address_and_length_format": 68
  }'
```

**Response:**
```json
{
  "session_id": "abc123",
  "max_block_size": 4094,
  "expected_blocks": 16,
  "transfer_url": "/vehicle/v1/components/vtx_ecm/software/download/abc123",
  "finalize_url": "/vehicle/v1/components/vtx_ecm/software/download/abc123"
}
```

### 3. Transfer Data Blocks
```bash
# Transfer binary data (repeat for each block)
curl -X PUT http://localhost:18080/vehicle/v1/components/vtx_ecm/software/download/abc123 \
  -H "Content-Type: application/octet-stream" \
  --data-binary @block1.bin
```

**Response:**
```json
{
  "block_counter": 1,
  "bytes_transferred": 4094,
  "total_transferred": 4094,
  "remaining": 61442,
  "progress_percent": 6.25
}
```

### 4. Finalize Download
```bash
curl -X DELETE http://localhost:18080/vehicle/v1/components/vtx_ecm/software/download/abc123
```

**Response:**
```json
{
  "success": true,
  "total_bytes": 65536,
  "duration_ms": 12500,
  "checksum": "A1B2C3D4"
}
```

### 5. Reset ECU (Optional)
```bash
curl -X POST http://localhost:18080/vehicle/v1/components/vtx_ecm/operations/ecu_reset/executions \
  -H "Content-Type: application/json" \
  -d '{"params": {"reset_type": 1}}'
```

## Upload Workflow (Reading Memory)

### 1. Prepare Session
```bash
# Switch to extended session (programming may be required for some ECUs)
curl -X PUT http://localhost:18080/vehicle/v1/components/vtx_ecm/modes/session \
  -H "Content-Type: application/json" \
  -d '{"value": "extended"}'
```

### 2. Start Upload
```bash
curl -X POST http://localhost:18080/vehicle/v1/components/vtx_ecm/software/upload \
  -H "Content-Type: application/json" \
  -d '{
    "memory_address": "0x00000000",
    "memory_size": 256
  }'
```

**Response:**
```json
{
  "session_id": "xyz789",
  "max_block_size": 4094,
  "expected_blocks": 1,
  "retrieve_url": "/vehicle/v1/components/vtx_ecm/software/upload/xyz789"
}
```

### 3. Retrieve Data
```bash
curl http://localhost:18080/vehicle/v1/components/vtx_ecm/software/upload/xyz789
```

**Response:**
```json
{
  "block_counter": 1,
  "data": "000102030405...",
  "bytes_received": 256
}
```

### 4. Finalize Upload
```bash
curl -X DELETE http://localhost:18080/vehicle/v1/components/vtx_ecm/software/upload/xyz789
```

## Request Parameters

### Start Download/Upload Request
| Field | Type | Description |
|-------|------|-------------|
| `memory_address` | string/int | Target address (hex string or integer) |
| `memory_size` | int | Total bytes to transfer |
| `data_format` | int | 0=uncompressed, others vendor-specific |
| `address_and_length_format` | int | Byte sizes (default 0x44 = 4+4) |

### Address Format Byte
```
0x44 = 4 bytes address + 4 bytes length
0x24 = 2 bytes address + 4 bytes length
0x14 = 1 byte address + 4 bytes length
```

## Error Handling

| HTTP | UDS NRC | Meaning |
|------|---------|---------|
| 412 | 0x22 | Session/security requirements not met |
| 403 | 0x33 | Security access denied |
| 409 | - | Transfer already in progress |
| 400 | 0x31 | Invalid address or size |
| 502 | 0x72 | Programming failure |
| 504 | - | Transfer timeout |

## Best Practices

1. **Always verify prerequisites:**
   - Check current session mode
   - Verify security access level
   - Ensure no other transfer in progress

2. **Handle block transfers carefully:**
   - Track block counter (wraps at 255)
   - Verify each block response
   - Implement retry logic for failures

3. **Validate transfers:**
   - Compare final checksum
   - Verify total bytes transferred
   - Check ECU response after reset

4. **Clean up on failure:**
   - Always call finalize (DELETE) even on error
   - Return ECU to default session
   - Log transfer details for debugging

## Complete Flash Script Example

```bash
#!/bin/bash
# Flash firmware to ECU

ECU="vtx_ecm"
BASE_URL="http://localhost:18080/vehicle/v1/components/$ECU"
FIRMWARE="firmware.bin"
ADDRESS="0x00100000"

# 1. Switch to programming session
curl -X PUT "$BASE_URL/modes/session" \
  -H "Content-Type: application/json" \
  -d '{"value": "programming"}'

# 2. Security access
SEED=$(curl -s -X PUT "$BASE_URL/modes/security" \
  -H "Content-Type: application/json" \
  -d '{"value": "level1_requestseed"}' | jq -r '.seed')
KEY=$(calculate_key "$SEED")  # Your key algorithm
curl -X PUT "$BASE_URL/modes/security" \
  -H "Content-Type: application/json" \
  -d "{\"value\": \"level1\", \"key\": \"$KEY\"}"

# 3. Start download
SIZE=$(stat -f%z "$FIRMWARE" 2>/dev/null || stat -c%s "$FIRMWARE")
RESP=$(curl -s -X POST "$BASE_URL/software/download" \
  -H "Content-Type: application/json" \
  -d "{\"memory_address\": \"$ADDRESS\", \"memory_size\": $SIZE}")
SESSION=$(echo "$RESP" | jq -r '.session_id')
BLOCK_SIZE=$(echo "$RESP" | jq -r '.max_block_size')

# 4. Transfer blocks
OFFSET=0
while [ $OFFSET -lt $SIZE ]; do
  dd if="$FIRMWARE" bs=$BLOCK_SIZE skip=$((OFFSET/BLOCK_SIZE)) count=1 2>/dev/null | \
    curl -X PUT "$BASE_URL/software/download/$SESSION" \
      -H "Content-Type: application/octet-stream" \
      --data-binary @-
  OFFSET=$((OFFSET + BLOCK_SIZE))
done

# 5. Finalize
curl -X DELETE "$BASE_URL/software/download/$SESSION"

# 6. Reset
curl -X POST "$BASE_URL/operations/ecu_reset/executions" \
  -H "Content-Type: application/json" \
  -d '{"params": {"reset_type": 1}}'

echo "Flash complete!"
```

See [references/flash-sequences.md](references/flash-sequences.md) for vendor-specific flash sequences.
