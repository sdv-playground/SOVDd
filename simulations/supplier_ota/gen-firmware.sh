#!/bin/bash
# =============================================================================
# gen-firmware.sh — Generate firmware images for all simulation ECUs
#
# Uses the binary format defined in crates/example-ecu/src/sw_package.rs:
#
#   ┌─────────────────────────────────────┐
#   │  Header magic "EXAMPLE_FW" (10)     │  offset 0
#   ├─────────────────────────────────────┤
#   │  Version string (32 bytes, padded)  │  offset 10
#   ├─────────────────────────────────────┤
#   │  Target ECU ID (32 bytes, padded)   │  offset 42
#   ├─────────────────────────────────────┤
#   │  Firmware data (variable)           │  offset 74
#   ├─────────────────────────────────────┤
#   │  SHA-256 of bytes 0..(len-42) (32)  │  offset len-42
#   │  Footer magic "EXFW_END!\0" (10)    │  offset len-10
#   └─────────────────────────────────────┘
#
# ECU definitions are read from the simulation TOML configs (config/ecu-*.toml).
# The target version is bumped from the current ecu_sw_version parameter.
#
# Usage:
#   ./gen-firmware.sh                  # default 1 MiB data payload
#   ./gen-firmware.sh --size 1048576   # custom data size
#   ./gen-firmware.sh --out /tmp/fw    # custom output directory
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="$SCRIPT_DIR/firmware"
DATA_SIZE=1048576  # 1 MiB default
CONFIG_DIR="$SCRIPT_DIR/config"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --size)  DATA_SIZE="$2"; shift 2 ;;
        --out)   OUT_DIR="$2"; shift 2 ;;
        *)       echo "Unknown option: $1"; exit 1 ;;
    esac
done

mkdir -p "$OUT_DIR"

python3 - "$OUT_DIR" "$DATA_SIZE" "$CONFIG_DIR" << 'PYEOF'
import struct, sys, os, hashlib, re, glob

out_dir = sys.argv[1]
data_size = int(sys.argv[2])
config_dir = sys.argv[3]

# Use tomllib (3.11+) or tomli as fallback
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        import ast
        class _MinimalToml:
            @staticmethod
            def loads(s):
                result = {}
                current_section = result
                for line in s.split("\n"):
                    line = line.strip()
                    if not line or line.startswith("#"):
                        continue
                    m = re.match(r'^\[+([^\]]+)\]+', line)
                    if m:
                        section = m.group(1).strip()
                        if line.startswith("[["):
                            result.setdefault(section, [])
                            result[section].append({})
                            current_section = result[section][-1]
                        else:
                            result.setdefault(section, {})
                            current_section = result[section]
                        continue
                    m = re.match(r'^(\w+)\s*=\s*(.+)$', line)
                    if m:
                        key, val = m.group(1), m.group(2).strip()
                        if val.startswith('"') and val.endswith('"'):
                            val = val[1:-1]
                        elif val in ("true", "false"):
                            val = val == "true"
                        elif val.startswith("["):
                            val = ast.literal_eval(val)
                        else:
                            try: val = int(val, 0)
                            except ValueError:
                                try: val = float(val)
                                except ValueError: pass
                        current_section[key] = val
                return result
        class tomllib:
            @staticmethod
            def loads(s):
                return _MinimalToml.loads(s)

# ── Format constants (must match crates/example-ecu/src/sw_package.rs) ─────────

HEADER_MAGIC  = b"EXAMPLE_FW"       # 10 bytes
FOOTER_MAGIC  = b"EXFW_END!\x00"    # 10 bytes
VERSION_LEN   = 32
TARGET_ECU_LEN = 32
FOOTER_SIZE   = 32 + len(FOOTER_MAGIC)  # SHA-256 + footer magic = 42

# ── Read ECU definitions from simulation TOML configs ───────────────────────

def read_ecu_config(toml_path):
    with open(toml_path, "r") as f:
        cfg = tomllib.loads(f.read())
    ecu_id = cfg.get("id", "unknown")
    current_version = None
    for param in cfg.get("parameters", []):
        if param.get("id") == "ecu_sw_version":
            current_version = str(param.get("value", ""))
            break
    return ecu_id, current_version

def bump_version(version_str):
    """Bump the major version: v1.0.0 -> v2.0.0, VORTEX-VX500-v2.4.1 -> VORTEX-VX500-v3.0.0."""
    m = re.match(r'^(.*?)v?(\d+)\.(\d+)\.(\d+)$', version_str)
    if m:
        prefix_str = m.group(1)
        major = int(m.group(2)) + 1
        had_v = version_str[len(prefix_str):].startswith("v")
        v_prefix = "v" if had_v else ""
        return f"{prefix_str}{v_prefix}{major}.0.0"
    return version_str + "-update"

config_files = sorted(glob.glob(os.path.join(config_dir, "ecu-*.toml")))
if not config_files:
    print(f"ERROR: No ecu-*.toml files found in {config_dir}", file=sys.stderr)
    sys.exit(1)

ecus = []
for cf in config_files:
    ecu_id, current_ver = read_ecu_config(cf)
    if current_ver is None:
        print(f"  WARNING: {os.path.basename(cf)}: no ecu_sw_version parameter, skipping")
        continue
    target_ver = bump_version(current_ver)
    ecus.append({"id": ecu_id, "current_version": current_ver, "target_version": target_ver})

# ── Build firmware images ───────────────────────────────────────────────────

def pad(s: bytes, length: int) -> bytes:
    return s[:length].ljust(length, b"\x00")

def build_firmware_image(ecu_id: str, version: str, data_size: int) -> bytes:
    """Build binary firmware image matching sw_package::FirmwareImage::to_bytes()."""
    # Generate deterministic data payload
    seed = hashlib.sha256((ecu_id + version).encode()).digest()
    chunks = []
    remaining = data_size
    h = seed
    while remaining > 0:
        h = hashlib.sha256(h).digest()
        take = min(len(h), remaining)
        chunks.append(h[:take])
        remaining -= take
    fw_data = b"".join(chunks)

    # Build image: header + data (before footer)
    buf = bytearray()
    buf += HEADER_MAGIC                           # offset 0:  magic (10)
    buf += pad(version.encode("utf-8"), VERSION_LEN)    # offset 10: version (32)
    buf += pad(ecu_id.encode("utf-8"), TARGET_ECU_LEN)  # offset 42: target ECU (32)
    buf += fw_data                                 # offset 74: data (variable)

    # SHA-256 of everything so far
    checksum = hashlib.sha256(bytes(buf)).digest()
    buf += checksum                                # 32 bytes
    buf += FOOTER_MAGIC                            # 10 bytes

    return bytes(buf)

# ── Generate ────────────────────────────────────────────────────────────────

print(f"Generating firmware images ({data_size / (1024*1024):.1f} MiB data each)...")
print()

for ecu in ecus:
    image = build_firmware_image(ecu["id"], ecu["target_version"], data_size)
    filename = f"{ecu['id']}_fw_{ecu['target_version']}.bin"
    path = os.path.join(out_dir, filename)
    with open(path, "wb") as f:
        f.write(image)

    total_mb = len(image) / (1024 * 1024)
    print(f"  {ecu['id']:.<30s} {ecu['current_version']} -> {ecu['target_version']}  ({total_mb:.1f} MiB)")
    print(f"  {'':30s} {path}")

PYEOF

echo ""
echo "Firmware images written to: $OUT_DIR/"
echo ""
ls -lh "$OUT_DIR"/*.bin
