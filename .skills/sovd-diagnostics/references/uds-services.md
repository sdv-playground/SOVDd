# UDS Services Reference

## Diagnostic Session Control (0x10)

Controls the diagnostic session mode.

**Sessions:**
- `0x01` - Default session (limited diagnostics)
- `0x02` - Programming session (for flashing)
- `0x03` - Extended session (full diagnostics)

**Response:** Session parameters (P2 timing)

## Security Access (0x27)

Two-step authentication process:
1. Request seed (odd sub-function: 0x01, 0x03, etc.)
2. Send key (even sub-function: 0x02, 0x04, etc.)

**Security Levels:**
- Level 1 (0x01/0x02): Basic access
- Level 2 (0x03/0x04): Extended access
- Vendor-specific levels available

## Read Data By Identifier (0x22)

Read one or more DIDs in a single request.

**Standard DIDs:**
| DID | Description |
|-----|-------------|
| 0xF186 | Active diagnostic session |
| 0xF187 | Spare part number |
| 0xF188 | Software version |
| 0xF189 | Hardware version |
| 0xF18A | Supplier ID |
| 0xF18B | ECU manufacturing date |
| 0xF18C | ECU serial number |
| 0xF190 | VIN |
| 0xF191 | Hardware part number |
| 0xF192 | Supplier hardware version |
| 0xF193 | Supplier software version |
| 0xF194 | Supplier manufacturer info |
| 0xF195 | Software part number |

## Write Data By Identifier (0x2E)

Write data to a DID. Usually requires extended session and security access.

## Routine Control (0x31)

Control diagnostic routines.

**Sub-functions:**
- `0x01` - Start routine
- `0x02` - Stop routine
- `0x03` - Request routine results

**Common Routines:**
| RID | Description |
|-----|-------------|
| 0x0202 | Check programming dependencies |
| 0x0203 | Erase memory |
| 0xFF00 | Check programming preconditions |
| 0xFF01 | Finalize programming |

## Read DTC Information (0x19)

Read diagnostic trouble codes.

**Sub-functions:**
| Sub | Description |
|-----|-------------|
| 0x01 | Report number of DTCs by status mask |
| 0x02 | Report DTCs by status mask |
| 0x03 | Report DTC snapshot ID |
| 0x04 | Report DTC snapshot by DTC number |
| 0x06 | Report DTCs with extended data |
| 0x0A | Report supported DTCs |

**DTC Status Bits:**
| Bit | Meaning |
|-----|---------|
| 0 | Test failed |
| 1 | Test failed this operation cycle |
| 2 | Pending DTC |
| 3 | Confirmed DTC |
| 4 | Test not completed since last clear |
| 5 | Test failed since last clear |
| 6 | Test not completed this operation cycle |
| 7 | Warning indicator requested |

## Clear Diagnostic Information (0x14)

Clear DTCs from memory.

**Group parameter:**
- `0xFFFFFF` - All DTCs
- Specific group IDs for selective clearing

## Input Output Control (0x2F)

Control ECU outputs for testing.

**Sub-functions:**
- `0x00` - Return control to ECU
- `0x01` - Reset to default
- `0x02` - Freeze current state
- `0x03` - Short-term adjustment

## Request Download (0x34)

Initiate download (write to ECU).

**Parameters:**
- Data format (compression/encryption)
- Address and length format
- Memory address
- Memory size

**Response:** Max block length

## Request Upload (0x35)

Initiate upload (read from ECU).

Same parameters as Request Download.

## Transfer Data (0x36)

Transfer data blocks during download/upload.

**Parameters:**
- Block sequence counter (wraps at 0xFF)
- Data bytes (max from 0x34/0x35 response)

## Request Transfer Exit (0x37)

Finalize transfer operation.

## Negative Response Codes (NRC)

| Code | Name | Description |
|------|------|-------------|
| 0x10 | General reject | Request not supported |
| 0x11 | Service not supported | Unknown service ID |
| 0x12 | Sub-function not supported | Unknown sub-function |
| 0x13 | Incorrect message length | Wrong data length |
| 0x14 | Response too long | Response exceeds buffer |
| 0x21 | Busy repeat request | ECU busy, retry |
| 0x22 | Conditions not correct | Prerequisites not met |
| 0x24 | Request sequence error | Wrong sequence |
| 0x25 | No response from subnet | Gateway error |
| 0x26 | Failure prevents execution | Hardware failure |
| 0x31 | Request out of range | Invalid parameter |
| 0x33 | Security access denied | Authentication required |
| 0x35 | Invalid key | Wrong key sent |
| 0x36 | Exceeded attempts | Too many wrong keys |
| 0x37 | Required time delay not expired | Wait before retry |
| 0x70 | Upload/download not accepted | Transfer rejected |
| 0x71 | Transfer suspended | Transfer interrupted |
| 0x72 | General programming failure | Flash error |
| 0x73 | Wrong block sequence counter | Sequence mismatch |
| 0x78 | Response pending | Processing, wait |
| 0x7E | Sub-function not supported in session | Wrong session |
| 0x7F | Service not supported in session | Wrong session |
