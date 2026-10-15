# Convenience targets. Each component also works with its native tooling directly.
.PHONY: test lint build up down

test:
	cd server && go test -race ./...
	cd agent && cargo test --all
	cd viewer && npm test

lint:
	cd server && test -z "$$(gofmt -l .)" && go vet ./...
	cd agent && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings
	cd viewer && npm run typecheck

build:
	cd server && go build -o bin/carson-server ./cmd/carson-server
	cd agent && cargo build --release
	cd viewer && npm run build

up:
	docker compose -f deploy/docker-compose.yml --env-file .env up --build

down:
	docker compose -f deploy/docker-compose.yml --env-file .env down
