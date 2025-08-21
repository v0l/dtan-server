ARG IMAGE=rust:bookworm

FROM node:bookworm AS ui_builder
WORKDIR /src
RUN git clone https://github.com/v0l/dtan.git && \
    cd dtan && yarn && VITE_DTAN_SERVER=1 yarn build

FROM $IMAGE AS build
WORKDIR /src
COPY . .
RUN cargo install --path . --root /app/build

FROM $IMAGE AS runner
WORKDIR /app
COPY --from=build /app/build .
COPY --from=ui_builder /src/dtan/dist www
ENV RUST_LOG=info
ENTRYPOINT ["./bin/dtan-server"]