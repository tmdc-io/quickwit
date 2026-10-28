#!/usr/bin/env bash
# Refresh @sha256 pins for official Docker Hub base images used in Dockerfiles.
# Requires: docker buildx, jq
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v docker >/dev/null || ! docker buildx version >/dev/null 2>&1; then
	echo "error: docker buildx is required" >&2
	exit 1
fi
if ! command -v jq >/dev/null; then
	echo "error: jq is required" >&2
	exit 1
fi

hub_digest() {
	local ref="$1"
	docker buildx imagetools inspect "$ref" --format '{{json .}}' | jq -r '.manifest.digest'
}

pin_from_line() {
	local file="$1"
	local image_tag="$2"
	local hub_ref="$3"

	if [[ ! -f "$file" ]]; then
		echo "skip: $file (not found)"
		return 0
	fi

	local digest
	digest="$(hub_digest "$hub_ref")"
	if [[ -z "$digest" || "$digest" == "null" ]]; then
		echo "error: could not resolve digest for $hub_ref" >&2
		exit 1
	fi

	# image_tag is escaped for use in the regex (only [a-z0-9:./-] today).
	local before
	before="$(grep -E "^FROM ${image_tag}@sha256:" "$file" | head -1 || true)"
	perl -pi -e "s/^FROM ${image_tag}\@sha256:[a-f0-9]+/FROM ${image_tag}\@${digest}/g" "$file"
	local after
	after="$(grep -E "^FROM ${image_tag}@sha256:" "$file" | head -1 || true)"

	if [[ "$before" == "$after" ]]; then
		echo "unchanged: $file ($image_tag already at $digest)"
	else
		echo "updated:  $file ($image_tag -> $digest)"
	fi
}

echo "Resolving Docker Hub manifest digests..."
pin_from_line Dockerfile node:24 node:24
pin_from_line Dockerfile rust:bookworm rust:bookworm
pin_from_line Dockerfile debian:bookworm-slim debian:bookworm-slim
pin_from_line distribution/docker/ubuntu/Dockerfile ubuntu:noble ubuntu:noble

echo "Done. Rebuild the image and rescan for OS CVEs after Debian publishes fixes."
