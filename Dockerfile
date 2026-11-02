# syntax=docker/dockerfile:1
# Azad's release (decision 0029): static binaries, an image that holds only azad, and an image to run azad in.
#   docker build --target dist --output type=local,dest=dist .   every binary, in dist/
#   docker build --target bin .                                  an image to copy azad from
#   docker build .                                               an image to run azad in

# Alpine's C library is musl, so the C code that ring and blake3 compile links statically, as Rust does.
FROM rust:1.99.1-alpine@sha256:7e9e2d068a107a2adafd19930011178d14a78326053cd4c3a9f7ad3fd049557d AS build
ENV RUSTUP_TOOLCHAIN=1.99.1 \
    CARGO_PROFILE_RELEASE_STRIP=symbols
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=azad-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=azad-target,target=/src/target <<-EOF
	set -eu
	cargo build --release --locked -p azad-cli -p azad-gha-runner -p azad-guest
	mkdir /out
	cd target/release
	cp azad azad-gha-runner azad-executor azad-guest azad-orchestrator /out/
EOF

FROM scratch AS dist
COPY --from=build /out/ /

FROM scratch AS bin
COPY --from=build /out/azad /azad

# Isolated runs need bubblewrap, which makes user namespaces and mounts its own /proc. Docker's default profiles forbid
# both, so run the container with `--security-opt seccomp=unconfined --security-opt apparmor=unconfined --security-opt
# systempaths=unconfined`, or `--privileged`. Azad keeps its store in /root/.cache; a volume there keeps results.
FROM alpine:3.24@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a AS azad
RUN apk add --no-cache bubblewrap ca-certificates git
COPY --from=bin / /usr/local/bin/
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/azad"]
