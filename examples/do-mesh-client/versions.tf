# A cloud server as a secure mesh client that the hub dials:
# docs/network/clients.md.
terraform {
  required_version = ">= 1.6"
  required_providers {
    digitalocean = {
      source  = "digitalocean/digitalocean"
      version = "~> 2.40"
    }
    # The client's WireGuard key, made here so its public half is known to
    # the hub before the server boots ("Keys and Terraform state").
    wireguard = {
      source  = "OJFord/wireguard"
      version = "~> 0.3"
    }
  }
}
