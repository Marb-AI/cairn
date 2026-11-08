package station

// Reporter formats a station's status line. Square and Circle in the audit fixture are
// the minimal version of this; here it is why `unreached` and `outline` have to know
// about `is_implementation`: ReportAll calls through the interface, so neither
// implementation has a direct caller by name, and before the fix both printed `unused`.
type Reporter interface {
	Report() string
}

// Terse is one line, no detail.
type Terse struct{ ID string }

func (t Terse) Report() string { return t.ID }

// Verbose spells out the altitude. Never named directly anywhere in this package -
// the whole point of the fixture.
type Verbose struct {
	ID       string
	Altitude int64
}

func (v Verbose) Report() string {
	if v.Altitude >= 800 {
		return v.ID + " (elevated)"
	}
	return v.ID
}

// ReportAll calls each Reporter's own method. Which concrete type runs is not knowable
// without running the program - that is what the interface is for.
func ReportAll(rs []Reporter) []string {
	out := make([]string, 0, len(rs))
	for _, r := range rs {
		out = append(out, r.Report())
	}
	return out
}
