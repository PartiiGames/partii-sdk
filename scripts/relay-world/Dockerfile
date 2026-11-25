# net-probe for the relay-world run (scripts/relay-world/run.sh). Built on
# Fly's remote builder from the partii-sdk root:
#   flyctl deploy --build-only --remote-only --push -c scripts/relay-world/fly.toml \
#     --dockerfile scripts/relay-world/Dockerfile --ignorefile scripts/relay-world/dockerignore
# No secrets in the image: PARTII_KEY is an app secret, set at machine start.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/examples/net-probe/target \
    cd examples/net-probe && cargo build --release --locked \
    && cp target/release/net-probe /usr/local/bin/net-probe

# Unit tests for what the probe relies on (partii-net, the rtc stats fix), on
# the builder too: flyctl deploy --build-only --build-target test ... (no push).
FROM build AS test
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo test --locked -p partii-net --lib > /tmp/net.txt 2>&1; rc=$?; tail -n 40 /tmp/net.txt; \
    cargo test --locked -p partii-rtc --lib statistics > /tmp/rtc.txt 2>&1; rc2=$?; tail -n 15 /tmp/rtc.txt; \
    test $rc = 0 && test $rc2 = 0

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl iptables iproute2 procps \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/net-probe /usr/local/bin/net-probe
COPY scripts/relay-world/agent.sh scripts/relay-world/probe.sh /usr/local/bin/
CMD ["/usr/local/bin/agent.sh"]
