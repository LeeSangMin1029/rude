# MIR Capture Gaps (2026-03-29)

## 문제

일부 크레이트가 mir.db에 누락됨. rust-analyzer 인덱싱에서 `ide` 크레이트가 빠진 것으로 확인.

## 근본 원인

`--keep-going` 모드에서 RUSTC_WRAPPER가 `cargo check --tests`를 실행할 때:
- `ide.test.rustc-args.json`은 생성되지만 `ide.lib.rustc-args.json`은 생성 안 됨
- `ide` 크레이트가 **lib + bin 없이 integration test만** 있는 구조일 수 있음
- `run_mir_direct`에서 test_files도 포함하도록 수정했지만, `ide`의 test args가 direct 모드에서 처리 안 됨

## 영향

- `TryToNav` trait (ide 크레이트) — 미인덱싱
- `ide` 크레이트의 모든 함수 — mir.db에 없음
- 다른 프로젝트에서도 같은 패턴의 크레이트가 누락될 수 있음

## 해결 계획

### Phase 1: lib 없는 크레이트의 test args 처리 (run_mir_direct)

현재 `run_mir_direct`에서 `all_files = lib_files` 후 test_files를 추가하지만, **daemon 모드에서 test_files를 처리하지 않음**. daemon은 lib_files만 처리.

수정:
1. `try_daemon_all`에 test_files도 전달
2. daemon fallback (subprocess) 에서도 test_files 처리 확인
3. `detect_missing_edge_crates`에서 test args만 있는 크레이트도 missing으로 감지

파일: `crates/rude-intel/src/mir_edges/runner.rs`

### Phase 2: RUSTC_WRAPPER의 lib args 캡처 개선

`--keep-going` 모드에서 일부 크레이트의 lib 빌드가 캡처 안 되는 원인 조사:
- cargo가 `ide` lib을 빌드하지 않는 건지 (dependencies만 check)
- RUSTC_WRAPPER가 호출되지만 args를 저장 안 하는 건지

파일: `tools/mir-callgraph/src/wrapper.rs`

### Phase 3: 증분 업데이트 호환

- 이미 인덱싱된 프로젝트에 새 크레이트가 추가되면 `detect_missing`이 감지
- test args로 인덱싱된 크레이트는 나중에 lib args가 생기면 자동 교체
- 기존 mir.db 데이터를 덮어쓰지 않고 보강

### Phase 4: 검증

- rust-analyzer에서 65개 전체 크레이트 인덱싱 확인
- `TryToNav`, `to_nav` 심볼이 mir.db에 존재하는지
- 기존 rude 프로젝트의 인덱싱이 깨지지 않는지
- 증분 업데이트가 정상 동작하는지

## 제약

- `cargo expand` 불필요 — MIR에 proc_macro 확장 코드가 이미 포함
- nightly rustc 필수 (기존과 동일)
- 성능 영향 최소화 — 추가 cargo check 없이 기존 args 활용
