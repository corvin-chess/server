# CV-199: production image for Compose / self-host (AGPL-3.0-or-later).
# Multi-stage so the runtime image has only the binary + CA certs.

FROM rust:bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release -p corvin-server

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates wget \
  && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/corvin-server /usr/local/bin/corvin-server
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENV HOST=0.0.0.0
ENV PORT=3000
EXPOSE 3000
HEALTHCHECK --interval=5s --timeout=3s --retries=10 --start-period=20s \
  CMD wget -qO- http://127.0.0.1:3000/health | grep -q ok
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
