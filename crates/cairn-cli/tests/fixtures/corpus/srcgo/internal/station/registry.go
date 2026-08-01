// Package station keeps the set of stations the network knows about.
package station

import (
	"errors"
	"sort"
	"strings"
	"sync"
)

// ErrUnknownStation is returned for an id nobody registered.
var ErrUnknownStation = errors.New("unknown station")

// Station is a registered measuring site.
type Station struct {
	ID       string
	Label    string
	Altitude int64
	Retired  bool
	Tags     []string
}

// Elevated says whether the site sits high enough for the lapse-rate correction.
func (s *Station) Elevated() bool {
	return s.Altitude >= 800
}

// HasTag reports membership without exposing the slice.
func (s *Station) HasTag(tag string) bool {
	for _, t := range s.Tags {
		if strings.EqualFold(t, tag) {
			return true
		}
	}
	return false
}

// Registry is the in-memory set of stations, safe for concurrent readers.
type Registry struct {
	mu       sync.RWMutex
	byID     map[string]*Station
	retired  int
	capacity int
}

// NewRegistry builds an empty registry with a soft capacity.
func NewRegistry(capacity int) *Registry {
	return &Registry{byID: make(map[string]*Station), capacity: capacity}
}

// Add registers a station, replacing any earlier one with the same id.
func (r *Registry) Add(s *Station) error {
	if s == nil || s.ID == "" {
		return errors.New("a station needs an id")
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, seen := r.byID[s.ID]; !seen && r.capacity > 0 && len(r.byID) >= r.capacity {
		return errors.New("registry is full")
	}
	r.byID[s.ID] = s
	if s.Retired {
		r.retired++
	}
	return nil
}

// Lookup finds a station by id.
func (r *Registry) Lookup(id string) (*Station, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	s, ok := r.byID[id]
	if !ok {
		return nil, ErrUnknownStation
	}
	return s, nil
}

// Active lists the stations still reporting, in a stable order.
func (r *Registry) Active() []*Station {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]*Station, 0, len(r.byID))
	for _, s := range r.byID {
		if !s.Retired {
			out = append(out, s)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

// Retire marks a station as no longer reporting.
func (r *Registry) Retire(id string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	s, ok := r.byID[id]
	if !ok {
		return ErrUnknownStation
	}
	if !s.Retired {
		s.Retired = true
		r.retired++
	}
	return nil
}

// Counts reports how many stations are held and how many have retired.
func (r *Registry) Counts() (total int, retired int) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(r.byID), r.retired
}

// normaliseID is the one place an id is cleaned, so two call sites cannot disagree.
func normaliseID(raw string) string {
	return strings.ToLower(strings.TrimSpace(raw))
}
