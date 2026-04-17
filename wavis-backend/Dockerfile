FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy workspace manifests first (for layer caching)
COPY Cargo.toml Cargo.lock* ./
COPY shared/Cargo.toml shared/Cargo.toml
COPY wavis-backend/Cargo.toml wavis-backend/Cargo.toml

# Copy source code
COPY shared/src/ shared/src/
COPY wavis-backend/src/ wavis-backend/src/
COPY wavis-backend/migrations/ wavis-backend/migrations/

# Overwrite workspace Cargo.toml to include only the crates needed for the backend build
RUN printf '[workspace]\nresolver = "3"\nmembers = ["shared", "wavis-backend"]\n' > Cargo.toml

RUN cargo build --release -p wavis-backend

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*

# Run as non-root user
RUN groupadd --gid 1001 wavis && useradd --uid 1001 --gid wavis --shell /bin/false wavis

COPY --from=builder /app/target/release/wavis-backend /usr/local/bin/wavis-backend

USER wavis

ENV RUST_LOG=info
EXPOSE 3000

CMD ["wavis-backend"]
