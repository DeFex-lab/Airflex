# AirFlex API Server - multi-stage Dockerfile
# Build context must be the repository root because the server is a pnpm workspace package.
FROM node:20-alpine AS builder

WORKDIR /app
RUN corepack enable

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY server/package.json ./server/package.json
COPY packages/shared/package.json ./packages/shared/package.json

RUN pnpm install --frozen-lockfile --filter airflex-server...

COPY server/ ./server/
COPY packages/shared/ ./packages/shared/

WORKDIR /app/server
RUN pnpm build

FROM node:20-alpine AS production

RUN addgroup -S airflex && adduser -S airflex -G airflex

WORKDIR /app
RUN corepack enable

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY server/package.json ./server/package.json
COPY packages/shared/package.json ./packages/shared/package.json

RUN pnpm install --prod --frozen-lockfile --filter airflex-server... && pnpm store prune

COPY --from=builder /app/server/dist ./server/dist
COPY --from=builder /app/packages/shared/dist ./packages/shared/dist

USER airflex
EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -qO- http://localhost:3001/health || exit 1

WORKDIR /app/server
CMD ["node", "dist/index.js"]
