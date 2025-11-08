# syntax=docker/dockerfile:1.4

FROM rustlang/rust:nightly as builder
WORKDIR /workspace

# Copy full workspace sources
COPY . .

RUN cargo build --release -p router

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace

COPY --from=builder /workspace/target/release/router /usr/local/bin/router
COPY configs /workspace/configs

EXPOSE 9099
ENTRYPOINT ["router"]
