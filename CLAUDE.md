# rude — Code Intelligence Tool

코드 구조 분석 + 편집 도구. Rust(MIR), Go(go/ast), TypeScript(TSC API) 지원.
nightly, Go SDK, Node.js는 `rude add` 시 자동 설치/감지.

## 크레이트 구조

| 크레이트 | 위치 | 역할 |
|---------|------|------|
| `rude` | `crates/rude/` | CLI + 편집 커맨드 |
| `rude-intel` | `crates/rude-intel/` | 분석 엔진 (그래프, 파싱, MIR) |
| `rude-db` | `crates/rude-db/` | SQLite 저장 (kv_cache) |
| `rude-util` | `crates/rude-util/` | 경로, 해시, 포맷 유틸 |
| `mir-callgraph` | `tools/mir-callgraph/` | rustc_private MIR 추출 (nightly 전용, 별도 workspace) |
| `go-callgraph` | `tools/go-callgraph/` | Go AST 기반 call graph (Go 바이너리) |
| `ts-callgraph` | `tools/ts-callgraph/` | TSC API call graph (Node.js) |

## 빌드/테스트

```bash
cargo nextest r --status-level fail          # 전체 테스트 (PASS 출력 안 함)
cargo nextest r -E "test(이름)"              # 특정 테스트
cargo nextest r --no-fail-fast               # 실패해도 전부 실행
cargo build --release                        # 릴리즈 빌드 (~40초, 취소 금지)
```

mir-callgraph는 별도 빌드:
```bash
cd tools/mir-callgraph && RUSTUP_TOOLCHAIN=nightly cargo build --release
```

Go/TS extractor:
```bash
cd tools/go-callgraph && go build -o go-callgraph .
cd tools/ts-callgraph && npm install && npx tsc
```

## 금지 사항

- `unwrap()` 사용 금지 — `?` 전파 또는 `unwrap_or` 사용
- `panic!` 사용 금지 — `bail!` 또는 `anyhow::anyhow!` 사용
- `let _ =` 으로 Result 무시 금지 — 최소 `.ok()` 명시
- 요청하지 않은 기능 추가 금지
- `///` doc comments, `//!` 모듈 doc 작성 금지 (clap 도움말만 예외)
- `mod.rs` 파일은 이미 있는 구조만 사용, 새로 생성하지 않음
- mir-callgraph 관련 변경은 반드시 nightly 빌드 확인 후 진행
- mir-callgraph 수정은 에이전트에 위임하지 않음 (nightly/DLL 문제로 hang 위험)

## 코드 스타일

- 주석은 "왜(why)"만. 코드가 자명한 곳의 "무엇(what)" 주석 금지
- 함수 사이 빈줄 1줄만
- 불필요한 빈줄 금지
- `eprintln!` = 진행 메시지 (stderr), `println!` = 결과 데이터 (stdout)

## 다국어 extractor 규칙

- 외부 도구(go-callgraph, ts-callgraph) 출력: `{"edges":[], "chunks":[]}` JSON (stdout)
- 프로젝트 루트 기준 상대경로만 사용
- stdlib/vendor 코드 제외
- Pos()==0 또는 line==0 심볼 제외
- 심볼 이름: leaf name (패키지 경로 없이). 메서드: `Type.Method`

## 출력 구조

- 단일 함수: 기존 context (caller/callee/type/test)
- trait impl 여러 개: 자동 그룹핑 (shared callers + 메서드명 중복 제거)
- 최근 검색 기록 기반 우선순위 (kv_cache `recent_query_names`)
- `display_name`: 짧은 이름 표시 (`<Type as Trait>::method` → `Type::method`)
- struct: caller/callee 0이 정상. `--blast`로 영향 범위 확인

## DB 구조

단일 테이블 `kv_cache (key TEXT, value BLOB)` — chunks/graph/edges는 bincode blob.
FileIndex만 JSON. 전체 그래프를 메모리에 로드하는 구조.

## 편집 기능 주의

- `locate_symbol`은 Rust는 `syn`으로, Go/TS는 `text_fallback`으로 심볼 위치 탐색
- 연속 편집 시 역순 정렬 (뒤에서부터 splice) — 앞 인덱스 유지
- `expand_to_attrs`는 `.rs` 파일에만 적용

## MIR 자동 복구

nightly 미설치 → 자동 설치. mir-callgraph 없음 → 자동 빌드.
nightly 버전 변경 → mir-callgraph 자동 재빌드 + stale 캐시 삭제.
모든 실패 → clean + 재시도. 에러 시 조치 방법 출력.
