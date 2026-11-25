# What to do with the new server: docs/how-to/cloud-mesh-client.md,
# "3. Register it on the hub".

output "public_ip" {
  value = digitalocean_droplet.client.ipv4_address
}

output "mesh_address" {
  value = local.mesh_address
}

output "client_pubkey" {
  value = wireguard_asymmetric_key.client.public_key
}

output "hub_command" {
  description = "Run on the hub to register the client and have the hub dial it."
  value = join(" ", [
    "xdctl mesh client add",
    "--box ${var.box}",
    "--pubkey ${wireguard_asymmetric_key.client.public_key}",
    "--name ${var.name}",
    "--endpoint ${digitalocean_droplet.client.ipv4_address}:${var.listen_port}",
  ])
}
