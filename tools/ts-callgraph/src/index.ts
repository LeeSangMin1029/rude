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
  kind: string;
  file: string;
  start: number;
  end: number;
  text: string;
}

interface CallGraphResult {
  edges: CallEdge[];
  chunks: Chunk[];
}

function collectSourceFiles(dir: string): string[] {
  const results: string[] = [];
  const exts = new Set([".ts", ".tsx", ".js", ".jsx"]);

  function walk(d: string) {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.name.startsWith(".")) continue;
      if (entry.name === "node_modules") continue;
      const full = path.join(d, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.isFile()) {
        const ext = path.extname(entry.name);
        if (exts.has(ext) && !entry.name.endsWith(".d.ts")) {
          results.push(full);
        }
      }
    }
  }

  walk(dir);
  return results;
}

function getSymbolQualifiedName(checker: ts.TypeChecker, symbol: ts.Symbol): string {
  const parts: string[] = [];
  let s: ts.Symbol | undefined = symbol;
  while (s) {
    const name = s.getName();
    if (name === "__type" || name === "default") {
      if (parts.length === 0) parts.unshift(name);
      break;
    }
    if (name.startsWith('"') || name === "__global") break;
    parts.unshift(name);
    const decls: ts.Declaration[] | undefined = s.getDeclarations();
    if (!decls || decls.length === 0) break;
    const parent: ts.Node = decls[0].parent;
    if (!parent) break;
    s = ts.isSourceFile(parent) ? undefined : checker.getSymbolAtLocation(parent);
    if (s && (s.flags & ts.SymbolFlags.Module) && s.getName().startsWith('"')) break;
  }
  return parts.join(".");
}

function resolveSymbol(checker: ts.TypeChecker, symbol: ts.Symbol): ts.Symbol {
  if (symbol.flags & ts.SymbolFlags.Alias) {
    return checker.getAliasedSymbol(symbol);
  }
  return symbol;
}

function getSymbolLocation(symbol: ts.Symbol): { file: string; start: number; end: number } | null {
  const decls = symbol.getDeclarations();
  if (!decls || decls.length === 0) return null;
  const decl = decls[0];
  const sf = decl.getSourceFile();
  if (sf.fileName.includes("node_modules")) return null;
  const startLine = sf.getLineAndCharacterOfPosition(decl.getStart()).line + 1;
  const endLine = sf.getLineAndCharacterOfPosition(decl.getEnd()).line + 1;
  return { file: sf.fileName, start: startLine, end: endLine };
}

