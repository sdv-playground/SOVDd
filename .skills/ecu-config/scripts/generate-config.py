#!/usr/bin/env python3
"""
Generate SOVD ECU configuration from discovery or template.

Usage:
    python generate-config.py --name engine_ecu --tx-id 0x7E0 --rx-id 0x7E8
    python generate-config.py --from-discovery discovery.json
"""

import argparse
import json
import sys
from datetime import datetime


def generate_toml_config(ecu_id: str, name: str, tx_id: str, rx_id: str,
                         interface: str = "vcan0", transport: str = "isotp") -> str:
    """Generate TOML configuration for an ECU."""

    config = f'''# SOVD ECU Configuration
# Generated: {datetime.now().isoformat()}

[server]
host = "0.0.0.0"
port = 18080

[ecu.{ecu_id}]
name = "{name}"
entity_type = "ecu"
description = "Auto-generated configuration for {name}"

'''

    if transport == "isotp":
        config += f'''[ecu.{ecu_id}.transport.isotp]
interface = "{interface}"
tx_id = "{tx_id}"
rx_id = "{rx_id}"
padding = 0xCC
'''
    elif transport == "doip":
        config += f'''[ecu.{ecu_id}.transport.doip]
host = "{interface}"
port = 13400
logical_address = {tx_id}
source_address = 0x0E80
'''

    config += f'''
# Session timing (optional overrides)
# [ecu.{ecu_id}.timing]
# p2_timeout_ms = 50
# p2_star_timeout_ms = 5000

# Security configuration (optional)
# [ecu.{ecu_id}.security]
# algorithm = "xor"
# secret = [0x12, 0x34, 0x56, 0x78]

# DID store path
[dids]
store_path = "config/dids.yaml"
'''

    return config


def generate_did_yaml(basic_dids: bool = True) -> str:
    """Generate basic DID definitions YAML."""

    yaml = '''# DID Definitions
# Standard identification and operational parameters

dids:
  # === Standard Identification DIDs ===
  - did: 0xF190
    name: vin
    description: Vehicle Identification Number
    access: public
    data_type: ascii
    length: 17

  - did: 0xF187
    name: part_number
    description: ECU Part Number
    access: public
    data_type: ascii
    length: 16

  - did: 0xF18C
    name: serial_number
    description: ECU Serial Number
    access: public
    data_type: ascii
    length: 16

  - did: 0xF188
    name: software_version
    description: Software Version
    access: public
    data_type: ascii
    length: 16

  - did: 0xF189
    name: hardware_version
    description: Hardware Version
    access: public
    data_type: ascii
    length: 16

  - did: 0xF191
    name: hardware_part_number
    description: Hardware Part Number
    access: public
    data_type: ascii
    length: 16

  - did: 0xF195
    name: software_part_number
    description: Software Part Number
    access: public
    data_type: ascii
    length: 16
'''

    if basic_dids:
        yaml += '''
  # === Example Operational DIDs ===
  # Uncomment and modify for your ECU

  # - did: 0x1000
  #   name: engine_speed
  #   description: Engine RPM
  #   access: extended
  #   data_type: uint16
  #   scaling:
  #     factor: 0.25
  #     offset: 0
  #     unit: rpm

  # - did: 0x1001
  #   name: vehicle_speed
  #   description: Vehicle Speed
  #   access: extended
  #   data_type: uint16
  #   scaling:
  #     factor: 0.01
  #     offset: 0
  #     unit: km/h
'''

    return yaml


def from_discovery(discovery_file: str) -> str:
    """Generate config from discovery JSON output."""

    with open(discovery_file, 'r') as f:
        data = json.load(f)

    configs = []
    for i, ecu in enumerate(data.get('ecus', [])):
        ecu_id = f"ecu_{i}"
        name = ecu.get('vin', f'ECU {i}')[:20]
        tx_id = ecu.get('tx_can_id', '0x7E0')
        rx_id = ecu.get('rx_can_id', '0x7E8')

        configs.append(generate_toml_config(ecu_id, name, tx_id, rx_id))

    return '\n---\n'.join(configs)


def main():
    parser = argparse.ArgumentParser(description='Generate SOVD ECU configuration')
    parser.add_argument('--name', '-n', help='ECU name')
    parser.add_argument('--id', '-i', help='ECU ID (default: derived from name)')
    parser.add_argument('--tx-id', '-t', help='TX CAN ID (tester -> ECU)')
    parser.add_argument('--rx-id', '-r', help='RX CAN ID (ECU -> tester)')
    parser.add_argument('--interface', '-I', default='vcan0', help='CAN interface')
    parser.add_argument('--transport', choices=['isotp', 'doip'], default='isotp')
    parser.add_argument('--from-discovery', '-d', help='Generate from discovery JSON')
    parser.add_argument('--dids', action='store_true', help='Also generate DID YAML')
    parser.add_argument('--output', '-o', help='Output file (default: stdout)')

    args = parser.parse_args()

    if args.from_discovery:
        config = from_discovery(args.from_discovery)
    elif args.name and args.tx_id and args.rx_id:
        ecu_id = args.id or args.name.lower().replace(' ', '_').replace('-', '_')
        config = generate_toml_config(
            ecu_id, args.name, args.tx_id, args.rx_id,
            args.interface, args.transport
        )
    else:
        parser.print_help()
        sys.exit(1)

    if args.output:
        with open(args.output, 'w') as f:
            f.write(config)
        print(f"Config written to {args.output}", file=sys.stderr)

        if args.dids:
            did_file = args.output.replace('.toml', '-dids.yaml')
            with open(did_file, 'w') as f:
                f.write(generate_did_yaml())
            print(f"DIDs written to {did_file}", file=sys.stderr)
    else:
        print(config)
        if args.dids:
            print("\n# === DID Definitions (save as dids.yaml) ===\n")
            print(generate_did_yaml())


if __name__ == '__main__':
    main()
