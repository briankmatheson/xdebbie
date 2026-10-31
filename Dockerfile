# SPDX-License-Identifier: GPL-3.0-or-later
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p capq

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/capq /capq
USER 65532:65532
ENTRYPOINT ["/capq"]
