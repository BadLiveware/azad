# syntax=docker/dockerfile:1
# Builds the guest kernel of Azad's microVM sandbox: Cloud Hypervisor's guest configuration with azad.config merged
# over it.
# docker build --output type=local,dest=<dir> guest/kernel
FROM debian:trixie-slim AS build
SHELL ["/bin/bash", "-euo", "pipefail", "-c"]
RUN <<-EOF
	apt-get update
	apt-get install -y --no-install-recommends bc bison ca-certificates curl flex gcc libc6-dev libelf-dev libssl-dev make xz-utils
	rm -rf /var/lib/apt/lists/*
EOF

ARG KERNEL_VERSION=6.18.59
WORKDIR /build
RUN curl -fsSL "https://cdn.kernel.org/pub/linux/kernel/v${KERNEL_VERSION%%.*}.x/linux-${KERNEL_VERSION}.tar.xz" | tar -xJ --strip-components=1

# The base is arch/x86/configs/ch_defconfig of https://github.com/cloud-hypervisor/linux, branch ch-7.2.8: the guest
# kernel that Cloud Hypervisor v54.0 tests with (its scripts/test-util.sh). It is a file of the GPL-2.0 kernel tree, so
# the build downloads it at a fixed revision and checksum instead of keeping a copy here.
ARG BASE_CONFIG_REVISION=639d0886248cd90799b0b590f7a62a1a3565a815
ARG BASE_CONFIG_SHA256=3e665b1b69e3376063bc9f9d56cbcfd504b0655226eca96ccd8d152c2bfa4af0
RUN <<-EOF
	curl -fsSL -o base.config "https://raw.githubusercontent.com/cloud-hypervisor/linux/${BASE_CONFIG_REVISION}/arch/x86/configs/ch_defconfig"
	echo "${BASE_CONFIG_SHA256}  base.config" | sha256sum -c -
EOF

COPY --link azad.config /build/
# olddefconfig drops an option whose dependencies are missing without a word, so every option of azad.config is
# checked after the merge.
RUN <<-EOF
	KCONFIG_CONFIG=.config ./scripts/kconfig/merge_config.sh -m base.config azad.config
	make olddefconfig
	wrong=0
	for option in $(sed -n 's/^CONFIG_\([A-Za-z0-9_]*\)=y$/\1/p' azad.config); do
		[ "$(./scripts/config --file .config --state "CONFIG_${option}")" = y ] || { echo "CONFIG_${option} is not set"; wrong=1; }
	done
	for option in $(sed -n 's/^# CONFIG_\([A-Za-z0-9_]*\) is not set$/\1/p' azad.config); do
		[ "$(./scripts/config --file .config --state "CONFIG_${option}")" = n ] || { echo "CONFIG_${option} is set"; wrong=1; }
	done
	[ "${wrong}" = 0 ]
	make -j"$(nproc)" vmlinux
	cp .config kernel.config
EOF

FROM scratch
COPY --from=build /build/vmlinux /vmlinux
COPY --from=build /build/kernel.config /kernel.config
