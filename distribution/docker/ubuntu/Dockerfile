FROM ubuntu:noble@sha256:24737cef17f4e22b1977df7d9a05e77c4e28b7da5b8911f8d8ccda136bf3573c AS builder

RUN apt-get update \
    && apt-get -y upgrade \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*
RUN curl -L https://raw.githubusercontent.com/quickwit-oss/quickwit/main/install.sh | sh


FROM ubuntu:noble@sha256:24737cef17f4e22b1977df7d9a05e77c4e28b7da5b8911f8d8ccda136bf3573c AS quickwit

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
COPY --from=builder /quickwit-v*/quickwit /usr/local/bin/quickwit
COPY --from=builder /quickwit-v*/config/quickwit.yaml /quickwit/config/quickwit.yaml

ENV QW_CONFIG=/quickwit/config/quickwit.yaml
ENV QW_DATA_DIR=/quickwit/qwdata
ENV QW_LISTEN_ADDRESS=0.0.0.0

RUN quickwit --version \
    && groupadd --system quickwit \
    && useradd --system --gid quickwit --home-dir /quickwit --shell /usr/sbin/nologin quickwit \
    && chown -R quickwit:quickwit /quickwit

USER quickwit

ENTRYPOINT ["quickwit"]
