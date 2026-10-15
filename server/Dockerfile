# Build context: repository root.
FROM node:24-alpine AS viewer
WORKDIR /src/viewer
COPY viewer/package.json viewer/package-lock.json ./
RUN npm ci
COPY viewer/ ./
COPY proto/ /src/proto/
RUN npm run build

FROM golang:1.27-alpine AS server
WORKDIR /src/server
COPY server/go.mod server/go.sum ./
RUN go mod download
COPY server/ ./
ARG VERSION=dev
RUN CGO_ENABLED=0 go build -trimpath \
      -ldflags "-s -w -X carson/server/internal/httpapi.Version=${VERSION}" \
      -o /out/carson-server ./cmd/carson-server
# Pre-create the data dir so the named volume inherits nonroot ownership.
RUN mkdir -p /out/data

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=server /out/carson-server /carson-server
COPY --from=viewer /src/viewer/dist /web
COPY --from=server --chown=65532:65532 /out/data /data
ENV CARSON_WEB_DIR=/web CARSON_DB_PATH=/data/carson.db
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/carson-server"]
