package station

import "testing"

func TestAddAndLookup(t *testing.T) {
	r := NewRegistry(0)
	if err := r.Add(&Station{ID: "alp-01", Label: "Alp Ridge", Altitude: 1200}); err != nil {
		t.Fatalf("add: %v", err)
	}
	s, err := r.Lookup("alp-01")
	if err != nil {
		t.Fatalf("lookup: %v", err)
	}
	if !s.Elevated() {
		t.Errorf("a station at 1200 m should be elevated")
	}
}

func TestLookupUnknownIsAnError(t *testing.T) {
	r := NewRegistry(0)
	if _, err := r.Lookup("nowhere"); err != ErrUnknownStation {
		t.Errorf("want ErrUnknownStation, got %v", err)
	}
}

func TestRetireIsCountedOnce(t *testing.T) {
	r := NewRegistry(0)
	_ = r.Add(&Station{ID: "dune-02"})
	_ = r.Retire("dune-02")
	_ = r.Retire("dune-02")
	total, retired := r.Counts()
	if total != 1 || retired != 1 {
		t.Errorf("want 1/1, got %d/%d", total, retired)
	}
}

func TestActiveSkipsRetiredAndSorts(t *testing.T) {
	r := NewRegistry(0)
	_ = r.Add(&Station{ID: "c-03"})
	_ = r.Add(&Station{ID: "a-01"})
	_ = r.Add(&Station{ID: "b-02", Retired: true})
	active := r.Active()
	if len(active) != 2 || active[0].ID != "a-01" {
		t.Errorf("unexpected active set: %+v", active)
	}
}

func TestNormaliseID(t *testing.T) {
	if got := normaliseID("  ALP-01 "); got != "alp-01" {
		t.Errorf("got %q", got)
	}
}
