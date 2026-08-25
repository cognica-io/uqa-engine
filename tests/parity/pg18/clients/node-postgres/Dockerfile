FROM public.ecr.aws/docker/library/node:22-bookworm-slim

WORKDIR /matrix
COPY package.json package-lock.json client.mjs ./
RUN npm ci --omit=dev --no-audit --no-fund

ENTRYPOINT ["node", "/matrix/client.mjs"]
