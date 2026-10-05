ARG QEMU_BUILDER=localhost/origami-qemu-windows-builder:dev
FROM ${QEMU_BUILDER}

# vcpkg builds its host tools natively and some ports with autotools.
RUN dnf --quiet install -y cargo cmake golang gcc-c++ autoconf automake libtool autoconf-archive \
 && dnf --quiet clean all
