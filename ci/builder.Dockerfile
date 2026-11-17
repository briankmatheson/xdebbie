# The image Drone builds xdebbie in (.drone.yml): rust plus what every run
# used to install first. Rebuild and push when the toolchain should move:
#   docker build -t registry.mesh.bmath.nyc/xdebbie-builder:1 -f ci/builder.Dockerfile ci
#   docker push registry.mesh.bmath.nyc/xdebbie-builder:1
FROM rust:1-bookworm
# cmake: aws-lc-sys (rustls's crypto). musl-tools: the static xdj.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake musl-tools \
 && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy \
 && rustup target add x86_64-unknown-linux-musl \
 && cargo install cargo-deb --locked \
 && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git"
