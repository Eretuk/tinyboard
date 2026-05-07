# Build stage: musl target for a fully static binary
FROM rust:alpine AS builder

# No C dependencies — redb is pure Rust, no sqlite, no perl needed
RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# Runtime stage — only TLS certs and timezone data needed
FROM alpine:3.21

RUN apk add --no-cache ca-certificates tzdata

# Run as non-root user
RUN addgroup -S tinyboard && adduser -S -G tinyboard tinyboard

WORKDIR /app

RUN mkdir -p /app/defaults /data/tinyboard
COPY config.yaml /app/defaults/config.yaml
COPY board.yaml /app/defaults/board.yaml

COPY --from=builder /app/target/release/tinyboard /usr/local/bin/tinyboard
COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh \
    && chown -R tinyboard:tinyboard /app /data

USER tinyboard

EXPOSE 8849
ENTRYPOINT ["/app/entrypoint.sh"]
CMD ["-c", "/data/tinyboard/config.yaml", "-b", "/data/tinyboard/board.yaml"]
