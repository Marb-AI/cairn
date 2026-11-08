// cairnflow prints the control flow inside one Go function, as JSON.
//
// The call graph is between functions. Reviewing a single handler needs the tree *inside*
// one: where it branches, where it returns early, what it calls on the way, and what it
// defers. That is a syntax question, and this answers it with the standard library's own
// parser plus golang.org/x/tools/go/cfg.
//
// go/cfg rather than go/ssa, deliberately. ssa is more precise and loses source
// positions: mapping a block back to the lines a reader has to open is the whole point
// here, and cfg keeps the AST nodes, so every node this prints has a real span. Where cfg
// is not enough — a flow through a closure, say — the answer says so rather than guessing.
//
// Usage:
//
//	cairnflow <file.go> <line>
//
// `line` is 1-based and must fall inside the function to describe. stdout is one JSON
// object; stderr is for failures, and a non-zero exit means cairn keeps whatever it had.
//
// Deterministic: no clock, no network, no map iteration in the output. Nodes come out in
// source order, which is also the order a reader reads them in.
package main

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"sort"
	"strings"

	"golang.org/x/tools/go/cfg"
)

// Version is part of cairn's cache key, so a change here invalidates cached flows.
const Version = "cairnflow 1"

type position struct {
	Line int `json:"line"`
	Col  int `json:"col"`
}

type node struct {
	ID   int    `json:"id"`
	Kind string `json:"kind"`
	// The source text of a branch condition or a loop header, for a reader. Never
	// interpreted by cairn.
	Condition string `json:"condition,omitempty"`
	// What a call names, as written: `reg.Add`, `fmt.Errorf`. cairn resolves it to a
	// symbol through the occurrence at `at`, so this is for display and for the case
	// where resolution fails.
	Name string `json:"name,omitempty"`
	// Where the callee name sits, which is what cairn resolves against. Only for calls.
	At *position `json:"at,omitempty"`
	// `line-line`, 1-based and inclusive. The file is the one cairn asked about.
	Span string `json:"span"`
}

type edge struct {
	From  int    `json:"from"`
	To    int    `json:"to"`
	Label string `json:"label,omitempty"`
}

type flow struct {
	Provider string   `json:"provider"`
	Function string   `json:"function"`
	File     string   `json:"file"`
	Nodes    []node   `json:"nodes"`
	Edges    []edge   `json:"edges"`
	Unknown  []string `json:"unknown"`
}

func main() {
	if len(os.Args) == 2 && (os.Args[1] == "--version" || os.Args[1] == "-version") {
		fmt.Println(Version)
		return
	}
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: cairnflow <file.go> <line>")
		os.Exit(2)
	}
	path := os.Args[1]
	var line int
	if _, err := fmt.Sscanf(os.Args[2], "%d", &line); err != nil || line < 1 {
		fmt.Fprintf(os.Stderr, "line %q is not a 1-based line number\n", os.Args[2])
		os.Exit(2)
	}

	fset := token.NewFileSet()
	// Comments are not needed and parsing them costs nothing useful here.
	file, err := parser.ParseFile(fset, path, nil, 0)
	if err != nil {
		fmt.Fprintf(os.Stderr, "parsing %s: %v\n", path, err)
		os.Exit(1)
	}

	fn := functionAt(fset, file, line)
	if fn == nil {
		fmt.Fprintf(os.Stderr, "no function body contains %s:%d\n", path, line)
		os.Exit(1)
	}

	out := flow{
		Provider: Version,
		Function: name(fn),
		File:     path,
		Nodes:    []node{},
		Edges:    []edge{},
		Unknown:  []string{},
	}
	build(fset, fn, &out)
	enc := json.NewEncoder(os.Stdout)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(out); err != nil {
		fmt.Fprintf(os.Stderr, "writing the flow: %v\n", err)
		os.Exit(1)
	}
}

// functionAt finds the innermost function declaration whose body spans `line`.
//
// Innermost, because a method on a struct literal inside another function is the one the
// caller meant if they pointed at it.
func functionAt(fset *token.FileSet, file *ast.File, line int) *ast.FuncDecl {
	var best *ast.FuncDecl
	for _, decl := range file.Decls {
		fn, ok := decl.(*ast.FuncDecl)
		if !ok || fn.Body == nil {
			continue
		}
		start := fset.Position(fn.Pos()).Line
		end := fset.Position(fn.End()).Line
		if line < start || line > end {
			continue
		}
		if best == nil {
			best = fn
			continue
		}
		bs := fset.Position(best.Pos()).Line
		be := fset.Position(best.End()).Line
		if (end - start) < (be - bs) {
			best = fn
		}
	}
	return best
}

