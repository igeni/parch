# Build image for Ubuntu 26.04 releases: native toolchain plus an x86_64 cross toolchain.
FROM ubuntu:26.04
ARG RUST_VERSION=1.93.0
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl build-essential dpkg-dev file \
      zip unzip libarchive-tools python3 \
      gcc-x86-64-linux-gnu libc6-dev-amd64-cross \
 && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION} \
 && rustup target add x86_64-unknown-linux-gnu \
 && chmod -R a+rX /opt/rustup /opt/cargo
