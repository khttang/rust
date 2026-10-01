# Validation image for running agent-harness under openshell-policy.yaml.
#
# Built by validate-openshell.sh from a staged context holding only the
# aarch64-linux binary (built in rust:1.90-trixie; the base must be trixie too so glibc matches).
# curl is included only so the validation can show that binaries other than
# /app/agent-harness are denied egress; drop it for a production image.

FROM debian:trixie-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system sandbox \
 && useradd --system --gid sandbox --home-dir /tmp --shell /usr/sbin/nologin sandbox

COPY agent-harness /app/agent-harness
RUN chmod 0555 /app/agent-harness

USER sandbox
WORKDIR /tmp
