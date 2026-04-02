package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"

	"golang.org/x/tools/go/callgraph"
	"golang.org/x/tools/go/callgraph/vta"
	"golang.org/x/tools/go/packages"
	"golang.org/x/tools/go/ssa"
	"golang.org/x/tools/go/ssa/ssautil"
)

type CallEdge struct {
	Caller      string `json:"caller"`
	Callee      string `json:"callee"`
	File        string `json:"file"`
	Line        int    `json:"line"`
	CallerFile  string `json:"caller_file"`
	CallerStart int    `json:"caller_start"`
	CallerEnd   int    `json:"caller_end"`
}

func main() {
	flag.Parse()
	patterns := flag.Args()
	if len(patterns) == 0 {
		patterns = []string{"./..."}
	}

	cfg := &packages.Config{Mode: packages.LoadAllSyntax}
	initial, err := packages.Load(cfg, patterns...)
	if err != nil {
		fmt.Fprintf(os.Stderr, "load: %v\n", err)
		os.Exit(2)
	}

	prog, _ := ssautil.AllPackages(initial, ssa.InstantiateGenerics)
	prog.Build()

	cg := vta.CallGraph(ssautil.AllFunctions(prog), nil)
	cg.DeleteSyntheticNodes()

	var edges []CallEdge
	callgraph.GraphVisitEdges(cg, func(e *callgraph.Edge) error {
		if e.Site == nil {
			return nil
		}
		pos := prog.Fset.Position(e.Site.Pos())
		caller := e.Caller.Func
		callee := e.Callee.Func
		callerPos := prog.Fset.Position(caller.Pos())
		callerEnd := callerPos.Line
		if caller.Syntax() != nil {
			callerEnd = prog.Fset.Position(caller.Syntax().End()).Line
		}
		edges = append(edges, CallEdge{
			Caller: caller.String(), Callee: callee.String(),
			File: pos.Filename, Line: pos.Line,
			CallerFile: callerPos.Filename, CallerStart: callerPos.Line, CallerEnd: callerEnd,
		})
		return nil
	})
	json.NewEncoder(os.Stdout).Encode(edges)
}
