package ingest

import (
	"fmt"

	"telemetry.example/srcgo/gen/telemetry"
	"telemetry.example/srcgo/internal/station"
)

// Verdict is what screening decided about one reading.
type Verdict struct {
	Reject   bool
	Alarming bool
	Reason   string
}

// Screen holds the thresholds a reading is judged against.
type Screen struct {
	MinCelsius  float64
	MaxCelsius  float64
	LapseRateDC float64
}

// NewScreen builds the default thresholds.
func NewScreen() *Screen {
	return &Screen{MinCelsius: -60, MaxCelsius: 60, LapseRateDC: 6.5}
}

// Judge screens one reading against one station.
func (s *Screen) Judge(r *telemetry.Reading, site *station.Station) Verdict {
	if r.Celsius < s.MinCelsius || r.Celsius > s.MaxCelsius {
		return Verdict{Reject: true, Reason: fmt.Sprintf("%.1f C is out of range", r.Celsius)}
	}
	if r.Humidity < 0 || r.Humidity > 100 {
		return Verdict{Reject: true, Reason: "humidity out of range"}
	}
	if site.Retired {
		return Verdict{Reject: true, Reason: "station has retired"}
	}
	corrected := s.correct(r.Celsius, site)
	if corrected <= -25 {
		return Verdict{Alarming: true, Reason: "severe cold"}
	}
	if corrected >= 40 {
		return Verdict{Alarming: true, Reason: "severe heat"}
	}
	return Verdict{}
}

// correct applies the lapse rate so a mountain site is compared on equal terms.
func (s *Screen) correct(celsius float64, site *station.Station) float64 {
	if !site.Elevated() {
		return celsius
	}
	return celsius + (float64(site.Altitude)/1000.0)*s.LapseRateDC
}
