package ingest

import (
	"context"
	"testing"

	"telemetry.example/srcgo/gen/telemetry"
	"telemetry.example/srcgo/internal/notify"
	"telemetry.example/srcgo/internal/station"
)

// stubConn records what would have gone over the wire.
type stubConn struct {
	calls []string
}

func (c *stubConn) Invoke(ctx context.Context, method string, in any, out any) error {
	c.calls = append(c.calls, method)
	return nil
}

func newFixture(t *testing.T) (*Service, *stubConn) {
	t.Helper()
	reg := station.NewRegistry(0)
	if err := reg.Add(&station.Station{ID: "alp-01", Label: "Alp Ridge", Altitude: 1200}); err != nil {
		t.Fatalf("seeding the registry: %v", err)
	}
	conn := &stubConn{}
	return NewService(reg, notify.NewAlerter(conn)), conn
}

func TestUploadAcceptsAPlainReading(t *testing.T) {
	svc, _ := newFixture(t)
	ack, err := svc.UploadReadings(context.Background(), &telemetry.ReadingBatch{
		Readings: []*telemetry.Reading{{StationId: "alp-01", Celsius: 4, Humidity: 50}},
	})
	if err != nil {
		t.Fatalf("upload: %v", err)
	}
	if ack.Accepted != 1 || ack.Rejected != 0 {
		t.Errorf("want 1/0, got %d/%d", ack.Accepted, ack.Rejected)
	}
}

func TestUploadRejectsAFaultySensor(t *testing.T) {
	svc, _ := newFixture(t)
	ack, _ := svc.UploadReadings(context.Background(), &telemetry.ReadingBatch{
		Readings: []*telemetry.Reading{{StationId: "alp-01", SensorFault: true}},
	})
	if ack.Rejected != 1 {
		t.Errorf("a faulty sensor must be rejected, got %d", ack.Rejected)
	}
}

func TestUploadRejectsAnUnknownStation(t *testing.T) {
	svc, _ := newFixture(t)
	ack, _ := svc.UploadReadings(context.Background(), &telemetry.ReadingBatch{
		Readings: []*telemetry.Reading{{StationId: "nowhere", Celsius: 4}},
	})
	if ack.Rejected != 1 {
		t.Errorf("an unknown station must be rejected, got %d", ack.Rejected)
	}
}

func TestSevereColdCrossesTheBoundary(t *testing.T) {
	svc, conn := newFixture(t)
	_, _ = svc.UploadReadings(context.Background(), &telemetry.ReadingBatch{
		Readings: []*telemetry.Reading{{StationId: "alp-01", Celsius: -40, Humidity: 30}},
	})
	if len(conn.calls) != 1 {
		t.Fatalf("want one alert call, got %v", conn.calls)
	}
}

func TestOversizedBatchIsRefused(t *testing.T) {
	svc, _ := newFixture(t)
	batch := &telemetry.ReadingBatch{Readings: make([]*telemetry.Reading, maxBatch+1)}
	if _, err := svc.UploadReadings(context.Background(), batch); err == nil {
		t.Errorf("a batch over the limit must be refused")
	}
}

func TestDescribeStationReportsTheRegistry(t *testing.T) {
	svc, _ := newFixture(t)
	info, err := svc.DescribeStation(context.Background(), &telemetry.StationQuery{StationId: "alp-01"})
	if err != nil {
		t.Fatalf("describe: %v", err)
	}
	if info.Altitude != 1200 {
		t.Errorf("got altitude %d", info.Altitude)
	}
}
