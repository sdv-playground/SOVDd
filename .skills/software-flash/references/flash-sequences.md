# Flash Sequences Reference

## Standard UDS Flash Sequence

```
1. DiagnosticSessionControl (0x10) -> Extended (0x03)
2. SecurityAccess (0x27) -> Request Seed (0x01)
3. SecurityAccess (0x27) -> Send Key (0x02)
4. DiagnosticSessionControl (0x10) -> Programming (0x02)
5. RoutineControl (0x31) -> Check Programming Preconditions
6. RequestDownload (0x34)
7. TransferData (0x36) x N
8. RequestTransferExit (0x37)
9. RoutineControl (0x31) -> Check Programming Dependencies
10. ECUReset (0x11) -> Hard Reset (0x01)
```

## Multi-Block Flash

For large firmware files, the flash sequence may need to be repeated for different memory regions:

```
For each region:
  1. RequestDownload (address, size)
  2. TransferData x N
  3. RequestTransferExit
```

## Memory Regions

| Region | Typical Address | Description |
|--------|-----------------|-------------|
| Bootloader | 0x00000000 | Usually protected |
| Application | 0x00010000 | Main firmware |
| Calibration | 0x000F0000 | Tuning data |
| NVM | 0x00100000 | Non-volatile memory |

## Data Format Identifier

The data format byte in RequestDownload/Upload:

```
High nibble: Compression method
  0 = No compression
  1 = Vendor-specific compression

Low nibble: Encryption method
  0 = No encryption
  1 = Vendor-specific encryption
```

## Address/Length Format Identifier

Specifies byte sizes for memory address and size parameters:

```
High nibble: Number of bytes for memory size
Low nibble: Number of bytes for memory address

0x44 = 4 bytes each (32-bit addressing)
0x24 = 2 bytes size, 4 bytes address
0x22 = 2 bytes each (16-bit addressing)
```

## Block Sequence Counter

- Start value is configurable: 0 (some OEMs) or 1 (ISO 14229 default)
- Increments with each TransferData
- Wraps from 255 back to the configured start value
- ECU tracks expected counter
- In SOVDd, configured via `transfer_data_block_counter_start` in the TOML config

## Error Recovery

### Sequence Error (NRC 0x24)
- Wrong block counter
- Solution: Restart transfer from last acknowledged block

### Programming Failure (NRC 0x72)
- Flash write failed
- Solution: Retry block or abort and retry entire transfer

### Busy (NRC 0x21)
- ECU processing previous request
- Solution: Wait and retry

## Timing Considerations

| Parameter | Typical Value | Description |
|-----------|---------------|-------------|
| P2 | 50ms | Normal response timeout |
| P2* | 5000ms | Extended response timeout |
| Block delay | 10-50ms | Between TransferData calls |
| Erase time | 1-30s | Memory erase duration |

## Checksum Algorithms

Common checksums for firmware validation:

| Algorithm | Size | Usage |
|-----------|------|-------|
| CRC-16 | 2 bytes | Block validation |
| CRC-32 | 4 bytes | File validation |
| SHA-256 | 32 bytes | Secure boot |

## Signature Verification

Some ECUs require signed firmware:
1. Calculate hash of firmware
2. Sign with OEM private key
3. Append signature to firmware
4. ECU verifies with public key
