# TypeScript Call Graph Extractor 상세 구현

## index.ts 전체 구조

```typescript
import * as ts from "typescript";
import * as path from "path";
import * as fs from "fs";

interface CallEdge {
  caller: string;
  callee: string;
  file: string;
  line: number;
  caller_file: string;
  caller_start: number;
  caller_end: number;
}

interface Chunk {
  name: string;
  file: string;
  kind: string;
  start_line: number;
  end_line: number;
  signature: string;
  crate_name: string;
}

function buildCallGraph(rootDir: string, tsconfigPath?: string): { edges: CallEdge[], chunks: Chunk[] } {
  const configPath = tsconfigPath || ts.findConfigFile(rootDir, ts.sys.fileExists, "tsconfig.json");
  let program: ts.Program;

  if (configPath) {
    const config = ts.readConfigFile(configPath, ts.sys.readFile);
    const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, path.dirname(configPath));
    program = ts.createProgram(parsed.fileNames, { ...parsed.options, skipLibCheck: true });
  } else {
    const files = findSourceFiles(rootDir);
    program = ts.createProgram(files, { allowJs: true, skipLibCheck: true, target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext });
  }

  const checker = program.getTypeChecker();
  const edges: CallEdge[] = [];
  const chunks: Chunk[] = [];
  const seenChunks = new Set<string>();

  for (const sf of program.getSourceFiles()) {
    if (sf.isDeclarationFile) continue;
    const relFile = path.relative(rootDir, sf.fileName);
    collectChunks(sf, relFile, checker, chunks, seenChunks);
    visitNode(sf, sf, relFile, null, checker, edges);
  }

  return { edges, chunks };
}

function getEnclosingFunction(node: ts.Node): ts.FunctionLikeDeclaration | null {
  let current = node.parent;
  while (current) {
    if (ts.isFunctionDeclaration(current) || ts.isMethodDeclaration(current) ||
        ts.isArrowFunction(current) || ts.isFunctionExpression(current) ||
        ts.isConstructorDeclaration(current)) {
      return current;
    }
    current = current.parent;
  }
  return null;
}

function getFunctionName(node: ts.FunctionLikeDeclaration, checker: ts.TypeChecker): string {
  if (ts.isConstructorDeclaration(node)) {
    const cls = node.parent;
    if (ts.isClassDeclaration(cls) && cls.name) return `${cls.name.text}.constructor`;
    return "<constructor>";
  }
  const name = (node as any).name;
  if (!name) return "<anonymous>";
  const sym = checker.getSymbolAtLocation(name);
  return sym?.getName() ?? name.getText();
}

function visitNode(
  node: ts.Node, sf: ts.SourceFile, relFile: string,
  callerFunc: ts.FunctionLikeDeclaration | null,
  checker: ts.TypeChecker, edges: CallEdge[]
) {
  if (ts.isFunctionLike(node)) callerFunc = node;

  if (ts.isCallExpression(node) && callerFunc) {
    const sig = checker.getResolvedSignature(node);
    const decl = sig?.declaration;
    if (decl && !ts.isJSDocSignature(decl) && decl.getSourceFile()) {
      let symbol = checker.getSymbolAtLocation(node.expression);
      if (symbol?.flags! & ts.SymbolFlags.Alias) {
        symbol = checker.getAliasedSymbol(symbol!);
      }
      const calleeName = symbol?.getName() ?? "<anonymous>";
      const callerName = getFunctionName(callerFunc, checker);
      const calleeSf = decl.getSourceFile();
      const callPos = sf.getLineAndCharacterOfPosition(node.getStart());
      const callerStart = sf.getLineAndCharacterOfPosition(callerFunc.getStart()).line + 1;
      const callerEnd = sf.getLineAndCharacterOfPosition(callerFunc.getEnd()).line + 1;

      edges.push({
        caller: callerName,
        callee: calleeName,
        file: relFile,
        line: callPos.line + 1,
        caller_file: relFile,
        caller_start: callerStart,
        caller_end: callerEnd,
      });
    }
  }

  ts.forEachChild(node, child => visitNode(child, sf, relFile, callerFunc, checker, edges));
}

function collectChunks(sf: ts.SourceFile, relFile: string, checker: ts.TypeChecker, chunks: Chunk[], seen: Set<string>) {
  function visit(node: ts.Node) {
    let name = "", kind = "", startLine = 0, endLine = 0, signature = "";
    if (ts.isFunctionDeclaration(node) && node.name) {
      name = node.name.text; kind = "function";
    } else if (ts.isClassDeclaration(node) && node.name) {
      name = node.name.text; kind = "struct";
    } else if (ts.isInterfaceDeclaration(node)) {
      name = node.name.text; kind = "trait";
    } else if (ts.isEnumDeclaration(node)) {
      name = node.name.text; kind = "enum";
    } else if (ts.isMethodDeclaration(node) && node.name) {
      const cls = node.parent;
      const clsName = ts.isClassDeclaration(cls) && cls.name ? cls.name.text + "." : "";
      name = clsName + node.name.getText(); kind = "function";
    }
    if (name && kind) {
      startLine = sf.getLineAndCharacterOfPosition(node.getStart()).line + 1;
      endLine = sf.getLineAndCharacterOfPosition(node.getEnd()).line + 1;
      const key = `${relFile}:${name}:${startLine}`;
      if (!seen.has(key)) {
        seen.add(key);
        const sig = checker.getSignatureFromDeclaration(node as any);
        if (sig) signature = checker.signatureToString(sig);
        chunks.push({ name, file: relFile, kind, start_line: startLine, end_line: endLine, signature, crate_name: "" });
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(sf);
}

function findSourceFiles(dir: string): string[] {
  const result: string[] = [];
  function walk(d: string) {
    for (const entry of fs.readdirSync(d, { withFileTypes: true })) {
      if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
      const full = path.join(d, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (/\.(ts|tsx|js|jsx)$/.test(entry.name) && !entry.name.endsWith(".d.ts")) result.push(full);
    }
  }
  walk(dir);
  return result;
}

// CLI
const args = process.argv.slice(2);
let rootDir = ".";
let tsconfigPath: string | undefined;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--tsconfig" && args[i+1]) { tsconfigPath = args[++i]; }
  else { rootDir = args[i]; }
}

const { edges, chunks } = buildCallGraph(path.resolve(rootDir), tsconfigPath);
console.log(JSON.stringify({ edges, chunks }));
```

## package.json
```json
{
  "name": "ts-callgraph",
  "version": "0.1.0",
  "main": "index.js",
  "scripts": { "build": "tsc" },
  "dependencies": { "typescript": "^5.0.0" },
  "devDependencies": { "@types/node": "^20.0.0" }
}
```

## 정확도 한계
- `any` 타입 → resolve 불가
- 동적 프로퍼티 접근 (`obj[key]()`) → 불가
- `eval`, `Function()` → 불가
- interface/abstract 메서드 → 선언부만 가리킴 (구현체 불명)
- 85-95% 정확도 예상
