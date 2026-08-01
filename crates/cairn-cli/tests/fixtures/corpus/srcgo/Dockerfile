# The hop the Go side needs: compose starts `/bin/collector`, and only this file says
# which source that binary was built from. Nothing here is ever built — the corpus exists
# to be read, not run.
FROM golang:1.22-bookworm AS builder
WORKDIR /src
COPY . .
RUN go build -o /out/collector ./cmd/collector

FROM gcr.io/distroless/base-debian12
COPY --from=builder /out/collector /bin/collector
EXPOSE 9101
ENTRYPOINT ["/bin/collector"]