function extractCallGraph(program: ts.Program): CallGraphResult {
  const checker = program.getTypeChecker();
  const edges: CallEdge[] = [];
  const chunks: Chunk[] = [];
  const seenChunks = new Set<string>();

  function addChunk(node: ts.Node, name: string, kind: string) {
    const sf = node.getSourceFile();
    if (sf.fileName.includes("node_modules")) return;
    const startLine = sf.getLineAndCharacterOfPosition(node.getStart()).line + 1;
    const endLine = sf.getLineAndCharacterOfPosition(node.getEnd()).line + 1;
    const key = `${sf.fileName}:${startLine}:${name}`;
    if (seenChunks.has(key)) return;
    seenChunks.add(key);
    chunks.push({
      name,
      kind,
      file: sf.fileName,
      start: startLine,
      end: endLine,
      text: node.getText(sf),
    });
  }

  function getEnclosingFunction(node: ts.Node): { name: string; file: string; start: number; end: number } | null {
    let current = node.parent;
    while (current) {
      if (ts.isFunctionDeclaration(current) && current.name) {
        const sf = current.getSourceFile();
        return {
          name: current.name.text,
          file: sf.fileName,
          start: sf.getLineAndCharacterOfPosition(current.getStart()).line + 1,
          end: sf.getLineAndCharacterOfPosition(current.getEnd()).line + 1,
        };
      }
      if (ts.isMethodDeclaration(current) && current.name) {
        const className = getEnclosingClassName(current);
        const methodName = current.name.getText();
        const qualName = className ? `${className}.${methodName}` : methodName;
        const sf = current.getSourceFile();
        return {
          name: qualName,
          file: sf.fileName,
          start: sf.getLineAndCharacterOfPosition(current.getStart()).line + 1,
          end: sf.getLineAndCharacterOfPosition(current.getEnd()).line + 1,
        };
      }
      if (ts.isArrowFunction(current) || ts.isFunctionExpression(current)) {
        const varDecl = current.parent;
        if (ts.isVariableDeclaration(varDecl) && varDecl.name) {
          const name = varDecl.name.getText();
          const sf = current.getSourceFile();
          return {
            name,
            file: sf.fileName,
            start: sf.getLineAndCharacterOfPosition(current.getStart()).line + 1,
            end: sf.getLineAndCharacterOfPosition(current.getEnd()).line + 1,
          };
        }
        if (ts.isPropertyDeclaration(varDecl) && varDecl.name) {
          const className = getEnclosingClassName(varDecl);
          const propName = varDecl.name.getText();
          const qualName = className ? `${className}.${propName}` : propName;
          const sf = current.getSourceFile();
          return {
            name: qualName,
            file: sf.fileName,
            start: sf.getLineAndCharacterOfPosition(current.getStart()).line + 1,
            end: sf.getLineAndCharacterOfPosition(current.getEnd()).line + 1,
          };
        }
      }
      if (ts.isConstructorDeclaration(current)) {
        const className = getEnclosingClassName(current);
        const qualName = className ? `${className}.constructor` : "constructor";
        const sf = current.getSourceFile();
        return {
          name: qualName,
          file: sf.fileName,
          start: sf.getLineAndCharacterOfPosition(current.getStart()).line + 1,
          end: sf.getLineAndCharacterOfPosition(current.getEnd()).line + 1,
        };
      }
      current = current.parent;
    }
    return null;
  }

  function getEnclosingClassName(node: ts.Node): string | null {
    let current = node.parent;
    while (current) {
      if (ts.isClassDeclaration(current) && current.name) {
        return current.name.text;
      }
      current = current.parent;
    }
    return null;
  }

  function visit(node: ts.Node) {
    const sf = node.getSourceFile();
    if (sf.fileName.includes("node_modules")) return;

    if (ts.isFunctionDeclaration(node) && node.name) {
      addChunk(node, node.name.text, "function");
    } else if (ts.isClassDeclaration(node) && node.name) {
      addChunk(node, node.name.text, "class");
    } else if (ts.isInterfaceDeclaration(node)) {
      addChunk(node, node.name.text, "interface");
    } else if (ts.isEnumDeclaration(node)) {
      addChunk(node, node.name.text, "enum");
    } else if (ts.isMethodDeclaration(node) && node.name) {
      const className = getEnclosingClassName(node);
      const name = className ? `${className}.${node.name.getText()}` : node.name.getText();
      addChunk(node, name, "method");
    } else if (ts.isVariableDeclaration(node) && node.name && node.initializer) {
      if (ts.isArrowFunction(node.initializer) || ts.isFunctionExpression(node.initializer)) {
        addChunk(node.parent.parent, node.name.getText(), "function");
      }
    }

    if (ts.isCallExpression(node) || ts.isNewExpression(node)) {
      const caller = getEnclosingFunction(node);
      if (!caller) {
        ts.forEachChild(node, visit);
        return;
      }

      let calleeName: string | null = null;
      let calleeLocation: { file: string; start: number; end: number } | null = null;

      const sig = checker.getResolvedSignature(node);
      if (sig) {
        const decl = sig.getDeclaration();
        if (decl) {
          const declSf = decl.getSourceFile();
          if (!declSf.fileName.includes("node_modules")) {
            const sym = checker.getSymbolAtLocation(
              ts.isCallExpression(node) ? node.expression : node.expression!
            );
            if (sym) {
              const resolved = resolveSymbol(checker, sym);
              calleeName = getSymbolQualifiedName(checker, resolved);
              calleeLocation = getSymbolLocation(resolved);
            }
          }
        }
      }

      if (!calleeName) {
        const expr = ts.isCallExpression(node) ? node.expression : node.expression;
        if (expr) {
          const sym = checker.getSymbolAtLocation(expr);
          if (sym) {
            const resolved = resolveSymbol(checker, sym);
            const loc = getSymbolLocation(resolved);
            if (loc) {
              calleeName = getSymbolQualifiedName(checker, resolved);
              calleeLocation = loc;
            }
          }
        }
      }

      if (calleeName && calleeLocation) {
        const callLine = sf.getLineAndCharacterOfPosition(node.getStart()).line + 1;
        edges.push({
          caller: caller.name,
          callee: calleeName,
          file: calleeLocation.file,
          line: callLine,
          caller_file: caller.file,
          caller_start: caller.start,
          caller_end: caller.end,
        });
      }
    }

    ts.forEachChild(node, visit);
  }

  for (const sf of program.getSourceFiles()) {
    if (sf.isDeclarationFile) continue;
    if (sf.fileName.includes("node_modules")) continue;
    visit(sf);
  }

  return { edges, chunks };
}

