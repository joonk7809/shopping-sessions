FROM rust:1.91-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
ENV RUSTFLAGS="-C target-cpu=x86-64-v4 -C target-feature=+crt-static"
RUN cargo build --release --locked --target x86_64-unknown-linux-gnu && cp target/x86_64-unknown-linux-gnu/release/ss /app

FROM scratch
COPY --from=build /app /app
ENTRYPOINT ["/app"]
