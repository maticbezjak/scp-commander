#!/bin/sh
# linuxserver/openssh-server ships "AllowTcpForwarding no"; the jump-host test
# tunnels a direct-tcpip channel through this bastion, so enable forwarding.
# Runs as a /custom-cont-init.d script before sshd starts. The running daemon
# reads /config/sshd/sshd_config (a persistent volume), so edit that one.
for CONF in /config/sshd/sshd_config /etc/ssh/sshd_config; do
    [ -f "$CONF" ] || continue
    if grep -q '^AllowTcpForwarding' "$CONF"; then
        sed -i 's/^AllowTcpForwarding .*/AllowTcpForwarding yes/' "$CONF"
    else
        echo 'AllowTcpForwarding yes' >> "$CONF"
    fi
done
