FROM rust:1-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383 AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock types.json ./
COPY src ./src
RUN cargo build --release --locked

FROM alpine:3@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf
# apk upgrade : applique les correctifs de sécurité publiés depuis la date du digest épinglé
# (openssl, musl, zlib, util-linux...). Le digest est relevé par renovate/dependabot.
RUN apk upgrade --no-cache \
    && apk add --no-cache ca-certificates \
    && adduser -D -u 1000 nbmcp
COPY --from=builder /src/target/release/nbmcp /usr/local/bin/nbmcp
USER nbmcp
EXPOSE 8000
# Image pensée pour le mode HTTP ; en stdio : docker run -i --rm … nbmcp
ENTRYPOINT ["nbmcp"]
CMD ["--transport", "http", "--host", "0.0.0.0", "--port", "8000"]
