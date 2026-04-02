# Go Call Graph Extractor 상세 구현

## main.go 전체 구조

```go
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

type Chunk struct {
    Name      string `json:"name"`
    File      string `json:"file"`
    Kind      string `json:"kind"`
    StartLine int    `json:"start_line"`
    EndLine   int    `json:"end_line"`
    Signature string `json:"signature"`
    CrateName string `json:"crate_name"`
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
        fmt.Fprintf(os.Stderr, "load error: %v\n", err)
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
        // caller 함수의 끝 위치 추정 (syntax에서)
        callerEnd := callerPos.Line
        if caller.Syntax() != nil {
            callerEnd = prog.Fset.Position(caller.Syntax().End()).Line
        }
        edges = append(edges, CallEdge{
            Caller:      caller.String(),
            Callee:      callee.String(),
            File:        pos.Filename,
            Line:        pos.Line,
            CallerFile:  callerPos.Filename,
            CallerStart: callerPos.Line,
            CallerEnd:   callerEnd,
        })
        return nil
    })

    json.NewEncoder(os.Stdout).Encode(edges)
}
```

## go.mod
```
module github.com/user/go-callgraph

go 1.21

require golang.org/x/tools v0.28.0
```

## VTA가 interface dispatch를 해결하는 원리
1. 타입 전파 그래프 구성: `var w io.Writer = os.Stdout` → w에 *os.File이 흐름
2. 호출 사이트 `w.Write()` 에서 도달 가능 타입 = {*os.File}
3. edge 생성: caller → (*os.File).Write

CHA는 io.Writer를 구현하는 모든 타입의 Write를 추가하지만, VTA는 실제로 흐르는 타입만 추적.

## rude runner 통합

`mir_edges/runner.rs`에 추가:
```rust
pub fn run_go_callgraph(project_dir: &Path) -> Result<Vec<GoCallEdge>> {
    let bin = find_go_callgraph_bin()?;
    let output = Command::new(bin)
        .arg("./...")
        .current_dir(project_dir)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("go-callgraph failed: {stderr}");
    }
    let edges: Vec<GoCallEdge> = serde_json::from_slice(&output.stdout)?;
    Ok(edges)
}
```