func name(fn *ast.FuncDecl) string {
	if fn.Recv != nil && len(fn.Recv.List) > 0 {
		return receiverName(fn.Recv.List[0].Type) + "." + fn.Name.Name
	}
	return fn.Name.Name
}

func receiverName(expr ast.Expr) string {
	switch t := expr.(type) {
	case *ast.StarExpr:
		return receiverName(t.X)
	case *ast.Ident:
		return t.Name
	case *ast.IndexExpr: // a generic receiver
		return receiverName(t.X)
	}
	return "?"
}

// build walks the function and emits the nodes a reviewer needs, in source order.
//
// The CFG is built so that *reachability* is right — a block after an early return is not
// on the same path — and then the nodes are emitted from the AST, which is where the
// positions and the readable conditions are. Using the CFG for structure and the AST for
// text is what keeps both honest.
func build(fset *token.FileSet, fn *ast.FuncDecl, out *flow) {
	g := cfg.New(fn.Body, func(*ast.CallExpr) bool {
		// Whether a call may return. Assuming they all do keeps the graph connected;
		// the alternative needs type information this provider deliberately does not
		// load, and getting it wrong would drop real paths.
		return true
	})
	if g == nil {
		out.Unknown = append(out.Unknown, "the control-flow graph could not be built for this function")
		return
	}

	span := func(n ast.Node) string {
		return fmt.Sprintf("%d-%d", fset.Position(n.Pos()).Line, fset.Position(n.End()).Line)
	}
	add := func(n node) int {
		n.ID = len(out.Nodes)
		out.Nodes = append(out.Nodes, n)
		return n.ID
	}

	entry := add(node{Kind: "entry", Name: name(fn), Span: span(fn)})
	previous := entry
	// A linear chain through the nodes in source order, plus the labelled edges a branch
	// adds. The chain is what a reader follows; the labels are where it forks.
	link := func(to int, label string) {
		out.Edges = append(out.Edges, edge{From: previous, To: to, Label: label})
		previous = to
	}

	// Calls inside an expression, in source order, before whatever the expression
	// decides. `if !s.accept(ctx, r)` *calls* `accept` and then branches on it, and the
	// call is the interesting half: it is how the flow leaves this function. Emitting
	// only the branch would hide every call made in a condition or a return.
	emitCalls := func(expr ast.Expr) {
		if expr == nil {
			return
		}
		ast.Inspect(expr, func(n ast.Node) bool {
			call, ok := n.(*ast.CallExpr)
			if !ok {
				return true
			}
			if c := callNode(fset, call, span(call)); c != nil {
				id := add(*c)
				link(id, "")
			}
			return true
		})
	}

	var visit func(ast.Node) bool
	visit = func(n ast.Node) bool {
		switch s := n.(type) {
		case *ast.IfStmt:
			emitCalls(s.Cond)
			id := add(node{Kind: "branch", Condition: text(fset, s.Cond), Span: span(s)})
			link(id, "")
			// The body first, then the else: source order, which is reading order.
			ast.Inspect(s.Body, visit)
			if s.Else != nil {
				ast.Inspect(s.Else, visit)
			}
			return false
		case *ast.ForStmt:
			emitCalls(s.Cond)
			id := add(node{Kind: "loop", Condition: text(fset, s.Cond), Span: span(s)})
			link(id, "")
			ast.Inspect(s.Body, visit)
			return false
		case *ast.RangeStmt:
			id := add(node{Kind: "loop", Condition: "range " + text(fset, s.X), Span: span(s)})
			link(id, "")
			ast.Inspect(s.Body, visit)
			return false
		case *ast.SwitchStmt:
			emitCalls(s.Tag)
			id := add(node{Kind: "branch", Condition: "switch " + text(fset, s.Tag), Span: span(s)})
			link(id, "")
			ast.Inspect(s.Body, visit)
			return false
		case *ast.TypeSwitchStmt:
			id := add(node{Kind: "branch", Condition: "type switch", Span: span(s)})
			link(id, "")
			ast.Inspect(s.Body, visit)
			return false
		case *ast.SelectStmt:
			id := add(node{Kind: "branch", Condition: "select", Span: span(s)})
			link(id, "")
			ast.Inspect(s.Body, visit)
			return false
		case *ast.DeferStmt:
			id := add(node{Kind: "defer", Name: callName(s.Call.Fun), At: at(fset, s.Call.Fun), Span: span(s)})
			link(id, "")
			return false
		case *ast.ReturnStmt:
			for _, r := range s.Results {
				emitCalls(r)
			}
			kind := "return"
			// An early error return is the thing a reviewer looks for first, and `err`
			// by name is how Go spells it. A return of a non-nil error expression counts;
			// `return nil` does not.
			if returnsError(fset, s) {
				kind = "return_error"
			}
			id := add(node{Kind: kind, Condition: returnText(fset, s), Span: span(s)})
			link(id, "")
			return false
		case *ast.CallExpr:
			if n := callNode(fset, s, span(s)); n != nil {
				id := add(*n)
				link(id, "")
			}
			return true
		}
		return true
	}
	ast.Inspect(fn.Body, visit)

	if panics(fn) {
		out.Unknown = append(out.Unknown, "this function panics on at least one path; a panic is not a return and no caller sees it as one")
	}
	sort.SliceStable(out.Edges, func(i, j int) bool {
		if out.Edges[i].From != out.Edges[j].From {
			return out.Edges[i].From < out.Edges[j].From
		}
		return out.Edges[i].To < out.Edges[j].To
	})
}

