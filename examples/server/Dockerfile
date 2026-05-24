FROM rust:1-slim-bookworm AS build

WORKDIR /app
COPY . .
RUN cargo build --release -p codag-drain-server

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/codag-drain-server /usr/local/bin/codag-drain-server

ENV RUST_LOG=codag_drain_server=info,tower_http=info
EXPOSE 8088

ENTRYPOINT ["/usr/local/bin/codag-drain-server"]
