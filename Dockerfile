ARG IMAGE=rust:bookworm

FROM $IMAGE AS build
WORKDIR /app/src
COPY . .
RUN cargo install --path . --root /app/build

FROM $IMAGE AS runner
WORKDIR /app
COPY --from=build /app/build .
ENV RUST_LOG=info
ENTRYPOINT ["./bin/dtan-server"]