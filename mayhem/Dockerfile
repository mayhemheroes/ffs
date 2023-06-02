# Credit to nottirb for the base-builder image and workflow
# base-builder image: https://github.com/nottirb/base-builders
# workflow example used: https://github.com/nottirb/ciborium/blob/main/mayhem/Dockerfile

# Set the project name
ARG project=tokay

# Use the base builder image for Rust projects as the builder stage
FROM ghcr.io/nottirb/fuzz-rust:latest as builder

# Install pkg-config and libssl-dev for fuzzing
RUN apt-get update && apt-get install -y pkg-config libssl-dev libfuse-dev libfuse2 fuse

# Add source to the build stage
ADD . /${project}
WORKDIR /${project}

# Inject lib.rs to expose the library for fuzzing
RUN mv mayhem/injected_lib.rs src/lib.rs

# Build the fuzz targets
RUN cd ./fuzz && cargo +nightly hfuzz build

# Fresh image for the fuzzing stage
FROM ghcr.io/nottirb/fuzz-rust:latest

# Install dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev libfuse-dev libfuse2 fuse

# Copy the fuzz targets from the builder stage
COPY --from=builder ${project}/fuzz/hfuzz_target/x86_64-unknown-linux-gnu/release/fuzz_json /
COPY --from=builder ${project}/fuzz/hfuzz_target/x86_64-unknown-linux-gnu/release/fuzz_toml /
COPY --from=builder ${project}/fuzz/hfuzz_target/x86_64-unknown-linux-gnu/release/fuzz_yaml /