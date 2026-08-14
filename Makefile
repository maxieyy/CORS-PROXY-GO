BINARY=m3u8-proxy

.PHONY: build run test fmt vet

build:
	go build -trimpath -ldflags="-s -w" -o $(BINARY) ./cmd/m3u8-proxy

run:
	go run ./cmd/m3u8-proxy

test:
	go test ./...

fmt:
	gofmt -w ./cmd ./internal

vet:
	go vet ./...
