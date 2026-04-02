---
name: rude-multilang
description: "rude 다국어 call graph extractor 구현. Go VTA 또는 TypeScript Compiler API 기반 call graph 바이너리/스크립트를 생성하고, rude의 인덱싱 파이프라인에 통합한다. 'Go 지원', 'TypeScript 지원', '다국어', 'callgraph extractor' 요청 시 사용."
---

# rude 다국어 Call Graph Extractor

## 개요
rude는 Rust MIR 기반 100% 정확한 call graph를 제공한다. 이 스킬은 동일한 분석을 Go, TypeScript에 확장한다.
각 언어별 외부 바이너리/스크립트를 생성하고, rude의 기존 인덱싱 파이프라인에 통합한다.

## 아키텍처
```
rude add --lang go ./...
  └→ tools/go-callgraph (subprocess, JSON stdout)
       └→ rude가 파싱 → ParsedChunk + CallEdge 변환
            └→ .code.db 저장 → 기존 쿼리(context, trace, dead 등) 무수정 동작
```

## 공통 출력 스키마 (CallEdge JSON)
모든 extractor는 동일한 JSON 배열을 stdout에 출력한다:
```json
[{
  "caller": "fully::qualified::name",
  "callee": "fully::qualified::name",
  "file": "relative/path.ext",
  "line": 42,
  "caller_file": "relative/path.ext",
  "caller_start": 30,
  "caller_end": 55
}]
```

## Go Extractor 구현
상세: `references/go-extractor.md`

핵심:
1. `tools/go-callgraph/main.go` — VTA call graph 생성
2. `tools/go-callgraph/go.mod` — 의존성
3. rude-intel에 Go extractor runner 추가
4. `install-rude.sh`에 Go extractor 빌드 추가

## TSC Extractor 구현
상세: `references/tsc-extractor.md`

핵심:
1. `tools/ts-callgraph/index.ts` — TSC API call graph 생성
2. `tools/ts-callgraph/package.json` — 의존성
3. rude-intel에 TSC extractor runner 추가
4. `install-rude.sh`에 TSC extractor 설치 추가

## rude 통합 변경점

### 1. 언어 감지 (`pipeline.rs`)
- `Cargo.toml` 존재 → Rust (기존)
- `go.mod` 존재 → Go
- `tsconfig.json` 또는 `package.json` 존재 → TypeScript

### 2. Extractor Runner (`mir_edges/runner.rs`)
- `run_go_callgraph(project_dir) -> Result<Vec<CallEdge>>`
- `run_ts_callgraph(project_dir) -> Result<Vec<CallEdge>>`
- JSON stdout 파싱 → `CallEdge` 변환

### 3. Chunk 변환 (`ingest/`)
- `CallEdge` → `ParsedChunk` 변환 (기존 `mir.to_parsed()` 패턴 재사용)
- `chunk.language = "go"` / `"typescript"` 필드 추가

### 4. CLI (`cli.rs`)
- `rude add .` — 자동 감지 (Cargo.toml/go.mod/tsconfig.json)
- `rude add --lang go .` — 명시적 언어 지정

## 검증
- Go: bench-repos에서 Go 프로젝트 (gin, echo 등)로 `rude add` → `rude context` 테스트
- TS: bench-repos에서 TS 프로젝트 (zod, trpc 등)로 동일 테스트
- 기존 Rust 테스트가 깨지지 않는지 `cargo nextest r` 확인
