package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"go/ast"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"golang.org/x/tools/go/packages"
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
	Signature string `json:"signature,omitempty"`
	CrateName string `json:"crate_name"`
}

type Output struct {
	Edges  []CallEdge `json:"edges"`
	Chunks []Chunk    `json:"chunks"`
}

type funcSpan struct {
	name     string
	file     string
	startPos token.Pos
	endPos   token.Pos
	startLn  int
	endLn    int
}

func main() {
	flag.Parse()
	patterns := flag.Args()
	if len(patterns) == 0 {
		patterns = []string{"./..."}
	}
	start := time.Now()
	projectRoot, err := filepath.Abs(".")
	if err != nil {
		fmt.Fprintf(os.Stderr, "abs: %v\n", err)
		os.Exit(2)
	}
	projectRoot = filepath.ToSlash(projectRoot)
	cfg := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedSyntax |
			packages.NeedTypes | packages.NeedTypesInfo | packages.NeedImports,
	}
	initial, err := packages.Load(cfg, patterns...)
	if err != nil {
		fmt.Fprintf(os.Stderr, "load: %v\n", err)
		os.Exit(2)
	}
	for _, pkg := range initial {
		for _, e := range pkg.Errors {
			fmt.Fprintf(os.Stderr, "pkg error: %v\n", e)
		}
	}

	var allSpans []funcSpan
	chunkSet := make(map[string]bool)
	var chunks []Chunk

	for _, pkg := range initial {
		pkgName := pkg.Name
		fset := pkg.Fset
		for _, file := range pkg.Syntax {
			filename := fset.Position(file.Pos()).Filename
			relFile := toRelPath(filename, projectRoot)
			if relFile == "" {
				continue
			}
			ast.Inspect(file, func(n ast.Node) bool {
				switch decl := n.(type) {
				case *ast.FuncDecl:
					name := astFuncName(decl)
					startLn := fset.Position(decl.Pos()).Line
					endLn := fset.Position(decl.End()).Line
					allSpans = append(allSpans, funcSpan{
						name: name, file: relFile,
						startPos: decl.Pos(), endPos: decl.End(),
						startLn: startLn, endLn: endLn,
					})
					key := relFile + ":" + name
					if !chunkSet[key] {
						chunkSet[key] = true
						chunks = append(chunks, Chunk{
							Name: name, File: relFile, Kind: astFuncKind(decl),
							StartLine: startLn, EndLine: endLn,
							Signature: astFuncSig(decl), CrateName: pkgName,
						})
					}
				case *ast.GenDecl:
					if decl.Tok != token.TYPE {
						return true
					}
					for _, spec := range decl.Specs {
						ts, ok := spec.(*ast.TypeSpec)
						if !ok {
							continue
						}
						kind := ""
						switch ts.Type.(type) {
						case *ast.StructType:
							kind = "struct"
						case *ast.InterfaceType:
							kind = "interface"
						default:
							continue
						}
						startLn := fset.Position(ts.Pos()).Line
						endLn := fset.Position(ts.End()).Line
						key := relFile + ":" + ts.Name.Name
						if !chunkSet[key] {
							chunkSet[key] = true
							chunks = append(chunks, Chunk{
								Name: ts.Name.Name, File: relFile, Kind: kind,
								StartLine: startLn, EndLine: endLn,
								CrateName: pkgName,
							})
						}
					}
				}
				return true
			})
		}
	}

	sort.Slice(allSpans, func(i, j int) bool {
		return allSpans[i].startPos < allSpans[j].startPos
	})

	var edges []CallEdge
	for _, pkg := range initial {
		fset := pkg.Fset
		for id, obj := range pkg.TypesInfo.Uses {
			fn, ok := obj.(*types.Func)
			if !ok {
				continue
			}
			callPos := fset.Position(id.Pos())
			if !callPos.IsValid() || callPos.Line == 0 {
				continue
			}
			callFile := toRelPath(callPos.Filename, projectRoot)
			if callFile == "" {
				continue
			}
			caller := findEnclosing(id.Pos(), allSpans)
			if caller == nil {
				continue
			}
			edges = append(edges, CallEdge{
				Caller:      caller.name,
				Callee:      typesLeafName(fn),
				File:        callFile,
				Line:        callPos.Line,
				CallerFile:  caller.file,
				CallerStart: caller.startLn,
				CallerEnd:   caller.endLn,
			})
		}
	}

	out := Output{Edges: edges, Chunks: chunks}
	if out.Edges == nil {
		out.Edges = []CallEdge{}
	}
	if out.Chunks == nil {
		out.Chunks = []Chunk{}
	}
	elapsed := time.Since(start)
	fmt.Fprintf(os.Stderr, "edges: %d, chunks: %d, elapsed: %v\n", len(edges), len(chunks), elapsed)
	json.NewEncoder(os.Stdout).Encode(out)
}

