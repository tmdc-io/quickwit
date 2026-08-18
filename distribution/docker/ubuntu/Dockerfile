FROM ubuntu:noble@sha256:66460d557b25769b102175144d538d88219c077c678a49af4afca6fbfc1b5252 AS builder

RUN apt-get update && apt-get install -y curl
RUN curl -L https://install.quickwit.io | sh


FROM ubuntu:noble@sha256:66460d557b25769b102175144d538d88219c077c678a49af4afca6fbfc1b5252 AS quickwit

LABEL org.opencontainers.image.title="Quickwit"
LABEL maintainer="Quickwit, Inc. <hello@quickwit.io>"
LABEL org.opencontainers.image.vendor="Quickwit, Inc."
LABEL org.opencontainers.image.licenses="Apache-2.0"

RUN apt-get -y update \
    && apt-get -y install ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --system quickwit \
    && useradd --system --gid quickwit --home-dir /quickwit --shell /usr/sbin/nologin quickwit

WORKDIR /quickwit
RUN mkdir config qwdata \
    && chown -R quickwit:quickwit /quickwit
COPY --from=builder /quickwit-v*/quickwit /usr/local/bin/quickwit
COPY --from=builder /quickwit-v*/config/quickwit.yaml /quickwit/config/quickwit.yaml

ENV QW_CONFIG=/quickwit/config/quickwit.yaml
ENV QW_DATA_DIR=/quickwit/qwdata
ENV QW_LISTEN_ADDRESS=0.0.0.0

RUN quickwit --version

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD quickwit --version || exit 1

USER quickwit

ENTRYPOINT ["quickwit"]
