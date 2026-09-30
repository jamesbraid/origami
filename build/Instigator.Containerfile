ARG BASE_IMAGE=docker.io/library/debian:forky-slim
FROM ${BASE_IMAGE}

RUN apt-get update && apt-get install -y --no-install-recommends \
    golang-go ca-certificates \
 && rm -rf /var/lib/apt/lists/*
