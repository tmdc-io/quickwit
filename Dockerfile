# Base image digests: run `make update-docker-base-digests` before release builds.
FROM node:24@sha256:df02654fe43f6c43191e9eed5b3a54246d41dae8366028893c195b7fc00c745b AS ui-builder

COPY quickwit/quickwit-ui /quickwit/quickwit-ui

WORKDIR /quickwit/quickwit-ui

RUN touch .gitignore_for_build_directory \
    && NODE_ENV=production make install build


FROM rust:bookworm@sha256:e9da6f6c3ed5095fd5d33bf7cdf309830a178a8147888f1b2f76d5769bd9306c AS bin-builder

ARG CARGO_FEATURES=release-feature-set
ARG CARGO_PROFILE=release
ARG QW_COMMIT_DATE
ARG QW_COMMIT_HASH
ARG QW_COMMIT_TAGS
ARG LAMBDA_ZIP_PATH

ENV QW_COMMIT_DATE=$QW_COMMIT_DATE
ENV QW_COMMIT_HASH=$QW_COMMIT_HASH
ENV QW_COMMIT_TAGS=$QW_COMMIT_TAGS
ENV LAMBDA_ZIP_PATH=$LAMBDA_ZIP_PATH

RUN apt-get -y update \
    && apt-get -y upgrade \
    && apt-get -y install --no-install-recommends \
    ca-certificates \
    clang \
    cmake \
    libssl-dev \
    llvm \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

COPY quickwit /quickwit
COPY config/quickwit.yaml /quickwit/config/quickwit.yaml
COPY --from=ui-builder /quickwit/quickwit-ui/build /quickwit/quickwit-ui/build

WORKDIR /quickwit

RUN rustup toolchain install

RUN echo "Building workspace with feature(s) '$CARGO_FEATURES' and profile '$CARGO_PROFILE'" \
    && RUSTFLAGS="--cfg tokio_unstable" \
    cargo build \
    -p quickwit-cli \
    --features $CARGO_FEATURES \
    --bin quickwit \
    $(test "$CARGO_PROFILE" = "release" && echo "--release") \
    && echo "Copying binaries to /quickwit/bin" \
    && mkdir -p /quickwit/bin \
    && find target/$CARGO_PROFILE -maxdepth 1 -perm /a+x -type f -exec mv {} /quickwit/bin \;


FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS quickwit

LABEL org.opencontainers.image.title="Quickwit"
LABEL maintainer="Quickwit, Inc. <hello@quickwit.io>"
LABEL org.opencontainers.image.vendor="Quickwit, Inc."
LABEL org.opencontainers.image.licenses="Apache-2.0"

RUN apt-get -y update \
    && apt-get -y upgrade \
    && apt-get -y install --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /quickwit
RUN mkdir config qwdata
COPY --from=bin-builder /quickwit/bin/quickwit /usr/local/bin/quickwit
COPY --from=bin-builder /quickwit/config/quickwit.yaml /quickwit/config/quickwit.yaml

ENV QW_CONFIG=/quickwit/config/quickwit.yaml
ENV QW_DATA_DIR=/quickwit/qwdata
ENV QW_LISTEN_ADDRESS=0.0.0.0

RUN quickwit --version \
    && groupadd --system quickwit \
    && useradd --system --gid quickwit --home-dir /quickwit --shell /usr/sbin/nologin quickwit \
    && chown -R quickwit:quickwit /quickwit

USER quickwit

ENTRYPOINT ["quickwit"]
