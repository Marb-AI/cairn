// Package ingest serves TelemetryService: it accepts uploaded readings, screens them,
// and hands anything alarming to the alerting side over the network.
package ingest

import (
	"context"
	"fmt"

	"telemetry.example/srcgo/gen/telemetry"
	"telemetry.example/srcgo/internal/notify"
	"telemetry.example/srcgo/internal/station"
)

// maxBatch bounds one upload. A collector that has been offline for a week would
// otherwise arrive with a month of readings in a single call.
const maxBatch = 500

// Service is the TelemetryService implementation.
type Service struct {
	telemetry.UnimplementedTelemetryServiceServer

	stations *station.Registry
	alerts   *notify.Alerter
	// The alert client held directly, so the package-level `notify.SendAlert` can be
	// handed it. See the comment on that function for why the shape matters.
	alertClient telemetry.AlertServiceClient
	screen      *Screen
	accepted int64
	rejected int64
}

// NewService builds the server side.
func NewService(reg *station.Registry, alerts *notify.Alerter, alertClient telemetry.AlertServiceClient) *Service {
	return &Service{stations: reg, alerts: alerts, alertClient: alertClient, screen: NewScreen()}
}

// UploadReadings is the hot path: one call per collector per interval.
func (s *Service) UploadReadings(ctx context.Context, batch *telemetry.ReadingBatch) (*telemetry.IngestAck, error) {
	if batch == nil {
		return nil, fmt.Errorf("empty batch")
	}
	if len(batch.Readings) > maxBatch {
		return nil, fmt.Errorf("batch of %d exceeds the limit of %d", len(batch.Readings), maxBatch)
	}
	ack := &telemetry.IngestAck{}
	for _, r := range batch.Readings {
		if !s.accept(ctx, r) {
			ack.Rejected++
			continue
		}
		ack.Accepted++
	}
	s.accepted += ack.Accepted
	s.rejected += ack.Rejected
	return ack, nil
}

// DescribeStation answers what the registry holds about one site.
func (s *Service) DescribeStation(ctx context.Context, q *telemetry.StationQuery) (*telemetry.StationInfo, error) {
	found, err := s.stations.Lookup(q.GetStationId())
	if err != nil {
		return nil, err
	}
	return &telemetry.StationInfo{
		StationId: found.ID,
		Label:     found.Label,
		Altitude:  found.Altitude,
		Retired:   found.Retired,
	}, nil
}

// accept screens one reading and raises an alert when it needs one.
func (s *Service) accept(ctx context.Context, r *telemetry.Reading) bool {
	if r == nil || r.SensorFault {
		return false
	}
	site, err := s.stations.Lookup(r.StationId)
	if err != nil {
		return false
	}
	verdict := s.screen.Judge(r, site)
	if verdict.Reject {
		return false
	}
	if verdict.Alarming {
		// Fire and forget: the upload must not fail because the alerting side is down.
		s.alerts.Raise(ctx, r.StationId, verdict.Reason, r.Celsius)
		// The same alert again through the package-level entry point. Redundant as
		// behaviour and deliberate as structure: this is the call the indexer can follow,
		// so it is what makes the second boundary crossing visible to the graph.
		_ = notify.SendAlert(ctx, s.alertClient, r.StationId, verdict.Reason)
	}
	return true
}

// Totals reports what this process has seen since it started.
func (s *Service) Totals() (accepted int64, rejected int64) {
	return s.accepted, s.rejected
}
