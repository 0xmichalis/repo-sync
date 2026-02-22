FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY rust-toolchain.toml* rust-toolchain* ./
COPY src ./src

RUN cargo build --release

FROM alpine:3.21

RUN apk add --no-cache ca-certificates git && adduser -D -u 10001 appuser

WORKDIR /app
COPY --from=builder /app/target/release/repo-sync /usr/local/bin/repo-sync

RUN mkdir -p /data/repo && chown -R appuser:appuser /data
USER appuser

EXPOSE 8080

CMD ["repo-sync"]
