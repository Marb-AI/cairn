// Command collector is the entrypoint compose starts as the `telemetry-collector`
// service. Reaching it from a symbol is what proves the deployment chain: compose
// command -> built binary -> this main -> everything it calls.
package main

import (
	"context"
	"fmt"
	"os"

	"telemetry.example/srcgo/gen/telemetry"
	"telemetry.example/srcgo/internal/ingest"
	"telemetry.example/srcgo/internal/notify"
	"telemetry.example/srcgo/internal/station"
)

// loopbackConn stands in for a real connection so the module builds without a gRPC
// dependency. Nothing in the corpus dials anything.
type loopbackConn struct{}

func (loopbackConn) Invoke(ctx context.Context, method string, in any, out any) error {
	return nil
}

// server is the minimal registrar the generated code asks for.
type server struct {
	handlers map[string]any
}

func (s *server) RegisterHandler(name string, handler any) {
	if s.handlers == nil {
		s.handlers = make(map[string]any)
	}
	s.handlers[name] = handler
}

func seedRegistry() *station.Registry {
	reg := station.NewRegistry(1024)
	_ = reg.Add(&station.Station{ID: "alp-01", Label: "Alp Ridge", Altitude: 1200, Tags: []string{"mountain"}})
	_ = reg.Add(&station.Station{ID: "dune-02", Label: "Dune Flat", Altitude: 12})
	_ = reg.Add(&station.Station{ID: "fjord-03", Label: "Fjord Head", Altitude: 340, Retired: true})
	return reg
}

func run() error {
	reg := seedRegistry()
	alerter := notify.NewAlerter(loopbackConn{})
	svc := ingest.NewService(reg, alerter)

	srv := &server{}
	telemetry.RegisterTelemetryServiceServer(srv, svc)

	total, retired := reg.Counts()
	fmt.Printf("collector ready: %d stations, %d retired\n", total, retired)
	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "collector:", err)
		os.Exit(1)
	}
}