func findEnclosing(pos token.Pos, spans []funcSpan) *funcSpan {
	idx := sort.Search(len(spans), func(i int) bool {
		return spans[i].startPos > pos
	})
	for i := idx - 1; i >= 0; i-- {
		if spans[i].startPos <= pos && pos <= spans[i].endPos {
			return &spans[i]
		}
		if spans[i].endPos < pos {
			break
		}
	}
	return nil
}

func toRelPath(absPath, projectRoot string) string {
	p := filepath.ToSlash(absPath)
	if !strings.HasPrefix(p, projectRoot+"/") {
		return ""
	}
	return p[len(projectRoot)+1:]
}

func astFuncName(decl *ast.FuncDecl) string {
	if decl.Recv != nil && len(decl.Recv.List) > 0 {
		return astRecvName(decl.Recv.List[0].Type) + "." + decl.Name.Name
	}
	return decl.Name.Name
}

func astRecvName(expr ast.Expr) string {
	switch t := expr.(type) {
	case *ast.StarExpr:
		return astRecvName(t.X)
	case *ast.Ident:
		return t.Name
	case *ast.IndexExpr:
		return astRecvName(t.X)
	case *ast.IndexListExpr:
		return astRecvName(t.X)
	}
	return "?"
}

func astFuncKind(decl *ast.FuncDecl) string {
	if decl.Recv != nil {
		return "method"
	}
	return "function"
}

func astFuncSig(decl *ast.FuncDecl) string {
	var b strings.Builder
	b.WriteString("func ")
	if decl.Recv != nil && len(decl.Recv.List) > 0 {
		b.WriteByte('(')
		writeFieldList(&b, decl.Recv)
		b.WriteString(") ")
	}
	b.WriteString(decl.Name.Name)
	b.WriteByte('(')
	if decl.Type.Params != nil {
		writeFieldList(&b, decl.Type.Params)
	}
	b.WriteByte(')')
	if decl.Type.Results != nil && len(decl.Type.Results.List) > 0 {
		if len(decl.Type.Results.List) == 1 && len(decl.Type.Results.List[0].Names) == 0 {
			b.WriteByte(' ')
			writeExpr(&b, decl.Type.Results.List[0].Type)
		} else {
			b.WriteString(" (")
			writeFieldList(&b, decl.Type.Results)
			b.WriteByte(')')
		}
	}
	return b.String()
}

func writeFieldList(b *strings.Builder, fl *ast.FieldList) {
	for i, f := range fl.List {
		if i > 0 {
			b.WriteString(", ")
		}
		for j, name := range f.Names {
			if j > 0 {
				b.WriteString(", ")
			}
			b.WriteString(name.Name)
		}
		if len(f.Names) > 0 {
			b.WriteByte(' ')
		}
		writeExpr(b, f.Type)
	}
}

func writeExpr(b *strings.Builder, expr ast.Expr) {
	switch t := expr.(type) {
	case *ast.Ident:
		b.WriteString(t.Name)
	case *ast.StarExpr:
		b.WriteByte('*')
		writeExpr(b, t.X)
	case *ast.SelectorExpr:
		writeExpr(b, t.X)
		b.WriteByte('.')
		b.WriteString(t.Sel.Name)
	case *ast.ArrayType:
		b.WriteString("[]")
		writeExpr(b, t.Elt)
	case *ast.MapType:
		b.WriteString("map[")
		writeExpr(b, t.Key)
		b.WriteByte(']')
		writeExpr(b, t.Value)
	case *ast.InterfaceType:
		b.WriteString("interface{}")
	case *ast.FuncType:
		b.WriteString("func(")
		if t.Params != nil {
			writeFieldList(b, t.Params)
		}
		b.WriteByte(')')
		if t.Results != nil && len(t.Results.List) > 0 {
			b.WriteByte(' ')
			if len(t.Results.List) == 1 && len(t.Results.List[0].Names) == 0 {
				writeExpr(b, t.Results.List[0].Type)
			} else {
				b.WriteByte('(')
				writeFieldList(b, t.Results)
				b.WriteByte(')')
			}
		}
	case *ast.Ellipsis:
		b.WriteString("...")
		writeExpr(b, t.Elt)
	case *ast.ChanType:
		switch t.Dir {
		case ast.SEND:
			b.WriteString("chan<- ")
		case ast.RECV:
			b.WriteString("<-chan ")
		default:
			b.WriteString("chan ")
		}
		writeExpr(b, t.Value)
	default:
		b.WriteByte('?')
	}
}

func typesLeafName(fn *types.Func) string {
	sig := fn.Type().(*types.Signature)
	recv := sig.Recv()
	if recv != nil {
		return typesRecvLeaf(recv.Type()) + "." + fn.Name()
	}
	return fn.Name()
}

func typesRecvLeaf(t types.Type) string {
	switch ty := t.(type) {
	case *types.Pointer:
		return typesRecvLeaf(ty.Elem())
	case *types.Named:
		return ty.Obj().Name()
	}
	return "?"
}
