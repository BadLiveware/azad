# syntax=docker/dockerfile:1
# The image of Azad's own tasks (decision 0037): CircleCI's Rust image, because it has rustfmt and clippy, with
# bubblewrap, so that the sandbox tests start sandboxes inside Azad's sandbox, and with the targets of
# rust-toolchain.toml.
FROM cimg/rust:1.99.1@sha256:bc990fa20ef4af5b0e09c061000b84d873a5bc2900bb00235ff6346e3f86f3f5
USER root
RUN <<-EOF
	set -eu
	apt-get update
	apt-get install -y --no-install-recommends bubblewrap
	rm -rf /var/lib/apt/lists/*
EOF
USER circleci
RUN rustup target add --toolchain 1.99.1 x86_64-unknown-linux-musl wasm32-unknown-unknown
