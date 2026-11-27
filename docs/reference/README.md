# Reference

Exact names, defaults and formats, checked against the code for 0.23.0.

- [Configuration file](configuration.md): every key in
  `/etc/xdebbie/xdebbied.toml`, its default and meaning.
- [xdctl commands](xdctl.md): every command and flag.
- [xdebbied commands](xdebbied.md): `serve`, `bootstrap`, `check`.
- [REST API](api.md): every route, who may call it, what it returns.
- [The mesh spec and status](mesh-spec.md): the `/v1/mesh` document.
- [Files, ports and interfaces](files-and-ports.md): what is on a box, what
  it listens on, what the package installs.
- [Limits and max values](limits.md): every limit and timing, and what is
  not done yet.
- [Troubleshooting](troubleshooting.md): by symptom.

Commands are also in the binaries: `xdctl --help`, `xdctl COMMAND --help`,
`xdebbied --help`, `capx --help`. capx's options are in
[capx and Cluster API](../compute/kubernetes.md#running-capx).
