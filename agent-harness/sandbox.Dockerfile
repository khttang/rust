# Image for running agent-harness and the verified-fix task under
# openshell-policy.yaml.
#
# Built by validate-openshell.sh from a staged context holding the Linux
# binaries (built in rust:1.95, Debian trixie; the base must be trixie too so
# glibc matches) and the verified-fix corpus.
#
# - /app/agent-harness   the general CLI
# - /app/verified-fix    the verified-fix task CLI
# - /app/corpus          the task's corpus (read-only)
# - cbmc + gcc           CBMC_PACKAGE from images.env (cbmc pulls in gcc, its
#                        preprocessor); exact version so results reproduce
# - libc6-dev            LIBC_DEV_PACKAGE: C headers, so real C files (with
#                        #include <stdint.h> etc.) preprocess
# - curl                 only so validation can show that binaries other than
#                        the pinned ones are denied egress; drop it for production

# Pinned by digest (a multi-arch index) for reproducible builds.
FROM debian:trixie-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a

ARG CBMC_PACKAGE=cbmc=6.6.0-4
ARG LIBC_DEV_PACKAGE=libc6-dev=2.41-12+deb13u4

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl "$CBMC_PACKAGE" "$LIBC_DEV_PACKAGE" \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system sandbox \
 && useradd --system --gid sandbox --home-dir /tmp --shell /usr/sbin/nologin sandbox

COPY agent-harness verified-fix /app/
COPY corpus /app/corpus
RUN chmod 0555 /app/agent-harness /app/verified-fix \
 && chmod -R a+rX,go-w /app/corpus

USER sandbox
WORKDIR /tmp