function normalizePaths(result: CallGraphResult, baseDir: string): CallGraphResult {
  const norm = (p: string) => path.relative(baseDir, p).replace(/\\/g, "/");
  return {
    edges: result.edges.map((e) => ({
      ...e,
      file: norm(e.file),
      caller_file: norm(e.caller_file),
    })),
    chunks: result.chunks.map((c) => ({
      ...c,
      file: norm(c.file),
    })),
  };
}

function main() {
  const args = process.argv.slice(2);
  let tsconfigPath: string | undefined;
  let targetDir: string | undefined;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--tsconfig" && i + 1 < args.length) {
      tsconfigPath = args[++i];
    } else if (!args[i].startsWith("-")) {
      targetDir = args[i];
    }
  }

  if (!targetDir) {
    process.stderr.write("Usage: node dist/index.js [--tsconfig path] <directory>\n");
    process.exit(1);
  }

  targetDir = path.resolve(targetDir);
  if (!fs.existsSync(targetDir)) {
    process.stderr.write(`Directory not found: ${targetDir}\n`);
    process.exit(1);
  }

  let compilerOptions: ts.CompilerOptions = {
    target: ts.ScriptTarget.ES2020,
    module: ts.ModuleKind.CommonJS,
    allowJs: true,
    checkJs: false,
    skipLibCheck: true,
    noEmit: true,
    strict: false,
    jsx: ts.JsxEmit.React,
    esModuleInterop: true,
    moduleResolution: ts.ModuleResolutionKind.Node10,
    resolveJsonModule: true,
  };

  if (tsconfigPath) {
    tsconfigPath = path.resolve(tsconfigPath);
    const configFile = ts.readConfigFile(tsconfigPath, ts.sys.readFile);
    if (configFile.error) {
      process.stderr.write(`Error reading tsconfig: ${ts.flattenDiagnosticMessageText(configFile.error.messageText, "\n")}\n`);
      process.exit(1);
    }
    const parsed = ts.parseJsonConfigFileContent(configFile.config, ts.sys, path.dirname(tsconfigPath));
    compilerOptions = { ...parsed.options, noEmit: true, skipLibCheck: true };
  } else {
    const autoTsconfig = path.join(targetDir, "tsconfig.json");
    if (fs.existsSync(autoTsconfig)) {
      const configFile = ts.readConfigFile(autoTsconfig, ts.sys.readFile);
      if (!configFile.error) {
        const parsed = ts.parseJsonConfigFileContent(configFile.config, ts.sys, targetDir);
        compilerOptions = { ...parsed.options, noEmit: true, skipLibCheck: true };
        process.stderr.write(`Using tsconfig: ${autoTsconfig}\n`);
      }
    }
  }

  process.stderr.write(`Collecting source files from ${targetDir}...\n`);
  const files = collectSourceFiles(targetDir);
  process.stderr.write(`Found ${files.length} source files\n`);

  if (files.length === 0) {
    const result: CallGraphResult = { edges: [], chunks: [] };
    process.stdout.write(JSON.stringify(result, null, 2));
    return;
  }

  process.stderr.write("Creating program...\n");
  const program = ts.createProgram(files, compilerOptions);

  process.stderr.write("Extracting call graph...\n");
  const result = extractCallGraph(program);
  const normalized = normalizePaths(result, targetDir);

  process.stderr.write(`Extracted ${normalized.edges.length} edges, ${normalized.chunks.length} chunks\n`);
  process.stdout.write(JSON.stringify(normalized, null, 2));
}

main();
