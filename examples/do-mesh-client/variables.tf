# Inputs: docs/network/clients.md, "A cloud server as a client".

variable "name" {
  description = "Server name; also the client's name on the hub (xdctl mesh client add --name)."
  type        = string
}

variable "box" {
  description = "Mesh box number for this client (1-254, free on the hub: xdctl mesh ls)."
  type        = number
  validation {
    condition     = var.box >= 1 && var.box <= 254
    error_message = "box must be 1-254 (docs/network/clients.md)."
  }
}

variable "hub_pubkey" {
  description = "The hub's WireGuard public key (xdctl mesh wg on the hub)."
  type        = string
}

variable "hub_public_ip" {
  description = "The address the hub's traffic comes from on the internet (the home line). Only it may reach WireGuard and ssh."
  type        = string
}

variable "ssh_key_ids" {
  description = "DigitalOcean ssh key ids or fingerprints for root."
  type        = list(string)
}

variable "region" {
  type    = string
  default = "nyc3"
}

variable "size" {
  type    = string
  default = "s-1vcpu-1gb"
}

variable "image" {
  type    = string
  default = "debian-13-x64"
}

variable "listen_port" {
  description = "WireGuard port on the client (the hub dials it)."
  type        = number
  default     = 51820
}
