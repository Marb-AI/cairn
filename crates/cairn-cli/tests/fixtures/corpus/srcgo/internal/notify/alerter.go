// Package notify is the calling side of the boundary: it reaches AlertService, which is
// implemented in Python. Nothing in this package names the implementation, which is the
// point — the edge only exists in the generated client.
package notify

import (
	"context"
	"sync"

	"telemetry.example/srcgo/gen/telemetry"
)

// Alerter sends alerts and remembers which stations are already alarming, so a station
// reporting every thirty seconds raises one alert rather than a thousand.
type Alerter struct {
	mu      sync.Mutex
	client  telemetry.AlertServiceClient
	raised  map[string]string
	dropped int
}

// NewAlerter builds the client side over an existing connection.
func NewAlerter(conn telemetry.Conn) *Alerter {
	return &Alerter{
		client: telemetry.NewAlertServiceClient(conn),
		raised: make(map[string]string),
	}
}

// Raise reports a station as alarming, unless it already is for the same reason.
func (a *Alerter) Raise(ctx context.Context, stationID, reason string, celsius float64) {
	a.mu.Lock()
	if prev, seen := a.raised[stationID]; seen && prev == reason {
		a.dropped++
		a.mu.Unlock()
		return
	}
	a.raised[stationID] = reason
	a.mu.Unlock()

	_, err := a.client.RaiseAlert(ctx, &telemetry.AlertRequest{
		StationId: stationID,
		Reason:    reason,
		Celsius:   celsius,
	})
	if err != nil {
		// The alerting side being unreachable must not fail an upload, so this is
		// recorded and dropped rather than returned.
		a.forget(stationID)
	}
}

// Clear withdraws an alert once a station reads normally again.
func (a *Alerter) Clear(ctx context.Context, stationID string) {
	a.mu.Lock()
	_, seen := a.raised[stationID]
	delete(a.raised, stationID)
	a.mu.Unlock()
	if !seen {
		return
	}
	_, _ = a.client.ClearAlert(ctx, &telemetry.AlertRequest{StationId: stationID})
}

// forget drops the memo so the next reading tries again.
func (a *Alerter) forget(stationID string) {
	a.mu.Lock()
	delete(a.raised, stationID)
	a.mu.Unlock()
}

// Stats reports how many alerts are open and how many repeats were suppressed.
func (a *Alerter) Stats() (open int, dropped int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return len(a.raised), a.dropped
}
