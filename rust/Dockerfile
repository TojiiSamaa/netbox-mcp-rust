FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock types.json ./
COPY src ./src
RUN cargo build --release --locked

FROM alpine:3
RUN apk add --no-cache ca-certificates && adduser -D -u 1000 nbmcp
COPY --from=builder /src/target/release/nbmcp /usr/local/bin/nbmcp
USER nbmcp
EXPOSE 8000
# Image pensée pour le mode HTTP ; en stdio : docker run -i --rm … nbmcp
ENTRYPOINT ["nbmcp"]
CMD ["--transport", "http", "--host", "0.0.0.0", "--port", "8000"]
