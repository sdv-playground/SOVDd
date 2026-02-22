#!/bin/bash
# Setup virtual CAN interface for testing

set -e

INTERFACE="${1:-vcan0}"

echo "Setting up virtual CAN interface: $INTERFACE"

# Load vcan module
if ! lsmod | grep -q vcan; then
    echo "Loading vcan kernel module..."
    sudo modprobe vcan
fi

# Check if interface already exists
if ip link show "$INTERFACE" &>/dev/null; then
    echo "Interface $INTERFACE already exists"
else
    echo "Creating interface $INTERFACE..."
    sudo ip link add dev "$INTERFACE" type vcan
fi

# Bring interface up
echo "Bringing up $INTERFACE..."
sudo ip link set up "$INTERFACE"

# Verify
echo ""
echo "Interface status:"
ip link show "$INTERFACE"

echo ""
echo "Virtual CAN interface $INTERFACE is ready!"
echo ""
echo "Usage:"
echo "  1. Start example ECU: ./target/release/example-ecu --interface $INTERFACE"
echo "  2. Start SOVD server: ./target/release/sovd-server config/sovd.toml"
echo "  3. Monitor traffic:   candump $INTERFACE"