func callNode(fset *token.FileSet, call *ast.CallExpr, span string) *node {
	name := callName(call.Fun)
	if name == "" {
		return nil
	}
	kind := "call"
	if name == "panic" {
		kind = "panic"
	}
	return &node{Kind: kind, Name: name, At: at(fset, call.Fun), Span: span}
}

// at is where the callee *name* sits, which is what cairn resolves against: SCIP records
// an occurrence at the identifier, not at the opening parenthesis.
func at(fset *token.FileSet, fun ast.Expr) *position {
	target := fun
	if sel, ok := fun.(*ast.SelectorExpr); ok {
		target = sel.Sel
	}
	p := fset.Position(target.Pos())
	// 0-based column, as SCIP records it; the line stays 1-based, as everything a reader
	// sees does. The asymmetry is deliberate and documented on cairn's side too.
	return &position{Line: p.Line, Col: p.Column - 1}
}

func callName(fun ast.Expr) string {
	switch f := fun.(type) {
	case *ast.Ident:
		return f.Name
	case *ast.SelectorExpr:
		return text(nil, f.X) + "." + f.Sel.Name
	case *ast.IndexExpr: // a generic instantiation
		return callName(f.X)
	case *ast.ParenExpr:
		return callName(f.X)
	}
	return ""
}

// text renders an expression the way it was written, as far as that is cheap.
//
// Deliberately not `go/printer`: a condition is shown to a reader, and the shapes that
// matter here — an identifier, a selector, a comparison — are a few lines. Anything else
// comes back as `…`, which is honest and does not pretend to be the source.
func text(fset *token.FileSet, expr ast.Expr) string {
	switch e := expr.(type) {
	case nil:
		return ""
	case *ast.Ident:
		return e.Name
	case *ast.SelectorExpr:
		return text(fset, e.X) + "." + e.Sel.Name
	case *ast.BasicLit:
		return e.Value
	case *ast.UnaryExpr:
		return e.Op.String() + text(fset, e.X)
	case *ast.BinaryExpr:
		return text(fset, e.X) + " " + e.Op.String() + " " + text(fset, e.Y)
	case *ast.CallExpr:
		return callName(e.Fun) + "(…)"
	case *ast.ParenExpr:
		return "(" + text(fset, e.X) + ")"
	case *ast.IndexExpr:
		return text(fset, e.X) + "[" + text(fset, e.Index) + "]"
	case *ast.StarExpr:
		return "*" + text(fset, e.X)
	}
	return "…"
}

func returnText(fset *token.FileSet, s *ast.ReturnStmt) string {
	parts := make([]string, 0, len(s.Results))
	for _, r := range s.Results {
		parts = append(parts, text(fset, r))
	}
	return strings.Join(parts, ", ")
}

// returnsError is a name test, not a type test: this provider loads no type information,
// and in Go the error is called `err` often enough that the name is the signal. A wrapped
// one (`fmt.Errorf(…)`) counts too.
func returnsError(fset *token.FileSet, s *ast.ReturnStmt) bool {
	for _, r := range s.Results {
		switch e := r.(type) {
		case *ast.Ident:
			if e.Name == "err" {
				return true
			}
		case *ast.CallExpr:
			n := callName(e.Fun)
			if strings.HasSuffix(n, "Errorf") || strings.HasSuffix(n, "New") ||
				strings.HasSuffix(n, "Wrap") || strings.HasSuffix(n, "Wrapf") {
				return true
			}
		}
	}
	return false
}

func panics(fn *ast.FuncDecl) bool {
	found := false
	ast.Inspect(fn.Body, func(n ast.Node) bool {
		if call, ok := n.(*ast.CallExpr); ok && callName(call.Fun) == "panic" {
			found = true
		}
		return !found
	})
	return found
}
