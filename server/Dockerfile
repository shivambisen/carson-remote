# Build context: repository root (so later milestones can embed viewer/dist).
FROM golang:1.27-alpine AS build
WORKDIR /src/server
COPY server/go.mod ./
RUN go mod download
COPY server/ ./
ARG VERSION=dev
RUN CGO_ENABLED=0 go build -trimpath \
      -ldflags "-s -w -X carson/server/internal/httpapi.Version=${VERSION}" \
      -o /out/carson-server ./cmd/carson-server

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/carson-server /carson-server
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/carson-server"]
