ARG QEMU_BUILDER=localhost/origami-qemu-windows-builder:dev
FROM ${QEMU_BUILDER}

RUN dnf --quiet install -y cargo cmake golang && dnf --quiet clean all
