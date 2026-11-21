FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc binutils
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY build/choose.c /tmp/choose.c
RUN cc -O2 -march=x86-64 -c /tmp/choose.c -o /tmp/choose.o \
 && cargo rustc --locked --release --target x86_64-unknown-linux-musl \
 && cp target/x86_64-unknown-linux-musl/release/pipeline /base \
 && cargo rustc --locked --release --target x86_64-unknown-linux-musl -- -Ctarget-feature=+avx512ifma \
        -Clink-arg=/tmp/choose.o -Clink-arg=-Wl,--wrap=main \
        -Clink-arg="$(cc -print-libgcc-file-name)" -Clink-arg=-lc \
 && cp target/x86_64-unknown-linux-musl/release/pipeline /pipeline \
 && strip /pipeline /base

FROM scratch
COPY --from=build /pipeline /pipeline
COPY --from=build /base /base
USER 1000:1000
CMD ["/pipeline"]
