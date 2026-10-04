ARG QEMU_BUILDER=localhost/origami-qemu-windows-builder:dev
FROM docker.io/library/golang:1.27.1-trixie AS go-toolchain
FROM ${QEMU_BUILDER}

RUN dnf --quiet install -y cargo cmake && dnf --quiet clean all
COPY --from=go-toolchain /usr/local/go /usr/local/go
ENV PATH="/usr/local/go/bin:${PATH}"
ENV GOTOOLCHAIN=local
