# mir-callgraph rustc_public 마이그레이션 + compat 어댑터 계층

## 배경
- stable-mir-json 프로젝트가 rustc_public(구 stable_mir) 기반으로 동작 중
- compat/ 어댑터 패턴으로 nightly 변경을 1곳에서 관리
- 현재 mir-callgraph는 rustc_private API를 47곳에서 직접 호출 → nightly 변경 시 전체 깨짐

## 조사 결과
- `rustc_public` rlib: 현재 nightly(2026-03-23)에 포함됨
- `rustc_public`의 mir 모듈: body.rs, alloc.rs, mono.rs, visit.rs
- `rustc_query_system`: 현재 nightly에서 extern crate 불가 (rmeta만 없음)
- crates.io 미게시 (2026-03 기준)

## 현재 빌드 에러
1. `extern crate rustc_query_system` 제거 필요
2. `extract_all`에서 `db_path` 변수 스코프 문제
3. format string 인자 불일치

## 작업 분해

### Task A: 빌드 에러 수정 + compat 모듈 생성
- `extern crate rustc_query_system` 제거
- `db_path` 변수를 `extract_all` 인자로 전달
- format string 수정
- `compat.rs` 모듈 생성 — 모든 `rustc_*` import를 한 곳에 집중
  - `pub use rustc_middle::...`
  - `pub use rustc_hir::...`
  - `pub use rustc_span::...`
  - 헬퍼 함수: `canonical_name`, `extract_filename`, `extract_visibility`

### Task B: rustc_public 마이그레이션 (가능한 부분)
- `rustc_public::mir::Body` — MIR body 접근
- `rustc_public::CrateItem` — crate 항목 순회
- compat.rs에서 rustc_private → rustc_public 전환점을 cfg로 관리
- 불가능한 부분은 rustc_private 유지 (tcx.optimized_mir 등)

### Task C: MIR hash delta + sqlite direct write 안정화
- 현재 빌드 에러 수정 후 delta 동작 확인
- pre_truncate를 delta 기반으로 교체 (변경 함수만 DELETE + INSERT)
- body text를 mir.db에서 제거 (메타데이터만 저장)

## 기대 효과
- nightly 변경 시 compat.rs만 수정
- 증분 1.3초 유지 + 안정성 확보
- mir.db 크기 203MB → ~30MB
