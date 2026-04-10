# syntax=docker/dockerfile:1

# Zeptoclaw sandbox image for OpenShell
#
# Expects the pre-built zeptoclaw binary in the build context.
# Use the top-level install.sh to build and deploy.

ARG BASE_IMAGE=ghcr.io/nvidia/openshell-community/sandboxes/base:latest
FROM ${BASE_IMAGE}

USER root

COPY zeptoclaw /usr/local/bin/zeptoclaw
RUN chmod 755 /usr/local/bin/zeptoclaw

# jq: entrypoint uses it to assemble config.json from env vars
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates jq \
    && rm -rf /var/lib/apt/lists/*

COPY policy.yaml /etc/openshell/policy.yaml
COPY zeptoclaw-start.sh /usr/local/bin/zeptoclaw-start
RUN chmod +x /usr/local/bin/zeptoclaw-start

COPY config.sandbox.json /etc/zeptoclaw/config.sandbox.json
COPY mcp-servers.json /etc/zeptoclaw/mcp-servers.json

# Workspace bootstrap files and skills
COPY workspace-defaults/ /etc/zeptoclaw/workspace-defaults/

RUN mkdir -p /sandbox/.zeptoclaw/workspace \
             /sandbox/.zeptoclaw/sessions \
             /sandbox/.zeptoclaw/logs \
             /sandbox/.zeptoclaw/cron \
             /sandbox/.zeptoclaw/memory \
             /sandbox/skills && \
    chown -R sandbox:sandbox /sandbox/.zeptoclaw /sandbox/skills

USER sandbox

ENV RUST_LOG=zeptoclaw=info
ENV HOME=/sandbox

ENTRYPOINT ["/bin/bash"]
