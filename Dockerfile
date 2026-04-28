FROM oven/bun AS ui_builder
WORKDIR /src
RUN apt-get update && apt-get install -y git && \
    git clone https://github.com/v0l/dtan.git && \
    cd dtan && bun install && VITE_DTAN_SERVER=1 bun run build

FROM rust:trixie AS build
WORKDIR /src
COPY . .
RUN cargo install --path . --root /app/build

FROM debian:trixie-slim
WORKDIR /app
COPY --from=build /app/build .
COPY --from=ui_builder /src/dtan/dist www
## Install runtime libs
RUN apt update && \
    apt install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*
ENV RUST_LOG=info
ENTRYPOINT ["./bin/dtan-server"]