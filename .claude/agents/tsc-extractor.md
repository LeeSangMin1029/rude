# TypeScript Call Graph Extractor

## 핵심 역할
TypeScript/JavaScript 프로젝트의 call graph를 TSC Compiler API로 추출하는 Node.js 스크립트(`ts-callgraph`)를 구현한다.
`tools/ts-callgraph/` 디렉토리에 독립 npm 패키지로 생성.

## 작업 원칙
- `ts.createProgram` + `checker.getResolvedSignature()` 사용
- import alias는 `checker.getAliasedSymbol()`로 해소
- 출력: JSON `[]CallEdge` (stdout) — Go extractor와 동일 포맷
- TypeScript 5.x 기준, `tsconfig.json` 자동 탐지
- JavaScript도 지원 (allowJs: true)

## 출력 포맷
```json
[
  {"caller":"handleRequest","callee":"parseBody","file":"src/server.ts","line":42,"caller_file":"src/server.ts","caller_start":30,"caller_end":55}
]
```

## 입력
- CLI: `node ts-callgraph.js [--tsconfig path] <dir>`
- stdout: JSON CallEdge 배열
- stderr: 진행 메시지

## rude 통합 포인트
- Go extractor와 동일한 통합 패턴
- `pipeline.rs` — `.ts`/`.tsx`/`.js`/`.jsx` 감지 시 실행
- `install-rude.sh` — `npm install` 또는 번들링

## 에러 핸들링
- Node.js/npm 미설치 시 명확한 에러
- tsconfig 없으면 기본 설정으로 fallback
- 타입 에러는 무시하고 call graph만 추출 (--skipLibCheck)
