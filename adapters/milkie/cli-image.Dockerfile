FROM node@sha256:363e1587494626837fa7f9a23bdb453d13b0ff3c67c705c2805cfc69c2d2fad7
WORKDIR /opt/atelier/adapter
COPY package.json package-lock.json ./
COPY vendor/ ./vendor/
RUN npm ci --omit=dev --no-audit --no-fund
RUN npm install --global --no-audit --no-fund @earendil-works/pi-coding-agent@0.85.1
COPY --chmod=755 grok /usr/local/bin/grok
ARG GROK_SHA256
RUN printf '%s  /usr/local/bin/grok\n' "$GROK_SHA256" | sha256sum -c - \
    && GROK_HOME=/tmp/grok-version grok --version \
    && PI_CODING_AGENT_DIR=/tmp/pi-version pi --version \
    && rm -rf /tmp/grok-version /tmp/pi-version
COPY dist/src/ ./dist/src/
LABEL atelier.milkie="e049f0b12479b07456e9c10acd709579ca3cd47f" atelier.adapter.protocol="2"
ENTRYPOINT ["node", "/opt/atelier/adapter/dist/src/main.js"]
