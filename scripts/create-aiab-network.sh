#!/bin/bash
# Create the "internal" network for Aviary-in-a-Box
# This network uses external DNS (CoreDNS) and DHCP (ISC DHCP Server)

set -e

NETWORK_NAME="internal"
BRIDGE_NAME="virbr-int"
NETWORK_ADDR="10.23.0.1"
NETMASK="255.255.0.0"
LIBVIRT_URI="qemu:///system"

echo "Creating Aviary-in-a-Box internal network..."

# Create network XML with DNS disabled
cat > /tmp/${NETWORK_NAME}-network.xml <<EOF
<network>
  <name>${NETWORK_NAME}</name>
  <uuid>4fa5bf4e-982c-4fc5-b7cd-f4e40de3d8bc</uuid>
  <forward mode='nat'>
    <nat>
      <port start='1024' end='65535'/>
    </nat>
  </forward>
  <bridge name='${BRIDGE_NAME}' stp='on' delay='0'/>
  <mac address='52:54:00:ba:c9:c0'/>
  <dns enable='no'/>
  <ip address='${NETWORK_ADDR}' netmask='${NETMASK}'>
  </ip>
</network>
EOF

# Check if network already exists
if virsh -c ${LIBVIRT_URI} net-info ${NETWORK_NAME} &>/dev/null; then
    echo "Network '${NETWORK_NAME}' already exists"
    echo "To recreate it, first run:"
    echo "  sudo virsh -c ${LIBVIRT_URI} net-destroy ${NETWORK_NAME}"
    echo "  sudo virsh -c ${LIBVIRT_URI} net-undefine ${NETWORK_NAME}"
    echo "Then run this script again"
    exit 1
fi

# Define network
echo "Defining network..."
virsh -c ${LIBVIRT_URI} net-define /tmp/${NETWORK_NAME}-network.xml

# Start network
echo "Starting network..."
virsh -c ${LIBVIRT_URI} net-start ${NETWORK_NAME}

# Enable autostart
echo "Enabling autostart..."
virsh -c ${LIBVIRT_URI} net-autostart ${NETWORK_NAME}

# Cleanup temp file
rm -f /tmp/${NETWORK_NAME}-network.xml

echo ""
echo "✅ Network '${NETWORK_NAME}' created successfully!"
echo ""
echo "Network info:"
virsh -c ${LIBVIRT_URI} net-info ${NETWORK_NAME}
echo ""
echo "Bridge status:"
ip addr show ${BRIDGE_NAME}
echo ""
echo "Note: This network uses external DNS and DHCP services:"
echo "  - DNS: CoreDNS (port 53)"
echo "  - DHCP: ISC DHCP Server (configured in /etc/dhcp/dhcpd.conf)"
echo ""
echo "Make sure these services are running:"
echo "  sudo systemctl status coredns"
echo "  sudo systemctl status isc-dhcp-server"
