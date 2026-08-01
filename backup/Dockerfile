FROM docker.io/alpine:3.22
# restic 0.18.0 in alpine 3.22 (>= 0.14 required for R2). rcon-cli from itzg.
ARG TARGETARCH=arm64
ARG RCON_CLI_VERSION=1.7.6
RUN apk add --no-cache restic curl \
 && curl -fsSL "https://github.com/itzg/rcon-cli/releases/download/${RCON_CLI_VERSION}/rcon-cli_${RCON_CLI_VERSION}_linux_${TARGETARCH}.tar.gz" \
    | tar xz -C /usr/local/bin rcon-cli \
 && apk del curl
COPY backup.sh /usr/local/bin/backup.sh
ENTRYPOINT ["/bin/sh", "/usr/local/bin/backup.sh"]
