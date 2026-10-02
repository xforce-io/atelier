ARG BASE_IMAGE=mcr.microsoft.com/playwright@sha256:f1e7e01021efd65dd1a2c56064be399f3e4de00fd021ac561325f2bfbb2b837a
FROM ${BASE_IMAGE}
WORKDIR /checks
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY check.mjs ./
USER pwuser
ENTRYPOINT ["node", "/checks/check.mjs", "/candidate"]
