FROM localhost/sgi-win64-builder:dev

RUN dnf --quiet install -y cargo cmake && dnf --quiet clean all
