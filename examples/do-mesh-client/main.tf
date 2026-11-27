# The server, its firewall and its WireGuard setup.
# Design: docs/network/clients.md, "A cloud server as a client".

resource "wireguard_asymmetric_key" "client" {}

locals {
  # docs/network/clients.md: a client is 10.22.BOX.1 and talks to the
  # hub (10.22.0.1) and, through it, the overlay (10.23.0.0/16).
  mesh_address = "10.22.${var.box}.1"
  wg_conf = templatefile("${path.module}/wg-flock.conf.tftpl", {
    private_key = wireguard_asymmetric_key.client.private_key
    address     = local.mesh_address
    listen_port = var.listen_port
    hub_pubkey  = var.hub_pubkey
  })
}

resource "digitalocean_droplet" "client" {
  name     = var.name
  region   = var.region
  size     = var.size
  image    = var.image
  ssh_keys = var.ssh_key_ids
  user_data = templatefile("${path.module}/cloud-init.yaml.tftpl", {
    wg_conf       = local.wg_conf
    listen_port   = var.listen_port
    hub_public_ip = var.hub_public_ip
  })
}

# First layer: DigitalOcean's firewall. Second: ufw on the server (cloud-init).
# docs/network/clients.md, "A cloud server as a client".
resource "digitalocean_firewall" "client" {
  name        = "${var.name}-mesh-client"
  droplet_ids = [digitalocean_droplet.client.id]

  inbound_rule {
    protocol         = "udp"
    port_range       = tostring(var.listen_port)
    source_addresses = ["${var.hub_public_ip}/32"]
  }
  inbound_rule {
    protocol         = "tcp"
    port_range       = "22"
    source_addresses = ["${var.hub_public_ip}/32"]
  }

  outbound_rule {
    protocol              = "tcp"
    port_range            = "1-65535"
    destination_addresses = ["0.0.0.0/0", "::/0"]
  }
  outbound_rule {
    protocol              = "udp"
    port_range            = "1-65535"
    destination_addresses = ["0.0.0.0/0", "::/0"]
  }
  outbound_rule {
    protocol              = "icmp"
    destination_addresses = ["0.0.0.0/0", "::/0"]
  }
}
