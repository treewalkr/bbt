# syntax=docker/dockerfile:1

FROM rust:1.96-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p bbt-server \
    && cp target/release/bbt-server /usr/local/bin/bbt-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data bbt \
    && mkdir /data && chown bbt /data
COPY --from=build /usr/local/bin/bbt-server /usr/local/bin/bbt-server
USER bbt
ENV BBT_DB=/data/bbt.db
VOLUME /data
EXPOSE 4781
# 0.0.0.0 inside the container only; compose publishes the port on the host's loopback.
CMD ["bbt-server", "serve", "--host", "0.0.0.0"]
