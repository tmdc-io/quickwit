# Building the Quickwit Docker Image (TMDC)

This document describes how to build and publish the TMDC Quickwit image
`docker.io/tmdcio/quickwit`.

## Prerequisites

- Docker with [Buildx](https://docs.docker.com/build/buildx/) enabled
- Git
- Make

## Local build

From the repository root:

```bash
make build-tmdc-docker GITHUB_TAGS=0.8.0-d1
```

This builds:

```text
docker.io/tmdcio/quickwit:0.8.0-d1
```

### Optional variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_TAGS` | current git branch name | Image tag |
| `PLATFORM` | `linux/amd64` | Target platform |
| `TMDC_IMAGE` | `docker.io/tmdcio/quickwit` | Image name |
| `DOCKER_BUILD_ARGS` | _(empty)_ | Extra `docker buildx build` args |

Examples:

```bash
# Explicit tag and platform
make build-tmdc-docker GITHUB_TAGS=0.8.0-d1 PLATFORM=linux/amd64

# Custom image name
make build-tmdc-docker GITHUB_TAGS=0.8.0-d1 TMDC_IMAGE=docker.io/tmdcio/quickwit
```

The build uses the root `Dockerfile` and injects commit metadata:

- `QW_COMMIT_DATE`
- `QW_COMMIT_HASH`
- `QW_COMMIT_TAGS`

## Push locally

Log in to Docker Hub, then push:

```bash
docker login
make push-tmdc-docker GITHUB_TAGS=0.8.0-d1
```

## CI build (recommended)

The workflow [`.github/workflows/tmdc-docker-build-push.yaml`](.github/workflows/tmdc-docker-build-push.yaml)
builds and pushes the image when you push a git tag matching `*-d*`
(for example `0.8.0-d1` or `v0.8.0-d1`).

### Required GitHub secrets

Configure these repository secrets:

- `DOCKER_HUB_USERNAME`
- `DOCKER_HUB_PASSWORD`

### Trigger a build

```bash
git tag 0.8.0-d1
git push origin 0.8.0-d1
```

CI will:

1. Check out the tagged commit
2. Build `docker.io/tmdcio/quickwit:<tag>` for `linux/amd64`
3. Push the image to Docker Hub

### Tag pattern

| Tag | Triggers workflow? |
|-----|--------------------|
| `0.8.0-d1` | Yes |
| `v0.8.0-d1` | Yes |
| `0.8.0` | No |
| `v0.8.0` | No |

## Upstream-style local build

To build the upstream-tagged image (`quickwit/quickwit:<branch>`) without
pushing to TMDC:

```bash
make docker-build
```

## Run the image

```bash
docker run --rm -p 7280:7280 docker.io/tmdcio/quickwit:0.8.0-d1 --version
```
